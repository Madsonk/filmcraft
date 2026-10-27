//! Matrix products: `MatMul`, `Einsum` (as a batched product) and a forward `GRU` layer.

use crate::ops::transpose;
use crate::par;
use crate::tensor::Tensor;
use crate::{Result, err};

/// `out[b, m, :] = a[b, m, :] · bm[b]` for row-major `a` `[B, M, K]` and `bm` `[B, K, N]`.
fn bmm(a: &[f32], bm: &[f32], batch: usize, m: usize, k: usize, n: usize) -> Result<Vec<f32>> {
    if a.len() != batch * m * k || bm.len() != batch * k * n {
        return err("matrix product: operand sizes do not match");
    }
    let mut out = vec![0f32; batch * m * n];
    if n == 0 {
        return Ok(out);
    }
    par::rows(&mut out, n, batch * m * k * n, |row, o| {
        let (bi, mi) = (row / m.max(1), row % m.max(1));
        let arow = a.get((bi * m + mi) * k..(bi * m + mi + 1) * k).unwrap_or(&[]);
        let bb = bm.get(bi * k * n..(bi + 1) * k * n).unwrap_or(&[]);
        for (kk, av) in arow.iter().enumerate() {
            if *av == 0.0 {
                continue;
            }
            if let Some(br) = bb.get(kk * n..(kk + 1) * n) {
                for (d, s) in o.iter_mut().zip(br) {
                    *d += av * s;
                }
            }
        }
    });
    Ok(out)
}

pub(crate) fn matmul(a: &Tensor, b: &Tensor) -> Result<Tensor> {
    let (av, bv) = (a.as_f32()?, b.as_f32()?);
    let ra = a.shape.len();
    let rb = b.shape.len();
    if ra < 2 || rb < 2 {
        return err("MatMul needs operands of rank 2 or more");
    }
    let (m, k) = (a.shape.get(ra - 2).copied().unwrap_or(0), a.shape.get(ra - 1).copied().unwrap_or(0));
    let (k2, n) = (b.shape.get(rb - 2).copied().unwrap_or(0), b.shape.get(rb - 1).copied().unwrap_or(0));
    if k != k2 {
        return err(format!("MatMul: inner sizes {k} and {k2} differ"));
    }
    let lead_a: usize = a.shape.get(..ra - 2).map_or(1, |s| s.iter().product());
    let mut shape: Vec<usize> = a.shape.get(..ra - 2).unwrap_or(&[]).to_vec();
    shape.push(m);
    shape.push(n);
    if rb == 2 {
        // one matrix for every batch of `a`
        let out = bmm(av, bv, 1, lead_a * m, k, n)?;
        return Tensor::f32(shape, out);
    }
    if a.shape.get(..ra - 2) != b.shape.get(..rb - 2) {
        return err("MatMul: batch dimensions differ");
    }
    Tensor::f32(shape, bmm(av, bv, lead_a, m, k, n)?)
}

/// Two-operand `Einsum` without ellipsis: labels are split into batch / left / contracted / right
/// and the product is a batched matrix product between two transposes.
pub(crate) fn einsum(eq: &str, a: &Tensor, b: &Tensor) -> Result<Tensor> {
    let eq: String = eq.chars().filter(|c| !c.is_whitespace()).collect();
    let (lhs, rhs) = eq.split_once("->").ok_or_else(|| crate::NnError("Einsum: the equation needs an explicit '->'".into()))?;
    let (la, lb) = lhs.split_once(',').ok_or_else(|| crate::NnError("Einsum: two operands expected".into()))?;
    let (la, lb): (Vec<char>, Vec<char>) = (la.chars().collect(), lb.chars().collect());
    let lo: Vec<char> = rhs.chars().collect();
    if la.len() != a.shape.len() || lb.len() != b.shape.len() {
        return err("Einsum: operand ranks do not match the equation");
    }
    let size = |l: char| -> Result<usize> {
        let sa = la.iter().position(|c| *c == l).and_then(|i| a.shape.get(i).copied());
        let sb = lb.iter().position(|c| *c == l).and_then(|i| b.shape.get(i).copied());
        match (sa, sb) {
            (Some(x), Some(y)) if x != y => err(format!("Einsum: label {l} has sizes {x} and {y}")),
            (Some(x), _) | (None, Some(x)) => Ok(x),
            _ => err("Einsum: unknown label"),
        }
    };
    let mut batch = Vec::new();
    let mut left = Vec::new();
    let mut contract = Vec::new();
    let mut right = Vec::new();
    for &l in &la {
        let (in_b, in_o) = (lb.contains(&l), lo.contains(&l));
        match (in_b, in_o) {
            (true, true) => batch.push(l),
            (true, false) => contract.push(l),
            (false, true) => left.push(l),
            (false, false) => return err("Einsum: a label summed inside one operand is not supported"),
        }
    }
    for &l in &lb {
        if !la.contains(&l) {
            if lo.contains(&l) {
                right.push(l);
            } else {
                return err("Einsum: a label summed inside one operand is not supported");
            }
        }
    }
    if lo.iter().any(|l| !la.contains(l) && !lb.contains(l)) || [&batch, &left, &right].iter().map(|v| v.len()).sum::<usize>() != lo.len() {
        return err("Einsum: the output labels do not match the operands");
    }
    let prod = |ls: &[char]| -> Result<usize> {
        let mut p = 1usize;
        for l in ls {
            p = p.checked_mul(size(*l)?).ok_or_else(|| crate::NnError("Einsum: size overflow".into()))?;
        }
        Ok(p)
    };
    let perm = |src: &[char], order: &[&[char]]| -> Vec<i64> {
        order.iter().flat_map(|g| g.iter()).filter_map(|l| src.iter().position(|c| c == l)).map(|i| i as i64).collect()
    };
    let at = transpose(a, Some(&perm(&la, &[&batch, &left, &contract])))?;
    let bt = transpose(b, Some(&perm(&lb, &[&batch, &contract, &right])))?;
    let (nb, nm, nk, nn) = (prod(&batch)?, prod(&left)?, prod(&contract)?, prod(&right)?);
    let out = bmm(at.as_f32()?, bt.as_f32()?, nb, nm, nk, nn)?;
    // [batch…, left…, right…] -> output label order
    let mut mid_labels: Vec<char> = batch.clone();
    mid_labels.extend(&left);
    mid_labels.extend(&right);
    let mid_shape: Vec<usize> = mid_labels.iter().map(|l| size(*l)).collect::<Result<_>>()?;
    let mid = Tensor::f32(mid_shape, out)?;
    let back: Vec<i64> = lo.iter().filter_map(|l| mid_labels.iter().position(|c| c == l)).map(|i| i as i64).collect();
    let res = transpose(&mid, Some(&back))?;
    Ok(res)
}

pub(crate) struct GruArgs {
    pub hidden: usize,
    pub linear_before_reset: bool,
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0f32; 8];
    let ((ca, ra), (cb, rb)) = (a.as_chunks::<8>(), b.as_chunks::<8>());
    for (x, y) in ca.iter().zip(cb) {
        for i in 0..8 {
            acc[i] += x[i] * y[i];
        }
    }
    let mut s: f32 = acc.iter().sum();
    for (x, y) in ra.iter().zip(rb) {
        s += x * y;
    }
    s
}

/// ONNX `GRU`, one forward direction. `x` is `[seq, batch, input]`; returns `Y` `[seq, 1, batch, H]`
/// and `Y_h` `[1, batch, H]`.
pub(crate) fn gru(x: &Tensor, w: &Tensor, r: &Tensor, bias: Option<&Tensor>, h0: Option<&Tensor>, a: &GruArgs) -> Result<(Tensor, Tensor)> {
    let hs = a.hidden;
    let [seq, batch, inp] = match x.shape.as_slice() {
        [s, b, i] => [*s, *b, *i],
        s => return err(format!("GRU: input must be [seq, batch, input], got {s:?}")),
    };
    if w.shape != [1, 3 * hs, inp] || r.shape != [1, 3 * hs, hs] {
        return err(format!(
            "GRU: weights {:?} / {:?} do not fit hidden size {hs} and input {inp} (only one forward direction is supported)",
            w.shape, r.shape
        ));
    }
    let (xv, wv, rv) = (x.as_f32()?, w.as_f32()?, r.as_f32()?);
    let zero_b = vec![0f32; 6 * hs];
    let bv: &[f32] = match bias {
        Some(b) => {
            if b.shape != [1, 6 * hs] {
                return err("GRU: bias must be [1, 6*hidden]");
            }
            b.as_f32()?
        }
        None => &zero_b,
    };
    let (wb, rb) = (bv.get(..3 * hs).unwrap_or(&[]), bv.get(3 * hs..).unwrap_or(&[]));
    // input contribution for every step at once: [seq*batch, 3H]
    let mut gx = vec![0f32; seq * batch * 3 * hs];
    par::rows(&mut gx, 3 * hs, seq * batch * 3 * hs * inp, |row, o| {
        let xr = xv.get(row * inp..(row + 1) * inp).unwrap_or(&[]);
        for (j, d) in o.iter_mut().enumerate() {
            *d = wb.get(j).copied().unwrap_or(0.0) + dot(wv.get(j * inp..(j + 1) * inp).unwrap_or(&[]), xr);
        }
    });
    let mut h = match h0 {
        Some(t) => {
            if t.shape != [1, batch, hs] {
                return err("GRU: initial_h must be [1, batch, hidden]");
            }
            t.as_f32()?.to_vec()
        }
        None => vec![0f32; batch * hs],
    };
    let mut y = vec![0f32; seq * batch * hs];
    let mut gh = vec![0f32; 3 * hs];
    let mut rh = vec![0f32; hs];
    for t in 0..seq {
        for bi in 0..batch {
            let hb = h.get(bi * hs..(bi + 1) * hs).unwrap_or(&[]).to_vec();
            let gxr = gx.get((t * batch + bi) * 3 * hs..(t * batch + bi + 1) * 3 * hs).unwrap_or(&[]);
            // z and r gates always read the previous state
            for j in 0..2 * hs {
                gh[j] = rb.get(j).copied().unwrap_or(0.0) + dot(rv.get(j * hs..(j + 1) * hs).unwrap_or(&[]), &hb);
            }
            let mut next = vec![0f32; hs];
            if a.linear_before_reset {
                for j in 2 * hs..3 * hs {
                    gh[j] = rb.get(j).copied().unwrap_or(0.0) + dot(rv.get(j * hs..(j + 1) * hs).unwrap_or(&[]), &hb);
                }
            } else {
                for j in 0..hs {
                    let rg = sigmoid(gxr.get(hs + j).copied().unwrap_or(0.0) + gh.get(hs + j).copied().unwrap_or(0.0));
                    rh[j] = rg * hb.get(j).copied().unwrap_or(0.0);
                }
                for j in 2 * hs..3 * hs {
                    gh[j] = rb.get(j).copied().unwrap_or(0.0) + dot(rv.get(j * hs..(j + 1) * hs).unwrap_or(&[]), &rh);
                }
            }
            for j in 0..hs {
                let z = sigmoid(gxr.get(j).copied().unwrap_or(0.0) + gh.get(j).copied().unwrap_or(0.0));
                let rg = sigmoid(gxr.get(hs + j).copied().unwrap_or(0.0) + gh.get(hs + j).copied().unwrap_or(0.0));
                let cand_h = gh.get(2 * hs + j).copied().unwrap_or(0.0);
                let n = if a.linear_before_reset {
                    (gxr.get(2 * hs + j).copied().unwrap_or(0.0) + rg * cand_h).tanh()
                } else {
                    (gxr.get(2 * hs + j).copied().unwrap_or(0.0) + cand_h).tanh()
                };
                if let Some(slot) = next.get_mut(j) {
                    *slot = (1.0 - z) * n + z * hb.get(j).copied().unwrap_or(0.0);
                }
            }
            if let Some(d) = h.get_mut(bi * hs..(bi + 1) * hs) {
                d.copy_from_slice(&next);
            }
            if let Some(d) = y.get_mut((t * batch + bi) * hs..(t * batch + bi + 1) * hs) {
                d.copy_from_slice(&next);
            }
        }
    }
    Ok((Tensor::f32(vec![seq, 1, batch, hs], y)?, Tensor::f32(vec![1, batch, hs], h)?))
}
