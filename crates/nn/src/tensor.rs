//! Dense tensors: row-major `f32` or `i64`.

use crate::{MAX_ELEMENTS, Result, err};

#[derive(Clone, Debug, PartialEq)]
pub enum Data {
    F(Vec<f32>),
    I(Vec<i64>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tensor {
    pub shape: Vec<usize>,
    pub data: Data,
}

/// Product of `shape`, refusing overflow and more than [`MAX_ELEMENTS`].
pub fn numel(shape: &[usize]) -> Result<usize> {
    let mut n: usize = 1;
    for &d in shape {
        n = match n.checked_mul(d) {
            Some(v) if v <= MAX_ELEMENTS => v,
            _ => return err(format!("tensor too large: shape {shape:?}")),
        };
    }
    Ok(n)
}

impl Tensor {
    pub fn f32(shape: Vec<usize>, v: Vec<f32>) -> Result<Tensor> {
        if numel(&shape)? != v.len() {
            return err(format!("shape {shape:?} does not hold {} values", v.len()));
        }
        Ok(Tensor { shape, data: Data::F(v) })
    }

    pub fn i64(shape: Vec<usize>, v: Vec<i64>) -> Result<Tensor> {
        if numel(&shape)? != v.len() {
            return err(format!("shape {shape:?} does not hold {} values", v.len()));
        }
        Ok(Tensor { shape, data: Data::I(v) })
    }

    pub fn zeros(shape: Vec<usize>) -> Result<Tensor> {
        let n = numel(&shape)?;
        Ok(Tensor { shape, data: Data::F(vec![0.0; n]) })
    }

    pub fn len(&self) -> usize {
        match &self.data {
            Data::F(v) => v.len(),
            Data::I(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn as_f32(&self) -> Result<&[f32]> {
        match &self.data {
            Data::F(v) => Ok(v),
            Data::I(_) => err("expected a float tensor"),
        }
    }

    pub fn as_i64(&self) -> Result<&[i64]> {
        match &self.data {
            Data::I(v) => Ok(v),
            Data::F(_) => err("expected an integer tensor"),
        }
    }

    /// The values as `i64` (floats are truncated).
    pub fn to_i64_vec(&self) -> Vec<i64> {
        match &self.data {
            Data::I(v) => v.clone(),
            Data::F(v) => v.iter().map(|x| *x as i64).collect(),
        }
    }

    /// The values as `f32`.
    pub fn to_f32_vec(&self) -> Vec<f32> {
        match &self.data {
            Data::F(v) => v.clone(),
            Data::I(v) => v.iter().map(|x| *x as f32).collect(),
        }
    }

    pub fn is_float(&self) -> bool {
        matches!(self.data, Data::F(_))
    }

    /// Same data, new shape.
    pub fn reshaped(&self, shape: Vec<usize>) -> Result<Tensor> {
        if numel(&shape)? != self.len() {
            return err(format!("cannot reshape {:?} into {shape:?}", self.shape));
        }
        Ok(Tensor { shape, data: self.data.clone() })
    }
}

/// Row-major strides of `shape`.
pub fn strides(shape: &[usize]) -> Vec<usize> {
    let mut s = vec![1usize; shape.len()];
    for i in (0..shape.len().saturating_sub(1)).rev() {
        s[i] = s[i + 1].saturating_mul(shape.get(i + 1).copied().unwrap_or(1));
    }
    s
}
