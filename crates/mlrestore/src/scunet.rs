//! SCUNet (Swin-Conv-UNet, Zhang et al., "Practical Blind Image Denoising via Swin-Conv-UNet and
//! Data Synthesis", Machine Intelligence Research 2023): inference only.
//!
//! A port of `models/network_scunet.py` from <https://github.com/cszn/SCUNet> (Apache-2.0; the
//! file says so and that it was modified, see `NOTICE`): a U-Net whose blocks split their
//! channels between a Swin transformer block (8 x 8 windows, alternating regular and shifted)
//! and a residual 3 x 3 convolution. Weights: the `scunet_color_real_psnr.pth` of the authors'
//! KAIR release (see [`crate::files`]), read by [`crate::pt`].
//!
//! The network is blind (it estimates the noise itself, there is no sigma input) and returns the
//! clean image, not the noise. Input and output are NCHW, 0..1, in an sRGB-like (gamma) encoding.
//! `H` and `W` must be multiples of 64 (the three 2x downsamplings and the 8 x 8 windows);
//! [`crate::tiles`] pads accordingly.

use std::collections::HashMap;
use std::sync::Mutex;

use candle_core::{DType, Device, Tensor};
use candle_nn::ops;

use crate::{Error, Result, pt};

const WINDOW: usize = 8;
const HEAD_DIM: usize = 32;
/// Blocks per stage in `scunet_color_real_psnr` (`config=[4,4,4,4,4,4,4]`).
const BLOCKS: usize = 4;
const DIM: usize = 64;

fn t(w: &pt::Weights, name: &str, shape: &[usize], dev: &Device) -> Result<Tensor> {
    let e = w.get(name).ok_or_else(|| Error::Model(format!("missing weight {name}")))?;
    if e.shape != shape {
        return Err(Error::Model(format!("{name}: shape {:?}, expected {shape:?}", e.shape)));
    }
    Ok(Tensor::from_vec(e.data.clone(), shape.to_vec(), dev)?)
}

struct Linear {
    /// `[in, out]`
    w: Tensor,
    b: Tensor,
}

impl Linear {
    fn load(w: &pt::Weights, name: &str, inp: usize, out: usize, dev: &Device) -> Result<Self> {
        Ok(Self { w: t(w, &format!("{name}.weight"), &[out, inp], dev)?.t()?.contiguous()?, b: t(w, &format!("{name}.bias"), &[out], dev)? })
    }
    /// `x`: `[rows, in]`.
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        Ok(x.matmul(&self.w)?.broadcast_add(&self.b)?)
    }
}

struct LayerNorm {
    w: Tensor,
    b: Tensor,
}

impl LayerNorm {
    fn load(w: &pt::Weights, name: &str, c: usize, dev: &Device) -> Result<Self> {
        Ok(Self { w: t(w, &format!("{name}.weight"), &[c], dev)?, b: t(w, &format!("{name}.bias"), &[c], dev)? })
    }
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        Ok(ops::layer_norm(x, &self.w, &self.b, 1e-5)?)
    }
}

struct Conv {
    w: Tensor,
    b: Option<Tensor>,
    stride: usize,
    pad: usize,
}

impl Conv {
    fn load(w: &pt::Weights, name: &str, inp: usize, out: usize, k: usize, stride: usize, pad: usize, bias: bool, dev: &Device) -> Result<Self> {
        Ok(Self {
            w: t(w, &format!("{name}.weight"), &[out, inp, k, k], dev)?,
            b: if bias { Some(t(w, &format!("{name}.bias"), &[out], dev)?) } else { None },
            stride,
            pad,
        })
    }
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let y = x.conv2d(&self.w, self.pad, self.stride, 1, 1)?;
        match &self.b {
            Some(b) => Ok(y.broadcast_add(&b.reshape((1, (), 1, 1))?)?),
            None => Ok(y),
        }
    }
}

/// `ConvTranspose2d(inp, out, 2, 2, 0, bias=False)`.
struct Up {
    w: Tensor,
}

impl Up {
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        Ok(x.conv_transpose2d(&self.w, 0, 0, 2, 1)?)
    }
}

/// Window attention of a Swin block; `shifted` blocks roll the feature map by half a window.
struct Msa {
    qkv: Linear,
    proj: Linear,
    /// Relative position bias, `[1, heads, 64, 64]`.
    bias: Tensor,
    heads: usize,
    shifted: bool,
}

/// Shifted-window masks by window grid `(rows, cols)`: `[windows, 1, 64, 64]`, 0 or a large
/// negative number where a query and a key come from different parts of the shifted map.
type Masks = Mutex<HashMap<(usize, usize), Tensor>>;

fn shift_mask(nh: usize, nw: usize, dev: &Device) -> Result<Tensor> {
    let (p, s) = (WINDOW, WINDOW / 2);
    let n = WINDOW * WINDOW;
    let mut m = vec![0f32; nh * nw * n * n];
    for w1 in 0..nh {
        for w2 in 0..nw {
            let base = (w1 * nw + w2) * n * n;
            for q in 0..n {
                let (p1, p2) = (q / p, q % p);
                for k in 0..n {
                    let (p3, p4) = (k / p, k % p);
                    let rows = w1 == nh - 1 && ((p1 < s) != (p3 < s));
                    let cols = w2 == nw - 1 && ((p2 < s) != (p4 < s));
                    if rows || cols {
                        m[base + q * n + k] = -1e9;
                    }
                }
            }
        }
    }
    Ok(Tensor::from_vec(m, (nh * nw, 1, n, n), dev)?)
}

/// `torch.roll` by `-s` along `dim` (negative `s`: toward lower indices; positive: toward higher).
fn roll(x: &Tensor, s: isize, dim: usize) -> Result<Tensor> {
    let n = x.dim(dim)?;
    let k = s.rem_euclid(n as isize) as usize;
    if k == 0 {
        return Ok(x.clone());
    }
    // roll by +k: out = [x[n-k..], x[..n-k]]
    Ok(Tensor::cat(&[&x.narrow(dim, n - k, k)?, &x.narrow(dim, 0, n - k)?], dim)?)
}

impl Msa {
    /// `x`: `[1, H, W, C]`.
    fn forward(&self, x: &Tensor, masks: &Masks) -> Result<Tensor> {
        let (_, h, w, c) = x.dims4()?;
        let (nh, nw) = (h / WINDOW, w / WINDOW);
        let half = (WINDOW / 2) as isize;
        let x = if self.shifted { roll(&roll(x, -half, 1)?, -half, 2)? } else { x.clone() };
        // windows: [nW, 64, C]
        let win = x.reshape((1, nh, WINDOW, nw, WINDOW, c))?.permute((0, 1, 3, 2, 4, 5))?.contiguous()?.reshape((nh * nw, WINDOW * WINDOW, c))?;
        let nwin = nh * nw;
        let n = WINDOW * WINDOW;
        let qkv = self.qkv.forward(&win.reshape((nwin * n, c))?)?.reshape((nwin, n, 3 * self.heads, HEAD_DIM))?;
        let part = |i: usize| -> Result<Tensor> { Ok(qkv.narrow(2, i * self.heads, self.heads)?.permute((0, 2, 1, 3))?.contiguous()?) };
        let (q, k, v) = (part(0)?, part(1)?, part(2)?);
        let scale = (HEAD_DIM as f64).powf(-0.5);
        let mut sim = (q.matmul(&k.transpose(2, 3)?.contiguous()?)? * scale)?.broadcast_add(&self.bias)?;
        if self.shifted {
            let mask = {
                let mut g = masks.lock().map_err(|_| Error::Model("mask cache poisoned".into()))?;
                match g.get(&(nh, nw)) {
                    Some(m) => m.clone(),
                    None => {
                        let m = shift_mask(nh, nw, x.device())?;
                        g.insert((nh, nw), m.clone());
                        m
                    }
                }
            };
            sim = sim.broadcast_add(&mask)?;
        }
        let probs = ops::softmax_last_dim(&sim)?;
        let out = probs.matmul(&v)?.permute((0, 2, 1, 3))?.contiguous()?.reshape((nwin * n, c))?;
        let out = self.proj.forward(&out)?;
        let out = out.reshape((1, nh, nw, WINDOW, WINDOW, c))?.permute((0, 1, 3, 2, 4, 5))?.contiguous()?.reshape((1, h, w, c))?;
        if self.shifted { roll(&roll(&out, half, 1)?, half, 2) } else { Ok(out) }
    }
}

struct SwinBlock {
    ln1: LayerNorm,
    msa: Msa,
    ln2: LayerNorm,
    fc1: Linear,
    fc2: Linear,
}

impl SwinBlock {
    fn forward(&self, x: &Tensor, masks: &Masks) -> Result<Tensor> {
        let x = (x + self.msa.forward(&self.ln1.forward(x)?, masks)?)?;
        let (_, h, w, c) = x.dims4()?;
        let y = self.ln2.forward(&x)?.reshape((h * w, c))?;
        let y = self.fc2.forward(&self.fc1.forward(&y)?.gelu_erf()?)?.reshape((1, h, w, c))?;
        Ok((x + y)?)
    }
}

struct ConvTransBlock {
    conv_dim: usize,
    trans_dim: usize,
    conv1_1: Conv,
    conv1_2: Conv,
    rc1: Conv,
    rc2: Conv,
    trans: SwinBlock,
}

impl ConvTransBlock {
    fn load(w: &pt::Weights, name: &str, half: usize, shifted: bool, dev: &Device) -> Result<Self> {
        let (cd, td) = (half, half);
        let c = cd + td;
        let heads = td / HEAD_DIM;
        let tb = format!("{name}.trans_block");
        // The checkpoint stores the bias table as [heads, 15, 15]; expand it to [heads, 64, 64].
        let table = w.get(&format!("{tb}.msa.relative_position_params")).ok_or_else(|| Error::Model("missing relative_position_params".into()))?;
        let side = 2 * WINDOW - 1;
        if table.shape != [heads, side, side] {
            return Err(Error::Model(format!("relative_position_params: shape {:?}", table.shape)));
        }
        let n = WINDOW * WINDOW;
        let mut bias = vec![0f32; heads * n * n];
        for hd in 0..heads {
            for p in 0..n {
                for q in 0..n {
                    let di = (p / WINDOW + WINDOW - 1) - q / WINDOW;
                    let dj = (p % WINDOW + WINDOW - 1) - q % WINDOW;
                    bias[(hd * n + p) * n + q] = table.data[(hd * side + di) * side + dj];
                }
            }
        }
        Ok(Self {
            conv_dim: cd,
            trans_dim: td,
            conv1_1: Conv::load(w, &format!("{name}.conv1_1"), c, c, 1, 1, 0, true, dev)?,
            conv1_2: Conv::load(w, &format!("{name}.conv1_2"), c, c, 1, 1, 0, true, dev)?,
            rc1: Conv::load(w, &format!("{name}.conv_block.0"), cd, cd, 3, 1, 1, false, dev)?,
            rc2: Conv::load(w, &format!("{name}.conv_block.2"), cd, cd, 3, 1, 1, false, dev)?,
            trans: SwinBlock {
                ln1: LayerNorm::load(w, &format!("{tb}.ln1"), td, dev)?,
                msa: Msa {
                    qkv: Linear::load(w, &format!("{tb}.msa.embedding_layer"), td, 3 * td, dev)?,
                    proj: Linear::load(w, &format!("{tb}.msa.linear"), td, td, dev)?,
                    bias: Tensor::from_vec(bias, (1, heads, n, n), dev)?,
                    heads,
                    shifted,
                },
                ln2: LayerNorm::load(w, &format!("{tb}.ln2"), td, dev)?,
                fc1: Linear::load(w, &format!("{tb}.mlp.0"), td, 4 * td, dev)?,
                fc2: Linear::load(w, &format!("{tb}.mlp.2"), 4 * td, td, dev)?,
            },
        })
    }

    fn forward(&self, x: &Tensor, masks: &Masks) -> Result<Tensor> {
        let y = self.conv1_1.forward(x)?;
        let conv_x = y.narrow(1, 0, self.conv_dim)?;
        let trans_x = y.narrow(1, self.conv_dim, self.trans_dim)?;
        let conv_x = (self.rc2.forward(&self.rc1.forward(&conv_x)?.relu()?)? + &conv_x)?;
        let trans_x = trans_x.permute((0, 2, 3, 1))?.contiguous()?;
        let trans_x = self.trans.forward(&trans_x, masks)?.permute((0, 3, 1, 2))?.contiguous()?;
        let res = self.conv1_2.forward(&Tensor::cat(&[&conv_x, &trans_x], 1)?)?;
        Ok((x + res)?)
    }
}

/// One stage: its blocks (window type alternates W, SW, W, SW) and the resampling conv.
struct Stage {
    blocks: Vec<ConvTransBlock>,
}

impl Stage {
    fn load(w: &pt::Weights, prefix: &str, first: usize, half: usize, dev: &Device) -> Result<Self> {
        let blocks = (0..BLOCKS).map(|i| ConvTransBlock::load(w, &format!("{prefix}.{}", first + i), half, i % 2 == 1, dev)).collect::<Result<_>>()?;
        Ok(Self { blocks })
    }
    fn forward(&self, mut x: Tensor, masks: &Masks) -> Result<Tensor> {
        for b in &self.blocks {
            x = b.forward(&x, masks)?;
            // Metal queues commands and keeps every buffer they touch until they have run: wait
            // after each block so a tile's working set stays one block's, not the whole network's
            x.device().synchronize()?;
        }
        Ok(x)
    }
}

/// The `scunet_color_real_*` network (3 channels, `dim = 64`, four blocks per stage).
pub struct ScUnet {
    head: Conv,
    down: [(Stage, Conv); 3],
    body: Stage,
    up: [(Up, Stage); 3],
    tail: Conv,
    masks: Masks,
    device: Device,
}

impl ScUnet {
    pub fn load(w: &pt::Weights, dev: &Device) -> Result<Self> {
        let d = DIM;
        let down = |i: usize, half: usize, cin: usize, cout: usize| -> Result<(Stage, Conv)> {
            let p = format!("m_down{i}");
            Ok((Stage::load(w, &p, 0, half, dev)?, Conv::load(w, &format!("{p}.{BLOCKS}"), cin, cout, 2, 2, 0, false, dev)?))
        };
        let up = |i: usize, half: usize, cin: usize, cout: usize| -> Result<(Up, Stage)> {
            let p = format!("m_up{i}");
            Ok((Up { w: t(w, &format!("{p}.0.weight"), &[cin, cout, 2, 2], dev)? }, Stage::load(w, &p, 1, half, dev)?))
        };
        Ok(Self {
            head: Conv::load(w, "m_head.0", 3, d, 3, 1, 1, false, dev)?,
            down: [down(1, d / 2, d, 2 * d)?, down(2, d, 2 * d, 4 * d)?, down(3, 2 * d, 4 * d, 8 * d)?],
            body: Stage::load(w, "m_body", 0, 4 * d, dev)?,
            up: [up(3, 2 * d, 8 * d, 4 * d)?, up(2, d, 4 * d, 2 * d)?, up(1, d / 2, 2 * d, d)?],
            tail: Conv::load(w, "m_tail.0", d, 3, 3, 1, 1, false, dev)?,
            masks: Mutex::new(HashMap::new()),
            device: dev.clone(),
        })
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    /// Denoise `x` (`[1, 3, H, W]`, 0..1, H and W multiples of 64).
    pub fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let (_, _, h, w) = x.dims4()?;
        if h % 64 != 0 || w % 64 != 0 {
            return Err(Error::Model(format!("SCUNet input {w} x {h} is not a multiple of 64")));
        }
        let m = &self.masks;
        let x1 = self.head.forward(x)?;
        let x2 = {
            let (s, c) = &self.down[0];
            c.forward(&s.forward(x1.clone(), m)?)?
        };
        let x3 = {
            let (s, c) = &self.down[1];
            c.forward(&s.forward(x2.clone(), m)?)?
        };
        let x4 = {
            let (s, c) = &self.down[2];
            c.forward(&s.forward(x3.clone(), m)?)?
        };
        let mut x = self.body.forward(x4.clone(), m)?;
        for (i, skip) in [&x4, &x3, &x2].into_iter().enumerate() {
            let (u, s) = &self.up[i];
            x = s.forward(u.forward(&(&x + skip)?)?, m)?;
        }
        let y = self.tail.forward(&(&x + &x1)?)?;
        Ok(y.to_dtype(DType::F32)?)
    }
}
