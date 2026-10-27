//! 2-D convolution, transposed convolution, max pooling and nearest resize (NCHW, `f32`).

use crate::par;
use crate::tensor::{Tensor, numel};
use crate::{Result, err};

pub(crate) struct ConvArgs {
    pub strides: [usize; 2],
    /// top, left, bottom, right
    pub pads: [usize; 4],
    pub dil: [usize; 2],
    pub group: usize,
}

fn dims4(t: &Tensor, what: &str) -> Result<[usize; 4]> {
    match t.shape.as_slice() {
        [a, b, c, d] => Ok([*a, *b, *c, *d]),
        s => err(format!("{what}: expected a 4-D tensor, got shape {s:?}")),
    }
}

fn out_len(input: usize, pad: usize, eff_k: usize, stride: usize) -> Result<usize> {
    let padded = input.checked_add(pad).ok_or_else(|| crate::NnError("convolution size overflow".into()))?;
    if stride == 0 || padded < eff_k {
        return err("convolution kernel larger than the padded input");
    }
    Ok((padded - eff_k) / stride + 1)
}

pub(crate) fn conv2d(x: &Tensor, w: &Tensor, bias: Option<&Tensor>, a: &ConvArgs) -> Result<Tensor> {
    let [n, c, h, wd] = dims4(x, "Conv input")?;
    let [m, cg, kh, kw] = dims4(w, "Conv weight")?;
    let g = a.group.max(1);
    if cg.checked_mul(g) != Some(c) || m % g != 0 {
        return err(format!("Conv: {c} input channels, weight {:?}, group {g}", w.shape));
    }
    let xs = x.as_f32()?;
    let ws = w.as_f32()?;
    let bs = match bias {
        Some(b) => {
            let v = b.as_f32()?;
            if v.len() != m {
                return err("Conv: bias length differs from the output channels");
            }
            Some(v)
        }
        None => None,
    };
    let [sh, sw] = a.strides;
    let [dh, dw] = a.dil;
    let [pt, pl, pb, pr] = a.pads;
    let eff_kh = dh.saturating_mul(kh.saturating_sub(1)) + 1;
    let eff_kw = dw.saturating_mul(kw.saturating_sub(1)) + 1;
    let oh = out_len(h, pt + pb, eff_kh, sh)?;
    let ow = out_len(wd, pl + pr, eff_kw, sw)?;
    let mg = m / g;
    let p = oh * ow;
    let shape = vec![n, m, oh, ow];
    let mut out = vec![0f32; numel(&shape)?];
    let k = cg * kh * kw;
    let hw = h * wd;
    let macs = n.saturating_mul(m).saturating_mul(p).saturating_mul(k);
    let plain = kh == 1 && kw == 1 && sh == 1 && sw == 1 && pt == 0 && pl == 0 && pb == 0 && pr == 0;

    if cg == 1 {
        // depthwise (or a channel multiplier): shifted multiply-accumulate on the rows
        par::rows(&mut out, p, macs, |idx, o| {
            let (ni, mi) = (idx / m, idx % m);
            let ch = (mi / mg) * cg;
            let xc = xs.get((ni * c + ch) * hw..(ni * c + ch + 1) * hw).unwrap_or(&[]);
            let wk = ws.get(mi * k..(mi + 1) * k).unwrap_or(&[]);
            if let Some(b) = bs.and_then(|b| b.get(mi)) {
                o.fill(*b);
            }
            for ky in 0..kh {
                for kx in 0..kw {
                    let wv = wk.get(ky * kw + kx).copied().unwrap_or(0.0);
                    if wv == 0.0 {
                        continue;
                    }
                    let off = kx * dw;
                    // first/last output column whose input column is inside the image
                    let ox0 = if off >= pl { 0 } else { (pl - off).div_ceil(sw) };
                    let ox1 = if wd + pl <= off { 0 } else { ((wd - 1 + pl - off) / sw + 1).min(ow) };
                    if ox0 >= ox1 {
                        continue;
                    }
                    for oy in 0..oh {
                        let iy = (oy * sh + ky * dh) as isize - pt as isize;
                        if iy < 0 || iy as usize >= h {
                            continue;
                        }
                        let irow = xc.get(iy as usize * wd..(iy as usize + 1) * wd).unwrap_or(&[]);
                        let Some(orow) = o.get_mut(oy * ow + ox0..oy * ow + ox1) else { continue };
                        if sw == 1 {
                            let start = ox0 + off - pl;
                            if let Some(src) = irow.get(start..start + (ox1 - ox0)) {
                                for (d, s) in orow.iter_mut().zip(src) {
                                    *d += wv * s;
                                }
                            }
                        } else {
                            for (j, d) in orow.iter_mut().enumerate() {
                                let ix = (ox0 + j) * sw + off - pl;
                                *d += wv * irow.get(ix).copied().unwrap_or(0.0);
                            }
                        }
                    }
                }
            }
        });
        return Tensor::f32(shape, out);
    }

    // general: im2col per (batch, group) then a row-parallel product
    let mut col: Vec<f32> = Vec::new();
    for ni in 0..n {
        for gi in 0..g {
            let xin = xs.get((ni * c + gi * cg) * hw..(ni * c + (gi + 1) * cg) * hw).unwrap_or(&[]);
            let cols: &[f32] = if plain {
                xin
            } else {
                col.clear();
                col.resize(numel(&[k, p])?, 0.0);
                for ci in 0..cg {
                    for ky in 0..kh {
                        for kx in 0..kw {
                            let r = (ci * kh + ky) * kw + kx;
                            let off = kx * dw;
                            let ox0 = if off >= pl { 0 } else { (pl - off).div_ceil(sw) };
                            let ox1 = if wd + pl <= off { 0 } else { ((wd - 1 + pl - off) / sw + 1).min(ow) };
                            for oy in 0..oh {
                                let iy = (oy * sh + ky * dh) as isize - pt as isize;
                                if iy < 0 || iy as usize >= h || ox0 >= ox1 {
                                    continue;
                                }
                                let irow = xin.get((ci * h + iy as usize) * wd..(ci * h + iy as usize + 1) * wd).unwrap_or(&[]);
                                let base = r * p + oy * ow;
                                if let Some(dst) = col.get_mut(base + ox0..base + ox1) {
                                    for (j, d) in dst.iter_mut().enumerate() {
                                        *d = irow.get((ox0 + j) * sw + off - pl).copied().unwrap_or(0.0);
                                    }
                                }
                            }
                        }
                    }
                }
                &col
            };
            let wg = ws.get(gi * mg * k..(gi + 1) * mg * k).unwrap_or(&[]);
            let o_start = (ni * m + gi * mg) * p;
            let Some(oblock) = out.get_mut(o_start..o_start + mg * p) else { return err("Conv: internal size error") };
            par::rows(oblock, p, mg * p * k, |mi, orow| {
                if let Some(b) = bs.and_then(|b| b.get(gi * mg + mi)) {
                    orow.fill(*b);
                }
                let wrow = wg.get(mi * k..(mi + 1) * k).unwrap_or(&[]);
                const CH: usize = 1024;
                let mut p0 = 0;
                while p0 < p {
                    let p1 = (p0 + CH).min(p);
                    let Some(oc) = orow.get_mut(p0..p1) else { break };
                    let mut kk = 0;
                    while kk + 4 <= k {
                        let (w0, w1, w2, w3) = (wrow[kk], wrow[kk + 1], wrow[kk + 2], wrow[kk + 3]);
                        let c0 = cols.get(kk * p + p0..kk * p + p1).unwrap_or(&[]);
                        let c1 = cols.get((kk + 1) * p + p0..(kk + 1) * p + p1).unwrap_or(&[]);
                        let c2 = cols.get((kk + 2) * p + p0..(kk + 2) * p + p1).unwrap_or(&[]);
                        let c3 = cols.get((kk + 3) * p + p0..(kk + 3) * p + p1).unwrap_or(&[]);
                        if c0.len() == oc.len() && c1.len() == oc.len() && c2.len() == oc.len() && c3.len() == oc.len() {
                            for i in 0..oc.len() {
                                oc[i] += w0 * c0[i] + w1 * c1[i] + w2 * c2[i] + w3 * c3[i];
                            }
                        }
                        kk += 4;
                    }
                    while kk < k {
                        let wv = wrow[kk];
                        if let Some(cr) = cols.get(kk * p + p0..kk * p + p1) {
                            for (d, s) in oc.iter_mut().zip(cr) {
                                *d += wv * s;
                            }
                        }
                        kk += 1;
                    }
                    p0 = p1;
                }
            });
        }
    }
    Tensor::f32(shape, out)
}

pub(crate) struct ConvTArgs {
    pub strides: [usize; 2],
    pub pads: [usize; 4],
    pub dil: [usize; 2],
    pub out_pad: [usize; 2],
    pub group: usize,
}

pub(crate) fn conv_transpose2d(x: &Tensor, w: &Tensor, bias: Option<&Tensor>, a: &ConvTArgs) -> Result<Tensor> {
    let [n, c, h, wd] = dims4(x, "ConvTranspose input")?;
    let [wc, mg, kh, kw] = dims4(w, "ConvTranspose weight")?;
    let g = a.group.max(1);
    if wc != c || c % g != 0 {
        return err(format!("ConvTranspose: {c} input channels, weight {:?}, group {g}", w.shape));
    }
    let cg = c / g;
    let m = mg * g;
    let [sh, sw] = a.strides;
    let [dh, dw] = a.dil;
    let [pt, pl, pb, pr] = a.pads;
    let full_h = (h.saturating_sub(1) * sh + dh * kh.saturating_sub(1) + a.out_pad[0] + 1) as isize - (pt + pb) as isize;
    let full_w = (wd.saturating_sub(1) * sw + dw * kw.saturating_sub(1) + a.out_pad[1] + 1) as isize - (pl + pr) as isize;
    if full_h <= 0 || full_w <= 0 {
        return err("ConvTranspose: empty output");
    }
    let (oh, ow) = (full_h as usize, full_w as usize);
    let shape = vec![n, m, oh, ow];
    let mut out = vec![0f32; numel(&shape)?];
    let xs = x.as_f32()?;
    let ws = w.as_f32()?;
    for ni in 0..n {
        for gi in 0..g {
            for ic in 0..cg {
                let ci = gi * cg + ic;
                for oc in 0..mg {
                    let mo = gi * mg + oc;
                    for ky in 0..kh {
                        for kx in 0..kw {
                            let wv = ws.get(((ci * mg + oc) * kh + ky) * kw + kx).copied().unwrap_or(0.0);
                            for y in 0..h {
                                let oy = (y * sh + ky * dh) as isize - pt as isize;
                                if oy < 0 || oy as usize >= oh {
                                    continue;
                                }
                                for xx in 0..wd {
                                    let ox = (xx * sw + kx * dw) as isize - pl as isize;
                                    if ox < 0 || ox as usize >= ow {
                                        continue;
                                    }
                                    let xv = xs.get(((ni * c + ci) * h + y) * wd + xx).copied().unwrap_or(0.0);
                                    if let Some(d) = out.get_mut(((ni * m + mo) * oh + oy as usize) * ow + ox as usize) {
                                        *d += xv * wv;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if let Some(b) = bias {
        let bv = b.as_f32()?;
        if bv.len() != m {
            return err("ConvTranspose: bias length differs from the output channels");
        }
        for (i, plane) in out.chunks_mut(oh * ow).enumerate() {
            let bb = bv.get(i % m).copied().unwrap_or(0.0);
            for v in plane {
                *v += bb;
            }
        }
    }
    Tensor::f32(shape, out)
}

pub(crate) fn max_pool2d(x: &Tensor, kernel: [usize; 2], strides: [usize; 2], pads: [usize; 4]) -> Result<Tensor> {
    let [n, c, h, wd] = dims4(x, "MaxPool input")?;
    let [kh, kw] = kernel;
    let [sh, sw] = strides;
    let [pt, pl, pb, pr] = pads;
    let oh = out_len(h, pt + pb, kh, sh)?;
    let ow = out_len(wd, pl + pr, kw, sw)?;
    let shape = vec![n, c, oh, ow];
    let mut out = vec![0f32; numel(&shape)?];
    let xs = x.as_f32()?;
    par::rows(&mut out, oh * ow, n * c * oh * ow * kh * kw, |idx, o| {
        let plane = xs.get(idx * h * wd..(idx + 1) * h * wd).unwrap_or(&[]);
        for oy in 0..oh {
            for ox in 0..ow {
                let mut best = f32::NEG_INFINITY;
                for ky in 0..kh {
                    let iy = (oy * sh + ky) as isize - pt as isize;
                    if iy < 0 || iy as usize >= h {
                        continue;
                    }
                    for kx in 0..kw {
                        let ix = (ox * sw + kx) as isize - pl as isize;
                        if ix < 0 || ix as usize >= wd {
                            continue;
                        }
                        if let Some(v) = plane.get(iy as usize * wd + ix as usize) {
                            best = best.max(*v);
                        }
                    }
                }
                if let Some(d) = o.get_mut(oy * ow + ox) {
                    *d = best;
                }
            }
        }
    });
    Tensor::f32(shape, out)
}

/// `nearest` resize with `asymmetric` coordinates and `floor` rounding (what exporters emit for
/// 2x feature-map upsampling). `scales` are per dimension; the batch and channel scales must be 1.
pub(crate) fn resize_nearest(x: &Tensor, out_hw: [usize; 2]) -> Result<Tensor> {
    let [n, c, h, wd] = dims4(x, "Resize input")?;
    let [oh, ow] = out_hw;
    if h == 0 || wd == 0 {
        return err("Resize: empty input");
    }
    let shape = vec![n, c, oh, ow];
    let mut out = vec![0f32; numel(&shape)?];
    let xs = x.as_f32()?;
    for (idx, o) in out.chunks_mut((oh * ow).max(1)).enumerate() {
        let plane = xs.get(idx * h * wd..(idx + 1) * h * wd).unwrap_or(&[]);
        for oy in 0..oh {
            let iy = (oy * h / oh.max(1)).min(h - 1);
            for ox in 0..ow {
                let ix = (ox * wd / ow.max(1)).min(wd - 1);
                if let Some(d) = o.get_mut(oy * ow + ox) {
                    *d = plane.get(iy * wd + ix).copied().unwrap_or(0.0);
                }
            }
        }
    }
    Tensor::f32(shape, out)
}
