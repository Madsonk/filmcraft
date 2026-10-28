//! RIFE v4 (Real-Time Intermediate Flow Estimation, Huang et al., ECCV 2022, MIT) on candle.
//!
//! The network takes two frames and returns the frame half-way between them. The architecture is the
//! `IFNet` of the `ECCV2022-RIFE` repository (MIT) as re-hosted in safetensors by TensorForger; this
//! file is a re-implementation from that definition, not a copy. The teacher branch (`block_tea`) is
//! training-only and is not loaded.
//!
//! Only the midpoint is predicted, so a retime by another ratio uses [`Rife::at`], which recurses on
//! midpoints down to 1/8 of a frame (`depth` 3): the time is rounded to the nearest eighth.
//!
//! Frames are padded (edge replicate) to a multiple of 32 and cropped back.

use std::collections::HashMap;
use std::path::Path;

use candle_core::{D, DType, Device, Tensor};

use crate::{Error, Image, Result, pick_device, read_file};

/// File name in the model folder.
pub const WEIGHTS: &str = "flownet.safetensors";

fn t(w: &HashMap<String, Tensor>, name: &str) -> Result<Tensor> {
    w.get(name).cloned().ok_or_else(|| Error::Model(format!("{WEIGHTS}: missing tensor {name}")))
}

struct Conv {
    w: Tensor,
    b: Tensor,
    prelu: Tensor,
    stride: usize,
}

impl Conv {
    /// `prefix.0` is the convolution, `prefix.1` the PReLU.
    fn load(w: &HashMap<String, Tensor>, prefix: &str, stride: usize) -> Result<Self> {
        let b = t(w, &format!("{prefix}.0.bias"))?;
        let c = b.dim(0)?;
        Ok(Self {
            w: t(w, &format!("{prefix}.0.weight"))?,
            b: b.reshape((1, c, 1, 1))?,
            prelu: t(w, &format!("{prefix}.1.weight"))?.reshape((1, c, 1, 1))?,
            stride,
        })
    }
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let y = x.conv2d(&self.w, 1, self.stride, 1, 1)?.broadcast_add(&self.b)?;
        prelu(&y, &self.prelu)
    }
}

/// `max(x, 0) + a * min(x, 0)`.
fn prelu(x: &Tensor, a: &Tensor) -> Result<Tensor> {
    let neg = x.neg()?.relu()?.neg()?;
    Ok((x.relu()? + neg.broadcast_mul(a)?)?)
}

struct Up {
    w0: Tensor,
    b0: Tensor,
    a: Tensor,
    w1: Tensor,
    b1: Tensor,
}

impl Up {
    fn load(w: &HashMap<String, Tensor>, p: &str) -> Result<Self> {
        let b0 = t(w, &format!("{p}.0.bias"))?;
        let b1 = t(w, &format!("{p}.2.bias"))?;
        let (c0, c1) = (b0.dim(0)?, b1.dim(0)?);
        Ok(Self {
            w0: t(w, &format!("{p}.0.weight"))?,
            b0: b0.reshape((1, c0, 1, 1))?,
            a: t(w, &format!("{p}.1.weight"))?.reshape((1, c0, 1, 1))?,
            w1: t(w, &format!("{p}.2.weight"))?,
            b1: b1.reshape((1, c1, 1, 1))?,
        })
    }
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let y = x.conv_transpose2d(&self.w0, 1, 0, 2, 1)?.broadcast_add(&self.b0)?;
        let y = prelu(&y, &self.a)?;
        Ok(y.conv_transpose2d(&self.w1, 1, 0, 2, 1)?.broadcast_add(&self.b1)?)
    }
}

struct Block {
    conv0: [Conv; 2],
    blocks: Vec<[Conv; 2]>,
    flow: Up,
    mask: Up,
}

impl Block {
    fn load(w: &HashMap<String, Tensor>, p: &str) -> Result<Self> {
        let mut blocks = Vec::new();
        for i in 0..4 {
            blocks.push([Conv::load(w, &format!("{p}.convblock{i}.0"), 1)?, Conv::load(w, &format!("{p}.convblock{i}.1"), 1)?]);
        }
        Ok(Self {
            conv0: [Conv::load(w, &format!("{p}.conv0.0"), 2)?, Conv::load(w, &format!("{p}.conv0.1"), 2)?],
            blocks,
            flow: Up::load(w, &format!("{p}.conv1"))?,
            mask: Up::load(w, &format!("{p}.conv2"))?,
        })
    }

    fn forward(&self, x: &Tensor, flow: &Tensor, scale: usize) -> Result<(Tensor, Tensor)> {
        let down = |v: &Tensor| -> Result<Tensor> {
            if scale == 1 { Ok(v.clone()) } else { Ok(v.upsample_bilinear2d_with_scale(1.0 / scale as f64, 1.0 / scale as f64, false)?) }
        };
        let up = |v: &Tensor| -> Result<Tensor> {
            if scale == 1 { Ok(v.clone()) } else { Ok(v.upsample_bilinear2d_with_scale(scale as f64, scale as f64, false)?) }
        };
        let x = down(x)?;
        let flow = (down(flow)? * (1.0 / scale as f64))?;
        let mut feat = Tensor::cat(&[&x, &flow], 1)?;
        for c in &self.conv0 {
            feat = c.forward(&feat)?;
        }
        for pair in &self.blocks {
            let mut y = feat.clone();
            for c in pair {
                y = c.forward(&y)?;
            }
            feat = (y + &feat)?;
        }
        let flow = (up(&self.flow.forward(&feat)?)? * scale as f64)?;
        let mask = up(&self.mask.forward(&feat)?)?;
        Ok((flow, mask))
    }
}

/// Backward warp of `img` (1,C,H,W) by `flow` (1,2,H,W) in pixels: bilinear, border padding, the
/// `grid_sample(align_corners=True)` of the reference.
fn warp(img: &Tensor, flow: &Tensor) -> Result<Tensor> {
    let (_, c, h, w) = img.dims4()?;
    let dev = img.device();
    let xs = Tensor::arange(0u32, w as u32, dev)?.to_dtype(DType::F32)?.reshape((1, 1, 1, w))?.broadcast_as((1, 1, h, w))?;
    let ys = Tensor::arange(0u32, h as u32, dev)?.to_dtype(DType::F32)?.reshape((1, 1, h, 1))?.broadcast_as((1, 1, h, w))?;
    let px = (xs + flow.narrow(1, 0, 1)?)?.clamp(0f32, (w - 1) as f32)?;
    let py = (ys + flow.narrow(1, 1, 1)?)?.clamp(0f32, (h - 1) as f32)?;
    let (x0, y0) = (px.floor()?, py.floor()?);
    let (wx, wy) = ((&px - &x0)?, (&py - &y0)?);
    let x1 = (&x0 + 1.0)?.clamp(0f32, (w - 1) as f32)?;
    let y1 = (&y0 + 1.0)?.clamp(0f32, (h - 1) as f32)?;
    let flat = img.reshape((1, c, h * w))?;
    let sample = |yy: &Tensor, xx: &Tensor| -> Result<Tensor> {
        let idx = ((yy * w as f64)? + xx)?.to_dtype(DType::U32)?.reshape((1, 1, h * w))?.broadcast_as((1, c, h * w))?.contiguous()?;
        Ok(flat.gather(&idx, 2)?.reshape((1, c, h, w))?)
    };
    let (v00, v01, v10, v11) = (sample(&y0, &x0)?, sample(&y0, &x1)?, sample(&y1, &x0)?, sample(&y1, &x1)?);
    let one = Tensor::ones_like(&wx)?;
    let (iwx, iwy) = ((&one - &wx)?, (&one - &wy)?);
    let top = ((v00.broadcast_mul(&iwx)?) + v01.broadcast_mul(&wx)?)?;
    let bot = ((v10.broadcast_mul(&iwx)?) + v11.broadcast_mul(&wx)?)?;
    Ok(((top.broadcast_mul(&iwy)?) + bot.broadcast_mul(&wy)?)?)
}

/// The loaded network.
pub struct Rife {
    blocks: [Block; 3],
    dev: Device,
    /// Flow estimation scale: 1 is the reference (blocks work at 1/4, 1/8 and 1/16 of the frame), 0.5 estimates
    /// the flow on half the pixels per side (about 3 times faster, a little less precise on fine motion) and the
    /// final warp still happens at full size.
    pub scale: f64,
}

/// Two frames prepared for interpolation, with the in-between frames already computed (a binary tree of
/// midpoints: asking for 1/4 and 3/4 after 1/2 costs one network pass each, not two).
pub struct Interval {
    a: Tensor,
    b: Tensor,
    w: usize,
    h: usize,
    nodes: HashMap<(u32, u32), Tensor>,
}

fn to_tensor(img: &Image, dev: &Device, pad_w: usize, pad_h: usize) -> Result<Tensor> {
    let x = Tensor::from_vec(img.data.clone(), (1, img.height, img.width, 3), dev)?.permute((0, 3, 1, 2))?.contiguous()?;
    let x = if pad_w > 0 { x.pad_with_same(D::Minus1, 0, pad_w)? } else { x };
    Ok(if pad_h > 0 { x.pad_with_same(D::Minus2, 0, pad_h)? } else { x })
}

impl Rife {
    pub fn load(dir: &Path) -> Result<Self> {
        let dev = pick_device();
        let p = dir.join(WEIGHTS);
        let w = candle_core::safetensors::load_buffer(&read_file(&p)?, &dev).map_err(|e| Error::Model(format!("{}: {e}", p.display())))?;
        let blocks = [Block::load(&w, "block0")?, Block::load(&w, "block1")?, Block::load(&w, "block2")?];
        Ok(Self { blocks, dev, scale: 1.0 })
    }

    pub fn device(&self) -> &Device {
        &self.dev
    }

    fn net(&self, a: &Tensor, b: &Tensor) -> Result<Tensor> {
        let (n, _, h, w) = a.dims4()?;
        let mut flow = Tensor::zeros((n, 4, h, w), DType::F32, &self.dev)?;
        let mut mask = Tensor::zeros((n, 1, h, w), DType::F32, &self.dev)?;
        let (mut wa, mut wb) = (a.clone(), b.clone());
        let sc = self.scale.clamp(0.25, 1.0);
        let scales = [(4.0 / sc).round() as usize, (2.0 / sc).round() as usize, (1.0 / sc).round() as usize];
        for (blk, s) in self.blocks.iter().zip(scales) {
            let (f0, m0) = blk.forward(&Tensor::cat(&[&wa, &wb, &mask], 1)?, &flow, s)?;
            let swapped = Tensor::cat(&[&flow.narrow(1, 2, 2)?, &flow.narrow(1, 0, 2)?], 1)?;
            let (f1, m1) = blk.forward(&Tensor::cat(&[&wb, &wa, &mask.neg()?], 1)?, &swapped, s)?;
            let f1s = Tensor::cat(&[&f1.narrow(1, 2, 2)?, &f1.narrow(1, 0, 2)?], 1)?;
            flow = (flow + ((f0 + f1s)? * 0.5)?)?;
            mask = (mask + ((m0 + m1.neg()?)? * 0.5)?)?;
            wa = warp(a, &flow.narrow(1, 0, 2)?)?;
            wb = warp(b, &flow.narrow(1, 2, 2)?)?;
        }
        let m = candle_nn::ops::sigmoid(&mask)?;
        let inv = (Tensor::ones_like(&m)? - &m)?;
        Ok(((wa.broadcast_mul(&m)?) + wb.broadcast_mul(&inv)?)?)
    }

    fn mid_tensor(&self, a: &Tensor, b: &Tensor) -> Result<Tensor> {
        Ok(self.net(a, b)?.clamp(0f32, 1f32)?)
    }

    fn pads(&self, w: usize, h: usize) -> (usize, usize) {
        let m = (32.0 / self.scale.clamp(0.25, 1.0)).round() as usize;
        ((m - w % m) % m, (m - h % m) % m)
    }

    fn to_image(&self, y: &Tensor, w: usize, h: usize) -> Result<Image> {
        let y = y.narrow(2, 0, h)?.narrow(3, 0, w)?.permute((0, 2, 3, 1))?.contiguous()?.flatten_all()?;
        Ok(Image { width: w, height: h, data: y.to_vec1::<f32>()? })
    }

    /// The frame half-way between `a` and `b` (same size).
    pub fn middle(&self, a: &Image, b: &Image) -> Result<Image> {
        self.at(a, b, 0.5, 1)
    }

    /// The frame at time `t` in 0..1 between `a` (0) and `b` (1), rounded to a multiple of 1/2^depth
    /// (depth 1..=4): recursion on midpoints.
    pub fn at(&self, a: &Image, b: &Image, t: f64, depth: u32) -> Result<Image> {
        let mut iv = self.interval(a, b)?;
        self.at_in(&mut iv, t, depth)
    }

    /// Prepare two frames (same size) for several [`Rife::at_in`] calls.
    pub fn interval(&self, a: &Image, b: &Image) -> Result<Interval> {
        if a.width != b.width || a.height != b.height {
            return Err(Error::Input("the two frames differ in size".into()));
        }
        let (pw, ph) = self.pads(a.width, a.height);
        Ok(Interval { a: to_tensor(a, &self.dev, pw, ph)?, b: to_tensor(b, &self.dev, pw, ph)?, w: a.width, h: a.height, nodes: HashMap::new() })
    }

    /// The frame at time `t` of the interval (rounded to a multiple of 1/2^depth), reusing earlier midpoints.
    pub fn at_in(&self, iv: &mut Interval, t: f64, depth: u32) -> Result<Image> {
        let depth = depth.clamp(1, 4);
        let steps = 1u32 << depth;
        let k = (t.clamp(0.0, 1.0) * steps as f64).round() as u32;
        let y = self.node(iv, k, steps)?;
        self.to_image(&y, iv.w, iv.h)
    }

    /// The tensor at k/steps (nodes are keyed by their reduced fraction).
    fn node(&self, iv: &mut Interval, k: u32, steps: u32) -> Result<Tensor> {
        if k == 0 {
            return Ok(iv.a.clone());
        }
        if k >= steps {
            return Ok(iv.b.clone());
        }
        let g = k.trailing_zeros().min(steps.trailing_zeros());
        let (k, steps) = (k >> g, steps >> g);
        if k == 0 {
            return Ok(iv.a.clone());
        }
        if let Some(n) = iv.nodes.get(&(k, steps)) {
            return Ok(n.clone());
        }
        // bisect: the midpoint of the neighbouring nodes (lo, hi) at the coarsest level that contains k/steps
        let (mut lo, mut hi, mut lo_f, mut hi_f) = (iv.a.clone(), iv.b.clone(), (0u32, 1u32), (1u32, 1u32));
        loop {
            let mid_f = (lo_f.0 * hi_f.1 + hi_f.0 * lo_f.1, 2 * lo_f.1 * hi_f.1);
            let g = mid_f.0.trailing_zeros().min(mid_f.1.trailing_zeros());
            let mid_f = (mid_f.0 >> g, mid_f.1 >> g);
            let mid = match iv.nodes.get(&mid_f) {
                Some(m) => m.clone(),
                None => {
                    let m = self.mid_tensor(&lo, &hi)?;
                    iv.nodes.insert(mid_f, m.clone());
                    m
                }
            };
            // compare k/steps with mid_f by cross multiplication
            let (l, r) = (k * mid_f.1, mid_f.0 * steps);
            if l == r {
                return Ok(mid);
            }
            if l < r {
                hi = mid;
                hi_f = mid_f;
            } else {
                lo = mid;
                lo_f = mid_f;
            }
        }
    }
}
