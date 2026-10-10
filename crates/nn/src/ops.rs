//! Element-wise and shape operators.

use crate::tensor::{Data, Tensor, numel, strides};
use crate::{NnError, Result, err};

// ------------------------------------------------------------------------ broadcasting

fn broadcast_shape(a: &[usize], b: &[usize]) -> Result<Vec<usize>> {
    let n = a.len().max(b.len());
    let mut out = vec![1usize; n];
    for i in 0..n {
        let da = if i + a.len() >= n { a.get(i + a.len() - n).copied().unwrap_or(1) } else { 1 };
        let db = if i + b.len() >= n { b.get(i + b.len() - n).copied().unwrap_or(1) } else { 1 };
        let d = if da == db || db == 1 {
            da
        } else if da == 1 {
            db
        } else {
            return err(format!("shapes {a:?} and {b:?} do not broadcast"));
        };
        if let Some(o) = out.get_mut(i) {
            *o = d;
        }
    }
    Ok(out)
}

/// Strides of `shape` seen through `out` (0 where the dimension is broadcast).
fn bstrides(shape: &[usize], out: &[usize]) -> Vec<usize> {
    let st = strides(shape);
    let n = out.len();
    (0..n)
        .map(|i| {
            if i + shape.len() < n {
                return 0;
            }
            let j = i + shape.len() - n;
            if shape.get(j).copied().unwrap_or(1) == 1 { 0 } else { st.get(j).copied().unwrap_or(0) }
        })
        .collect()
}

fn zip2<T: Copy>(a: &[T], ash: &[usize], b: &[T], bsh: &[usize], f: impl Fn(T, T) -> T) -> Result<(Vec<usize>, Vec<T>)> {
    if ash == bsh {
        return Ok((ash.to_vec(), a.iter().zip(b).map(|(x, y)| f(*x, *y)).collect()));
    }
    let out_shape = broadcast_shape(ash, bsh)?;
    let n = numel(&out_shape)?;
    if b.len() == 1 && out_shape == ash {
        let y = b.first().copied();
        let Some(y) = y else { return err("empty operand") };
        return Ok((out_shape, a.iter().map(|x| f(*x, y)).collect()));
    }
    if a.len() == 1 && out_shape == bsh {
        let Some(x) = a.first().copied() else { return err("empty operand") };
        return Ok((out_shape, b.iter().map(|y| f(x, *y)).collect()));
    }
    let sa = bstrides(ash, &out_shape);
    let sb = bstrides(bsh, &out_shape);
    let mut idx = vec![0usize; out_shape.len()];
    let (mut ia, mut ib) = (0usize, 0usize);
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let (Some(x), Some(y)) = (a.get(ia), b.get(ib)) else { return err("broadcast index error") };
        out.push(f(*x, *y));
        for d in (0..idx.len()).rev() {
            let (Some(i), Some(&len)) = (idx.get_mut(d), out_shape.get(d)) else { break };
            *i += 1;
            ia += sa.get(d).copied().unwrap_or(0);
            ib += sb.get(d).copied().unwrap_or(0);
            if *i < len {
                break;
            }
            ia -= sa.get(d).copied().unwrap_or(0) * len;
            ib -= sb.get(d).copied().unwrap_or(0) * len;
            *i = 0;
        }
    }
    Ok((out_shape, out))
}

#[derive(Clone, Copy)]
pub(crate) enum Bin {
    Add,
    Sub,
    Mul,
    Div,
}

pub(crate) fn binary(op: Bin, a: &Tensor, b: &Tensor) -> Result<Tensor> {
    match (&a.data, &b.data) {
        (Data::F(x), Data::F(y)) => {
            let f: fn(f32, f32) -> f32 = match op {
                Bin::Add => |p, q| p + q,
                Bin::Sub => |p, q| p - q,
                Bin::Mul => |p, q| p * q,
                Bin::Div => |p, q| p / q,
            };
            let (shape, v) = zip2(x, &a.shape, y, &b.shape, f)?;
            Tensor::f32(shape, v)
        }
        (Data::I(x), Data::I(y)) => {
            let f: fn(i64, i64) -> i64 = match op {
                Bin::Add => i64::wrapping_add,
                Bin::Sub => i64::wrapping_sub,
                Bin::Mul => i64::wrapping_mul,
                Bin::Div => |p, q| if q == 0 { 0 } else { p.wrapping_div(q) },
            };
            let (shape, v) = zip2(x, &a.shape, y, &b.shape, f)?;
            Tensor::i64(shape, v)
        }
        _ => err("arithmetic on a float and an integer tensor"),
    }
}

pub(crate) fn map_f32(t: &Tensor, f: impl Fn(f32) -> f32) -> Result<Tensor> {
    Tensor::f32(t.shape.clone(), t.as_f32()?.iter().map(|x| f(*x)).collect())
}

// ------------------------------------------------------------------------ casts and constants

pub(crate) fn cast(t: &Tensor, to: i64) -> Result<Tensor> {
    match to {
        1 | 11 => Tensor::f32(t.shape.clone(), t.to_f32_vec()),
        7 | 6 => Tensor::i64(t.shape.clone(), t.to_i64_vec()),
        9 => Tensor::i64(t.shape.clone(), t.to_i64_vec().into_iter().map(|x| i64::from(x != 0)).collect()),
        _ => err(format!("Cast to data type {to} is not supported")),
    }
}

pub(crate) fn constant_of_shape(shape: &Tensor, value: Option<&Tensor>) -> Result<Tensor> {
    let mut dims = Vec::new();
    for d in shape.as_i64()? {
        dims.push(usize::try_from(*d).map_err(|_| NnError("ConstantOfShape: negative dimension".into()))?);
    }
    let n = numel(&dims)?;
    match value.map(|v| &v.data) {
        Some(Data::I(v)) => Tensor::i64(dims, vec![v.first().copied().unwrap_or(0); n]),
        Some(Data::F(v)) => Tensor::f32(dims, vec![v.first().copied().unwrap_or(0.0); n]),
        None => Tensor::f32(dims, vec![0.0; n]),
    }
}

pub(crate) fn shape_of(t: &Tensor) -> Result<Tensor> {
    Tensor::i64(vec![t.shape.len()], t.shape.iter().map(|d| *d as i64).collect())
}

// ------------------------------------------------------------------------ shape operators

fn norm_axis(axis: i64, rank: usize) -> Result<usize> {
    let r = rank as i64;
    let a = if axis < 0 { axis + r } else { axis };
    if a < 0 || a >= r.max(1) {
        return err(format!("axis {axis} out of range for rank {rank}"));
    }
    Ok(a as usize)
}

pub(crate) fn reshape(t: &Tensor, shape: &Tensor) -> Result<Tensor> {
    let want = shape.as_i64()?;
    let mut dims: Vec<usize> = Vec::with_capacity(want.len());
    let mut infer: Option<usize> = None;
    for (i, d) in want.iter().enumerate() {
        match *d {
            -1 => {
                if infer.is_some() {
                    return err("Reshape: more than one -1");
                }
                infer = Some(i);
                dims.push(1);
            }
            0 => dims.push(t.shape.get(i).copied().ok_or_else(|| NnError("Reshape: 0 beyond the input rank".into()))?),
            d if d > 0 => dims.push(d as usize),
            _ => return err("Reshape: negative dimension"),
        }
    }
    if let Some(i) = infer {
        let known = numel(&dims)?;
        if known == 0 || !t.len().is_multiple_of(known) {
            return err(format!("Reshape: cannot fit {} values into {want:?}", t.len()));
        }
        if let Some(d) = dims.get_mut(i) {
            *d = t.len() / known;
        }
    }
    t.reshaped(dims)
}

pub(crate) fn squeeze(t: &Tensor, axes: Option<Vec<i64>>) -> Result<Tensor> {
    let rank = t.shape.len();
    let drop: Vec<usize> = match axes {
        Some(a) => a.iter().map(|x| norm_axis(*x, rank)).collect::<Result<_>>()?,
        None => t.shape.iter().enumerate().filter(|(_, d)| **d == 1).map(|(i, _)| i).collect(),
    };
    for &a in &drop {
        if t.shape.get(a).copied() != Some(1) {
            return err("Squeeze: the axis does not have size 1");
        }
    }
    let shape: Vec<usize> = t.shape.iter().enumerate().filter(|(i, _)| !drop.contains(i)).map(|(_, d)| *d).collect();
    t.reshaped(shape)
}

pub(crate) fn unsqueeze(t: &Tensor, axes: &[i64]) -> Result<Tensor> {
    let rank = t.shape.len() + axes.len();
    let mut ax: Vec<usize> = axes.iter().map(|x| norm_axis(*x, rank)).collect::<Result<_>>()?;
    ax.sort_unstable();
    ax.dedup();
    if ax.len() != axes.len() {
        return err("Unsqueeze: repeated axis");
    }
    let mut shape = Vec::with_capacity(rank);
    let mut src = t.shape.iter();
    for i in 0..rank {
        if ax.contains(&i) {
            shape.push(1);
        } else {
            shape.push(*src.next().ok_or_else(|| NnError("Unsqueeze: axis out of range".into()))?);
        }
    }
    t.reshaped(shape)
}

/// Output built from per-axis index lists: `out[i0, i1, …] = t[idx0[i0], idx1[i1], …]`.
fn select(t: &Tensor, idx: &[Vec<usize>]) -> Result<Tensor> {
    let shape: Vec<usize> = idx.iter().map(Vec::len).collect();
    let n = numel(&shape)?;
    let st = strides(&t.shape);
    let mut pos = vec![0usize; shape.len()];
    let mut flat: Vec<usize> = Vec::with_capacity(n);
    for _ in 0..n {
        let mut off = 0usize;
        for (d, p) in pos.iter().enumerate() {
            off += idx.get(d).and_then(|v| v.get(*p)).copied().unwrap_or(0) * st.get(d).copied().unwrap_or(0);
        }
        flat.push(off);
        for d in (0..pos.len()).rev() {
            if let Some(p) = pos.get_mut(d) {
                *p += 1;
                if *p < shape.get(d).copied().unwrap_or(0) {
                    break;
                }
                *p = 0;
            }
        }
    }
    match &t.data {
        Data::F(v) => Tensor::f32(shape, flat.iter().map(|o| v.get(*o).copied().unwrap_or(0.0)).collect()),
        Data::I(v) => Tensor::i64(shape, flat.iter().map(|o| v.get(*o).copied().unwrap_or(0)).collect()),
    }
}

pub(crate) fn transpose(t: &Tensor, perm: Option<&[i64]>) -> Result<Tensor> {
    let rank = t.shape.len();
    let p: Vec<usize> = match perm {
        Some(p) => p.iter().map(|x| norm_axis(*x, rank)).collect::<Result<_>>()?,
        None => (0..rank).rev().collect(),
    };
    if p.len() != rank {
        return err("Transpose: perm length differs from the rank");
    }
    let st = strides(&t.shape);
    let out_shape: Vec<usize> = p.iter().map(|a| t.shape.get(*a).copied().unwrap_or(1)).collect();
    let in_st: Vec<usize> = p.iter().map(|a| st.get(*a).copied().unwrap_or(0)).collect();
    let n = numel(&out_shape)?;
    let mut pos = vec![0usize; rank];
    let mut off = 0usize;
    let mut flat = Vec::with_capacity(n);
    for _ in 0..n {
        flat.push(off);
        for d in (0..rank).rev() {
            let (Some(c), Some(&len)) = (pos.get_mut(d), out_shape.get(d)) else { break };
            *c += 1;
            off += in_st.get(d).copied().unwrap_or(0);
            if *c < len {
                break;
            }
            off -= in_st.get(d).copied().unwrap_or(0) * len;
            *c = 0;
        }
    }
    match &t.data {
        Data::F(v) => Tensor::f32(out_shape, flat.iter().map(|o| v.get(*o).copied().unwrap_or(0.0)).collect()),
        Data::I(v) => Tensor::i64(out_shape, flat.iter().map(|o| v.get(*o).copied().unwrap_or(0)).collect()),
    }
}

pub(crate) fn concat(ts: &[&Tensor], axis: i64) -> Result<Tensor> {
    let Some(first) = ts.first() else { return err("Concat without inputs") };
    let rank = first.shape.len();
    let ax = norm_axis(axis, rank)?;
    let mut shape = first.shape.clone();
    let mut total = 0usize;
    for t in ts {
        if t.shape.len() != rank || t.is_float() != first.is_float() {
            return err("Concat: inputs differ in rank or type");
        }
        for (i, (a, b)) in t.shape.iter().zip(&first.shape).enumerate() {
            if i != ax && a != b {
                return err(format!("Concat: shapes {:?} and {:?} differ off the axis", t.shape, first.shape));
            }
        }
        total += t.shape.get(ax).copied().unwrap_or(0);
    }
    if let Some(d) = shape.get_mut(ax) {
        *d = total;
    }
    let outer: usize = first.shape.get(..ax).map_or(1, |s| s.iter().product());
    let inner: usize = first.shape.get(ax + 1..).map_or(1, |s| s.iter().product());
    numel(&shape)?;
    macro_rules! join {
        ($variant:ident, $ctor:ident) => {{
            let mut out = Vec::new();
            for o in 0..outer {
                for t in ts {
                    let Data::$variant(v) = &t.data else { return err("Concat: mixed types") };
                    let len = t.shape.get(ax).copied().unwrap_or(0) * inner;
                    out.extend_from_slice(v.get(o * len..(o + 1) * len).unwrap_or(&[]));
                }
            }
            Tensor::$ctor(shape, out)
        }};
    }
    if first.is_float() { join!(F, f32) } else { join!(I, i64) }
}

pub(crate) fn slice(t: &Tensor, starts: &[i64], ends: &[i64], axes: Option<&[i64]>, steps: Option<&[i64]>) -> Result<Tensor> {
    let rank = t.shape.len();
    let mut idx: Vec<Vec<usize>> = t.shape.iter().map(|d| (0..*d).collect()).collect();
    for (i, (&st, &en)) in starts.iter().zip(ends).enumerate() {
        let ax = match axes {
            Some(a) => norm_axis(a.get(i).copied().unwrap_or(i as i64), rank)?,
            None => i,
        };
        let step = steps.and_then(|s| s.get(i)).copied().unwrap_or(1);
        if step == 0 {
            return err("Slice: step 0");
        }
        let d = t.shape.get(ax).copied().unwrap_or(0) as i64;
        let norm = |v: i64| if v < 0 { v.saturating_add(d) } else { v };
        let (b, e) = if step > 0 { (norm(st).clamp(0, d), norm(en).clamp(0, d)) } else { (norm(st).clamp(-1, d - 1), norm(en).clamp(-1, d - 1)) };
        let mut v = Vec::new();
        let mut c = b;
        while (step > 0 && c < e) || (step < 0 && c > e) {
            if c >= 0 && c < d {
                v.push(c as usize);
            }
            c += step;
        }
        if let Some(slot) = idx.get_mut(ax) {
            *slot = v;
        }
    }
    select(t, &idx)
}

pub(crate) fn gather(t: &Tensor, indices: &Tensor, axis: i64) -> Result<Tensor> {
    let rank = t.shape.len();
    let ax = norm_axis(axis, rank)?;
    let dim = t.shape.get(ax).copied().unwrap_or(0) as i64;
    let ind: Vec<usize> = indices
        .as_i64()?
        .iter()
        .map(|i| {
            let j = if *i < 0 { *i + dim } else { *i };
            if j < 0 || j >= dim { err(format!("Gather: index {i} out of range {dim}")) } else { Ok(j as usize) }
        })
        .collect::<Result<_>>()?;
    let mut shape: Vec<usize> = t.shape.get(..ax).unwrap_or(&[]).to_vec();
    shape.extend_from_slice(&indices.shape);
    shape.extend_from_slice(t.shape.get(ax + 1..).unwrap_or(&[]));
    let outer: usize = t.shape.get(..ax).map_or(1, |s| s.iter().product());
    let inner: usize = t.shape.get(ax + 1..).map_or(1, |s| s.iter().product());
    numel(&shape)?;
    macro_rules! pick {
        ($variant:ident, $ctor:ident) => {{
            let Data::$variant(v) = &t.data else { return err("Gather: type error") };
            let mut out = Vec::with_capacity(outer * ind.len() * inner);
            for o in 0..outer {
                for &j in &ind {
                    let base = (o * dim as usize + j) * inner;
                    out.extend_from_slice(v.get(base..base + inner).unwrap_or(&[]));
                }
            }
            Tensor::$ctor(shape, out)
        }};
    }
    if t.is_float() { pick!(F, f32) } else { pick!(I, i64) }
}

/// Constant padding; negative pads crop.
pub(crate) fn pad(t: &Tensor, pads: &[i64], value: f32) -> Result<Tensor> {
    let rank = t.shape.len();
    if pads.len() != rank * 2 {
        return err("Pad: the pads tensor must hold 2 values per dimension");
    }
    let mut out_shape = Vec::with_capacity(rank);
    for d in 0..rank {
        let before = pads.get(d).copied().unwrap_or(0);
        let after = pads.get(rank + d).copied().unwrap_or(0);
        let n = t.shape.get(d).copied().unwrap_or(0) as i64 + before + after;
        if n < 0 {
            return err("Pad: negative output size");
        }
        out_shape.push(n as usize);
    }
    let n = numel(&out_shape)?;
    let st = strides(&t.shape);
    let mut pos = vec![0usize; rank];
    macro_rules! build {
        ($variant:ident, $ctor:ident, $fill:expr) => {{
            let Data::$variant(v) = &t.data else { return err("Pad: type error") };
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                let mut off = 0usize;
                let mut inside = true;
                for d in 0..rank {
                    let i = pos.get(d).copied().unwrap_or(0) as i64 - pads.get(d).copied().unwrap_or(0);
                    if i < 0 || i >= t.shape.get(d).copied().unwrap_or(0) as i64 {
                        inside = false;
                        break;
                    }
                    off += i as usize * st.get(d).copied().unwrap_or(0);
                }
                out.push(if inside { v.get(off).copied().unwrap_or($fill) } else { $fill });
                for d in (0..rank).rev() {
                    if let Some(p) = pos.get_mut(d) {
                        *p += 1;
                        if *p < out_shape.get(d).copied().unwrap_or(0) {
                            break;
                        }
                        *p = 0;
                    }
                }
            }
            Tensor::$ctor(out_shape, out)
        }};
    }
    if t.is_float() { build!(F, f32, value) } else { build!(I, i64, value as i64) }
}
