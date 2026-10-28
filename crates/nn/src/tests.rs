//! Tests of the interpreter: each operator against a plain reference written here, a small model
//! built byte by byte, and hostile input.

use crate::conv::{ConvArgs, ConvTArgs, conv_transpose2d, conv2d, max_pool2d, resize_nearest};
use crate::ops;
use crate::rnn::{GruArgs, einsum, gru, matmul};
use crate::{Model, Tensor};

/// Deterministic pseudo-random values in -1 … 1.
fn vals(n: usize, seed: u32) -> Vec<f32> {
    let mut s = seed.wrapping_mul(2_654_435_761).wrapping_add(12_345);
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((s >> 8) as f32 / (1u32 << 24) as f32) * 2.0 - 1.0
        })
        .collect()
}

fn t(shape: &[usize], v: Vec<f32>) -> Tensor {
    Tensor::f32(shape.to_vec(), v).expect("tensor")
}

#[allow(clippy::too_many_arguments)]
fn naive_conv(
    x: &[f32],
    xs: [usize; 4],
    w: &[f32],
    ws: [usize; 4],
    b: &[f32],
    st: [usize; 2],
    pads: [usize; 4],
    dil: [usize; 2],
    g: usize,
) -> (Vec<f32>, [usize; 4]) {
    let [n, c, h, wd] = xs;
    let [m, cg, kh, kw] = ws;
    let oh = (h + pads[0] + pads[2] - dil[0] * (kh - 1) - 1) / st[0] + 1;
    let ow = (wd + pads[1] + pads[3] - dil[1] * (kw - 1) - 1) / st[1] + 1;
    let mut out = vec![0f32; n * m * oh * ow];
    for ni in 0..n {
        for mi in 0..m {
            let gi = mi / (m / g);
            for oy in 0..oh {
                for ox in 0..ow {
                    let mut acc = f64::from(b.get(mi).copied().unwrap_or(0.0));
                    for ci in 0..cg {
                        for ky in 0..kh {
                            for kx in 0..kw {
                                let iy = (oy * st[0] + ky * dil[0]) as isize - pads[0] as isize;
                                let ix = (ox * st[1] + kx * dil[1]) as isize - pads[1] as isize;
                                if iy < 0 || ix < 0 || iy as usize >= h || ix as usize >= wd {
                                    continue;
                                }
                                let xi = ((ni * c + gi * cg + ci) * h + iy as usize) * wd + ix as usize;
                                let wi = ((mi * cg + ci) * kh + ky) * kw + kx;
                                acc += f64::from(x[xi]) * f64::from(w[wi]);
                            }
                        }
                    }
                    out[((ni * m + mi) * oh + oy) * ow + ox] = acc as f32;
                }
            }
        }
    }
    (out, [n, m, oh, ow])
}

fn close(a: &[f32], b: &[f32], tol: f32) {
    assert_eq!(a.len(), b.len());
    for (i, (p, q)) in a.iter().zip(b).enumerate() {
        assert!((p - q).abs() <= tol * (1.0 + q.abs()), "element {i}: {p} vs {q}");
    }
}

#[test]
fn convolution_matches_the_naive_loop_in_every_layout() {
    // (input shape, weight shape, strides, pads t l b r, dilations, groups)
    let cases: &[([usize; 4], [usize; 4], [usize; 2], [usize; 4], [usize; 2], usize)] = &[
        ([1, 3, 9, 11], [4, 3, 3, 3], [1, 1], [1, 1, 1, 1], [1, 1], 1),
        ([2, 4, 8, 8], [6, 2, 3, 3], [2, 2], [1, 1, 1, 1], [1, 1], 2),
        ([1, 6, 7, 10], [6, 1, 1, 3], [1, 2], [0, 1, 0, 1], [1, 1], 6),
        ([1, 5, 12, 9], [5, 1, 3, 3], [2, 1], [2, 1, 0, 2], [1, 1], 5),
        ([1, 2, 10, 10], [3, 2, 3, 3], [1, 1], [2, 2, 2, 2], [2, 2], 1),
        ([1, 8, 6, 6], [4, 8, 1, 1], [1, 1], [0, 0, 0, 0], [1, 1], 1),
        ([1, 4, 6, 5], [8, 1, 3, 3], [1, 1], [1, 1, 1, 1], [1, 1], 4),
    ];
    for (k, (xs, ws, st, pads, dil, g)) in cases.iter().enumerate() {
        let x = vals(xs.iter().product(), k as u32 + 1);
        let w = vals(ws.iter().product(), k as u32 + 100);
        let b = vals(ws[0], k as u32 + 200);
        let (want, os) = naive_conv(&x, *xs, &w, *ws, &b, *st, *pads, *dil, *g);
        let args = ConvArgs { strides: *st, pads: *pads, dil: *dil, group: *g };
        let got = conv2d(&t(xs, x), &t(ws, w), Some(&t(&[ws[0]], b)), &args).expect("conv");
        assert_eq!(got.shape, os.to_vec(), "case {k}");
        close(got.as_f32().expect("f32"), &want, 1e-4);
    }
}

#[test]
fn transposed_convolution_inverts_the_strided_shape() {
    // 1 channel, kernel (1,3), stride (1,2), pad (0,1), output padding (0,1): width 4 -> 8
    let x = t(&[1, 1, 1, 4], vec![1.0, 2.0, 3.0, 4.0]);
    let w = t(&[1, 1, 1, 3], vec![1.0, 10.0, 100.0]);
    let a = ConvTArgs { strides: [1, 2], pads: [0, 1, 0, 1], dil: [1, 1], out_pad: [0, 1], group: 1 };
    let y = conv_transpose2d(&x, &w, None, &a).expect("convT");
    assert_eq!(y.shape, vec![1, 1, 1, 8]);
    // full output has 4*2+... samples; with pad 1 left the first is x0*w1 shifted: [10,100+?...]
    // reference by scattering
    let mut full = [0f32; 12];
    for (i, xv) in [1.0f32, 2.0, 3.0, 4.0].iter().enumerate() {
        for (k, wv) in [1.0f32, 10.0, 100.0].iter().enumerate() {
            full[i * 2 + k] += xv * wv;
        }
    }
    close(y.as_f32().expect("f32"), &full[1..9], 1e-6);
}

#[test]
fn pooling_and_resize() {
    let x = t(&[1, 1, 4, 4], (0..16).map(|v| v as f32).collect());
    let y = max_pool2d(&x, [2, 2], [2, 2], [0; 4]).expect("pool");
    assert_eq!(y.as_f32().expect("f32"), &[5.0, 7.0, 13.0, 15.0]);
    // 5x5 window, stride 1, pad 2 keeps the size; the values grow right and down, so the maximum
    // of each window is its bottom-right cell inside the picture
    let z = max_pool2d(&x, [5, 5], [1, 1], [2; 4]).expect("pool");
    assert_eq!(z.shape, vec![1, 1, 4, 4]);
    let want: Vec<f32> = (0..16).map(|i| (((i / 4) + 2).min(3) * 4 + ((i % 4) + 2).min(3)) as f32).collect();
    assert_eq!(z.as_f32().expect("f32"), want.as_slice());
    let r = resize_nearest(&t(&[1, 1, 2, 2], vec![1.0, 2.0, 3.0, 4.0]), [4, 4]).expect("resize");
    assert_eq!(r.as_f32().expect("f32"), &[1.0, 1.0, 2.0, 2.0, 1.0, 1.0, 2.0, 2.0, 3.0, 3.0, 4.0, 4.0, 3.0, 3.0, 4.0, 4.0]);
}

fn sig(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

#[test]
fn gru_matches_the_equations_for_both_reset_modes() {
    let (seq, batch, inp, hs) = (6usize, 2usize, 5usize, 4usize);
    let x = vals(seq * batch * inp, 1);
    let w = vals(3 * hs * inp, 2);
    let r = vals(3 * hs * hs, 3);
    let b = vals(6 * hs, 4);
    for lbr in [false, true] {
        let got = gru(
            &t(&[seq, batch, inp], x.clone()),
            &t(&[1, 3 * hs, inp], w.clone()),
            &t(&[1, 3 * hs, hs], r.clone()),
            Some(&t(&[1, 6 * hs], b.clone())),
            None,
            &GruArgs { hidden: hs, linear_before_reset: lbr },
        )
        .expect("gru");
        let mut h = vec![vec![0f64; hs]; batch];
        let mut want = Vec::new();
        for ti in 0..seq {
            for bi in 0..batch {
                let xt: Vec<f64> = (0..inp).map(|i| f64::from(x[(ti * batch + bi) * inp + i])).collect();
                let wx = |g: usize, j: usize| -> f64 { (0..inp).map(|i| f64::from(w[(g * hs + j) * inp + i]) * xt[i]).sum::<f64>() + f64::from(b[g * hs + j]) };
                let rh = |g: usize, j: usize, hv: &[f64]| -> f64 {
                    (0..hs).map(|i| f64::from(r[(g * hs + j) * hs + i]) * hv[i]).sum::<f64>() + f64::from(b[3 * hs + g * hs + j])
                };
                let hp = h[bi].clone();
                let z: Vec<f64> = (0..hs).map(|j| sig(wx(0, j) + rh(0, j, &hp))).collect();
                let rg: Vec<f64> = (0..hs).map(|j| sig(wx(1, j) + rh(1, j, &hp))).collect();
                let rhp: Vec<f64> = (0..hs).map(|j| rg[j] * hp[j]).collect();
                let n: Vec<f64> = (0..hs).map(|j| if lbr { (wx(2, j) + rg[j] * rh(2, j, &hp)).tanh() } else { (wx(2, j) + rh(2, j, &rhp)).tanh() }).collect();
                h[bi] = (0..hs).map(|j| (1.0 - z[j]) * n[j] + z[j] * hp[j]).collect();
                want.extend(h[bi].iter().map(|v| *v as f32));
            }
        }
        assert_eq!(got.0.shape, vec![seq, 1, batch, hs]);
        close(got.0.as_f32().expect("f32"), &want, 1e-4);
    }
}

#[test]
fn einsum_grouped_linear_and_matmul() {
    let (b, tt, g, i, h) = (1usize, 3usize, 4usize, 5usize, 2usize);
    let a = vals(b * tt * g * i, 7);
    let w = vals(g * i * h, 8);
    let y = einsum("btgi,gih->btgh", &t(&[b, tt, g, i], a.clone()), &t(&[g, i, h], w.clone())).expect("einsum");
    assert_eq!(y.shape, vec![b, tt, g, h]);
    let mut want = vec![0f32; b * tt * g * h];
    for ti in 0..tt {
        for gi in 0..g {
            for hi in 0..h {
                want[(ti * g + gi) * h + hi] = (0..i).map(|ii| a[(ti * g + gi) * i + ii] * w[(gi * i + ii) * h + hi]).sum();
            }
        }
    }
    close(y.as_f32().expect("f32"), &want, 1e-5);
    let m = matmul(&t(&[2, 3], vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]), &t(&[3, 2], vec![1.0, 0.0, 0.0, 1.0, 1.0, 1.0])).expect("matmul");
    assert_eq!(m.as_f32().expect("f32"), &[4.0, 5.0, 10.0, 11.0]);
    assert!(einsum("ab,bc", &t(&[1, 1], vec![1.0]), &t(&[1, 1], vec![1.0])).is_err());
}

fn ints(shape: &[usize], v: &[i64]) -> Tensor {
    Tensor::i64(shape.to_vec(), v.to_vec()).expect("tensor")
}

#[test]
fn shape_operators() {
    // the exporter's "reverse the pad list" trick: slice with step -1 from the end
    let v = ints(&[4], &[0, 0, 2, 0]);
    let rev = ops::slice(&v, &[-1], &[-9_223_372_036_854_775_807], Some(&[0]), Some(&[-1])).expect("slice");
    assert_eq!(rev.as_i64().expect("i64"), &[0, 2, 0, 0]);
    let m = t(&[2, 3], vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
    let part = ops::slice(&m, &[1], &[3], Some(&[1]), None).expect("slice");
    assert_eq!(part.as_f32().expect("f32"), &[2.0, 3.0, 5.0, 6.0]);
    let tr = ops::transpose(&m, None).expect("transpose");
    assert_eq!((tr.shape.clone(), tr.as_f32().expect("f32").to_vec()), (vec![3, 2], vec![1.0, 4.0, 2.0, 5.0, 3.0, 6.0]));
    let r = ops::reshape(&m, &ints(&[2], &[-1, 2])).expect("reshape");
    assert_eq!(r.shape, vec![3, 2]);
    assert!(ops::reshape(&m, &ints(&[2], &[4, -1])).is_err());
    let c = ops::concat(&[&m, &m], 0).expect("concat");
    assert_eq!(c.shape, vec![4, 3]);
    let c1 = ops::concat(&[&m, &m], 1).expect("concat");
    assert_eq!(c1.as_f32().expect("f32")[..6], [1.0, 2.0, 3.0, 1.0, 2.0, 3.0]);
    let gth = ops::gather(&ints(&[3], &[7, 8, 9]), &Tensor::i64(vec![], vec![-1]).expect("scalar"), 0).expect("gather");
    assert_eq!((gth.shape.clone(), gth.as_i64().expect("i64").to_vec()), (vec![], vec![9]));
    assert!(ops::gather(&ints(&[3], &[7, 8, 9]), &ints(&[1], &[3]), 0).is_err());
    // pad in front and crop at the end
    let p = ops::pad(&m, &[0, 1, 0, -1], 0.0).expect("pad");
    assert_eq!(p.shape, vec![2, 3]);
    assert_eq!(p.as_f32().expect("f32"), &[0.0, 1.0, 2.0, 0.0, 4.0, 5.0]);
    let sq = ops::squeeze(&t(&[1, 2, 1], vec![1.0, 2.0]), Some(vec![0])).expect("squeeze");
    assert_eq!(sq.shape, vec![2, 1]);
    assert_eq!(ops::unsqueeze(&sq, &[0]).expect("unsqueeze").shape, vec![1, 2, 1]);
    // broadcasting
    let a = t(&[2, 1], vec![1.0, 2.0]);
    let b = t(&[1, 3], vec![10.0, 20.0, 30.0]);
    let s = ops::binary(ops::Bin::Add, &a, &b).expect("add");
    assert_eq!(s.as_f32().expect("f32"), &[11.0, 21.0, 31.0, 12.0, 22.0, 32.0]);
    assert!(ops::binary(ops::Bin::Add, &t(&[2], vec![1.0, 2.0]), &t(&[3], vec![1.0, 2.0, 3.0])).is_err());
}

// --- a model built byte by byte --------------------------------------------------------------

fn varint(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn field_len(f: u32, data: &[u8], out: &mut Vec<u8>) {
    varint(u64::from(f) << 3 | 2, out);
    varint(data.len() as u64, out);
    out.extend_from_slice(data);
}

fn field_int(f: u32, v: i64, out: &mut Vec<u8>) {
    varint(u64::from(f) << 3, out);
    varint(v as u64, out);
}

fn node(op: &str, ins: &[&str], outs: &[&str], attrs: &[Vec<u8>]) -> Vec<u8> {
    let mut n = Vec::new();
    for i in ins {
        field_len(1, i.as_bytes(), &mut n);
    }
    for o in outs {
        field_len(2, o.as_bytes(), &mut n);
    }
    field_len(4, op.as_bytes(), &mut n);
    for a in attrs {
        field_len(5, a, &mut n);
    }
    n
}

fn attr_ints(name: &str, v: &[i64]) -> Vec<u8> {
    let mut a = Vec::new();
    field_len(1, name.as_bytes(), &mut a);
    let mut packed = Vec::new();
    for x in v {
        varint(*x as u64, &mut packed);
    }
    field_len(8, &packed, &mut a);
    a
}

fn attr_int(name: &str, v: i64) -> Vec<u8> {
    let mut a = Vec::new();
    field_len(1, name.as_bytes(), &mut a);
    field_int(3, v, &mut a);
    a
}

fn init_f32(name: &str, dims: &[i64], v: &[f32]) -> Vec<u8> {
    let mut t = Vec::new();
    let mut d = Vec::new();
    for x in dims {
        varint(*x as u64, &mut d);
    }
    field_len(1, &d, &mut t);
    field_int(2, 1, &mut t);
    field_len(8, name.as_bytes(), &mut t);
    field_len(9, &v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>(), &mut t);
    t
}

fn value_info(name: &str) -> Vec<u8> {
    let mut v = Vec::new();
    field_len(1, name.as_bytes(), &mut v);
    v
}

fn model(nodes: Vec<Vec<u8>>, inits: Vec<Vec<u8>>, inputs: &[&str], outputs: &[&str]) -> Vec<u8> {
    let mut g = Vec::new();
    for n in nodes {
        field_len(1, &n, &mut g);
    }
    for i in inits {
        field_len(5, &i, &mut g);
    }
    for i in inputs {
        field_len(11, &value_info(i), &mut g);
    }
    for o in outputs {
        field_len(12, &value_info(o), &mut g);
    }
    let mut m = Vec::new();
    field_int(1, 8, &mut m);
    field_len(7, &g, &mut m);
    m
}

#[test]
fn a_small_model_runs_end_to_end() {
    // y = relu(conv3x3(x) + 1) with a kernel that picks the centre, then flattened
    let mut k = vec![0f32; 9];
    k[4] = 2.0;
    let bytes = model(
        vec![
            node("Conv", &["x", "w", "b"], &["c"], &[attr_ints("pads", &[1, 1, 1, 1]), attr_ints("kernel_shape", &[3, 3])]),
            node("Relu", &["c"], &["y"], &[]),
        ],
        vec![init_f32("w", &[1, 1, 3, 3], &k), init_f32("b", &[1], &[1.0])],
        &["x", "w", "b"],
        &["y"],
    );
    let m = Model::parse(&bytes).expect("parse");
    assert_eq!(m.input_names(), &["x".to_string()], "initialisers are not inputs");
    let out = m.run(vec![("x", t(&[1, 1, 2, 2], vec![-1.0, 0.5, 2.0, -3.0]))]).expect("run");
    assert_eq!(out[0].as_f32().expect("f32"), &[0.0, 2.0, 5.0, 0.0]);
    assert!(m.run(vec![]).is_err(), "missing input");
    assert!(m.run(vec![("nope", t(&[1], vec![0.0]))]).is_err());
}

#[test]
fn an_unsupported_operator_is_an_error_not_a_panic() {
    let bytes = model(vec![node("Loop", &["x"], &["y"], &[attr_int("n", 1)])], vec![], &["x"], &["y"]);
    let m = Model::parse(&bytes).expect("parse");
    let e = m.run(vec![("x", t(&[1], vec![0.0]))]).expect_err("must fail");
    assert!(e.0.contains("Loop"), "{e}");
}

#[test]
fn hostile_files_never_panic() {
    let k = vec![0.5f32; 9];
    let good = model(vec![node("Conv", &["x", "w"], &["y"], &[attr_ints("pads", &[1, 1, 1, 1])])], vec![init_f32("w", &[1, 1, 3, 3], &k)], &["x"], &["y"]);
    // every prefix
    for n in 0..good.len() {
        let _ = Model::parse(&good[..n]);
    }
    // random byte flips, parsed and run
    let mut s = 99u32;
    for _ in 0..400 {
        let mut b = good.clone();
        for _ in 0..3 {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let at = (s >> 8) as usize % b.len();
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            b[at] = (s >> 16) as u8;
        }
        if let Ok(m) = Model::parse(&b) {
            let _ = m.run(vec![("x", t(&[1, 1, 3, 3], vec![1.0; 9]))]);
        }
    }
    // absurd sizes
    let huge = model(vec![node("ConstantOfShape", &["s"], &["y"], &[])], vec![], &["s"], &["y"]);
    let m = Model::parse(&huge).expect("parse");
    assert!(m.run(vec![("s", ints(&[3], &[100_000, 100_000, 100_000]))]).is_err());
    assert!(m.run(vec![("s", ints(&[1], &[-5]))]).is_err());
}
