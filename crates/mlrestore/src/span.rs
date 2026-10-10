//! SPAN (Swift Parameter-free Attention Network, Wan et al., CVPR Workshops 2024): inference only.
//!
//! A port of `basicsr/archs/span_arch.py` from <https://github.com/hongyuanyu/SPAN> (Apache-2.0;
//! the file says so and that it was modified, see `NOTICE`). Every `Conv3XC` (a 1x1, a 3x3 and a
//! 1x1 convolution plus a 1x1 shortcut) is folded into a single 3x3 convolution when the weights
//! are loaded, exactly as the reference does at inference, so the network is plain 3x3
//! convolutions, SiLU, a parameter-free attention (`sigmoid(x) - 0.5`) and a pixel shuffle.
//!
//! Weights: `2xNomosUni_span_multijpg.safetensors` by Philip Hofmann (CC BY 4.0), a 2x model
//! trained on the NomosUni dataset with JPEG and multi-scale degradations (see [`crate::files`]).
//! Input and output are NCHW, 0..1, sRGB-like encoding.

use std::collections::HashMap;

use candle_core::{DType, Device, Tensor};
use candle_nn::ops;

use crate::{Error, Result};

type Weights = HashMap<String, Tensor>;

fn get(w: &Weights, name: &str) -> Result<Tensor> {
    w.get(name).cloned().ok_or_else(|| Error::Model(format!("missing weight {name}")))
}

/// A 3x3 convolution with bias (the folded `Conv3XC`).
struct Conv3 {
    w: Tensor,
    b: Tensor,
}

impl Conv3 {
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        Ok(x.conv2d(&self.w, 1, 1, 1, 1)?.broadcast_add(&self.b.reshape((1, (), 1, 1))?)?)
    }

    /// Fold `<name>.conv.{0,1,2}` and `<name>.sk` into one 3x3 convolution.
    ///
    /// The three convolutions are linear, so the folded kernel is the network's response to an
    /// impulse at each of the 3 x 3 taps of each input channel; the bias is its response to a
    /// zero input. Zero padding commutes with this (the reference's training path pads the input
    /// with zeros, its inference path pads the folded convolution's input with zeros).
    fn fold(w: &Weights, name: &str) -> Result<Self> {
        let (w1, b1) = (get(w, &format!("{name}.conv.0.weight"))?, get(w, &format!("{name}.conv.0.bias"))?);
        let (w2, b2) = (get(w, &format!("{name}.conv.1.weight"))?, get(w, &format!("{name}.conv.1.bias"))?);
        let (w3, b3) = (get(w, &format!("{name}.conv.2.weight"))?, get(w, &format!("{name}.conv.2.bias"))?);
        let (sk_w, sk_b) = (get(w, &format!("{name}.sk.weight"))?, get(w, &format!("{name}.sk.bias"))?);
        let (c_out, c_in) = (sk_w.dim(0)?, sk_w.dim(1)?);
        let dev = w1.device();
        let chain = |x: &Tensor, bias: bool| -> Result<Tensor> {
            let bz = |b: &Tensor| -> Result<Tensor> { Ok(if bias { b.reshape((1, (), 1, 1))? } else { b.zeros_like()?.reshape((1, (), 1, 1))? }) };
            let y = x.conv2d(&w1, 0, 1, 1, 1)?.broadcast_add(&bz(&b1)?)?;
            let y = y.conv2d(&w2, 0, 1, 1, 1)?.broadcast_add(&bz(&b2)?)?;
            Ok(y.conv2d(&w3, 0, 1, 1, 1)?.broadcast_add(&bz(&b3)?)?)
        };
        // Impulses: image k = (i, ky, kx) has a 1 in channel i at tap (ky, kx).
        let mut imp = vec![0f32; c_in * 9 * c_in * 9];
        for k in 0..c_in * 9 {
            let (i, tap) = (k / 9, k % 9);
            imp[k * c_in * 9 + i * 9 + tap] = 1.0;
        }
        let imp = Tensor::from_vec(imp, (c_in * 9, c_in, 3, 3), dev)?;
        // [c_in * 9, c_out, 1, 1] -> [c_out, c_in, 3, 3]
        let wk = chain(&imp, false)?.reshape((c_in, 3, 3, c_out))?.permute((3, 0, 1, 2))?.contiguous()?;
        let bias = chain(&Tensor::zeros((1, c_in, 3, 3), DType::F32, dev)?, true)?.reshape(c_out)?;
        // The 1x1 shortcut joins at the centre tap.
        let sk = sk_w.pad_with_zeros(2, 1, 1)?.pad_with_zeros(3, 1, 1)?;
        Ok(Self { w: (wk + sk)?, b: (bias + sk_b)? })
    }
}

struct Spab {
    c1: Conv3,
    c2: Conv3,
    c3: Conv3,
}

impl Spab {
    /// Returns the block's output and its first convolution's output.
    fn forward(&self, x: &Tensor) -> Result<(Tensor, Tensor)> {
        let o1 = self.c1.forward(x)?;
        let o2 = self.c2.forward(&ops::silu(&o1)?)?;
        let o3 = self.c3.forward(&ops::silu(&o2)?)?;
        let att = (ops::sigmoid(&o3)? - 0.5)?;
        Ok(((o3 + x)?.mul(&att)?, o1))
    }
}

/// How the input is normalised before the network: `(x - mean) * range`. The reference's
/// defaults (`rgb_mean = (0.4488, 0.4371, 0.4040)`, `img_range = 255`) are what the NomosUni
/// checkpoint expects (measured: without them the output is uncorrelated with the photo).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Norm {
    None,
    Mean { mean: [f32; 3], range: f32 },
}

impl Norm {
    pub const REFERENCE: Norm = Norm::Mean { mean: [0.4488, 0.4371, 0.4040], range: 255.0 };
}

pub struct Span {
    conv_1: Conv3,
    blocks: Vec<Spab>,
    conv_2: Conv3,
    conv_cat: (Tensor, Tensor),
    up: (Tensor, Tensor),
    scale: usize,
    norm: Norm,
    device: Device,
}

impl Span {
    pub fn load(w: &Weights, norm: Norm, dev: &Device) -> Result<Self> {
        let blocks = (1..=6)
            .map(|i| {
                let p = |c: usize| Conv3::fold(w, &format!("block_{i}.c{c}_r"));
                Ok(Spab { c1: p(1)?, c2: p(2)?, c3: p(3)? })
            })
            .collect::<Result<Vec<_>>>()?;
        let up_w = get(w, "upsampler.0.weight")?;
        let scale = match up_w.dim(0)? {
            12 => 2,
            27 => 3,
            48 => 4,
            n => return Err(Error::Model(format!("SPAN upsampler with {n} output channels (expected 12, 27 or 48)"))),
        };
        Ok(Self {
            conv_1: Conv3::fold(w, "conv_1")?,
            blocks,
            conv_2: Conv3::fold(w, "conv_2")?,
            conv_cat: (get(w, "conv_cat.weight")?, get(w, "conv_cat.bias")?),
            up: (up_w, get(w, "upsampler.0.bias")?),
            scale,
            norm,
            device: dev.clone(),
        })
    }

    pub fn scale(&self) -> usize {
        self.scale
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    /// `x`: `[1, 3, H, W]`, 0..1 -> `[1, 3, H * scale, W * scale]` (not clamped).
    pub fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let x = match self.norm {
            Norm::None => x.clone(),
            Norm::Mean { mean, range } => {
                let m = Tensor::from_vec(mean.to_vec(), (1, 3, 1, 1), x.device())?;
                (x.broadcast_sub(&m)? * range as f64)?
            }
        };
        let f = self.conv_1.forward(&x)?;
        let (b1, _) = self.blocks[0].forward(&f)?;
        let (mut cur, mut o1) = (b1.clone(), b1.clone());
        for (i, b) in self.blocks.iter().enumerate().skip(1) {
            let (n, first) = b.forward(&cur)?;
            cur = n;
            if i == 5 {
                // The reference applies its in-place SiLU to this tensor, so the activated
                // values are what it returns (and concatenates).
                o1 = ops::silu(&first)?;
            }
        }
        let b6 = self.conv_2.forward(&cur)?;
        let cat = Tensor::cat(&[&f, &b6, &b1, &o1], 1)?;
        let y = cat.conv2d(&self.conv_cat.0, 0, 1, 1, 1)?.broadcast_add(&self.conv_cat.1.reshape((1, (), 1, 1))?)?;
        let y = y.conv2d(&self.up.0, 1, 1, 1, 1)?.broadcast_add(&self.up.1.reshape((1, (), 1, 1))?)?;
        Ok(ops::pixel_shuffle(&y, self.scale)?)
    }
}

/// The tensors of a safetensors file as f32 on `dev`.
pub fn read_safetensors(bytes: &[u8], dev: &Device) -> Result<Weights> {
    let raw = candle_core::safetensors::load_buffer(bytes, &Device::Cpu)?;
    raw.into_iter().map(|(k, v)| Ok((k, v.to_dtype(DType::F32)?.to_device(dev)?))).collect()
}
