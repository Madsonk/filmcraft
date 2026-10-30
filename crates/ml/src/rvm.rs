//! Robust Video Matting (Lin et al., WACV 2022; GPL-3.0 code and weights) on candle: the alpha matte of
//! the person in a video, with a recurrent state that keeps the matte stable from one frame to the next.
//!
//! **Personal use only.** The published weights are under the GPL-3.0; FilmCraft never bundles them and
//! installs them only after the licence has been accepted. This file contains no code from the reference
//! implementation: it is written from the paper (MobileNetV3-Large encoder with the last stage dilated for an
//! output stride of 16, LR-ASPP, a four-level recurrent decoder with ConvGRU on half the channels, a
//! 1x1 projection, and a deep guided filter to bring the matte back to the full resolution) and from the names
//! and shapes of the tensors of `rvm_mobilenetv3.pth`; its output was compared with the reference model.
//!
//! The network runs on the frame reduced to at most [`WORK`] pixels on its long side (the paper's
//! "downsample ratio"); the guided-filter refiner lifts the matte back to the frame's size.

use std::path::Path;

use candle_core::{DType, Device, Tensor};

use crate::{Error, Result, pick_device, read_file};

pub const WEIGHTS: &str = "rvm_mobilenetv3.pth";
/// Longest side the network works at.
pub const WORK: usize = 512;

type W = std::collections::HashMap<String, Tensor>;

fn t(w: &W, k: &str) -> Result<Tensor> {
    w.get(k).cloned().ok_or_else(|| Error::Model(format!("{WEIGHTS}: missing tensor {k}")))
}

/// A convolution with its batch norm folded in.
struct Cv {
    w: Tensor,
    b: Tensor,
    stride: usize,
    pad: usize,
    dil: usize,
    groups: usize,
}

impl Cv {
    fn plain(w: &W, name: &str, stride: usize, pad: usize, groups: usize) -> Result<Self> {
        let k = t(w, &format!("{name}.weight"))?;
        let co = k.dim(0)?;
        let b = match w.get(&format!("{name}.bias")) {
            Some(b) => b.clone(),
            None => Tensor::zeros(co, DType::F32, k.device())?,
        };
        Ok(Self { w: k, b, stride, pad, dil: 1, groups })
    }

    /// `conv` (no bias) followed by the batch norm `bn` (running statistics), folded.
    fn bn(w: &W, conv: &str, bn: &str, eps: f64, (stride, pad, dil, groups): (usize, usize, usize, usize)) -> Result<Self> {
        let k = t(w, &format!("{conv}.weight"))?;
        let co = k.dim(0)?;
        let (g, be, m, v) =
            (t(w, &format!("{bn}.weight"))?, t(w, &format!("{bn}.bias"))?, t(w, &format!("{bn}.running_mean"))?, t(w, &format!("{bn}.running_var"))?);
        let scale = (g / (v + eps)?.sqrt()?)?;
        let kw = k.broadcast_mul(&scale.reshape((co, 1, 1, 1))?)?;
        let cb = match w.get(&format!("{conv}.bias")) {
            Some(b) => b.clone(),
            None => Tensor::zeros(co, DType::F32, k.device())?,
        };
        let b = (be + ((cb - m)? * &scale)?)?;
        Ok(Self { w: kw, b, stride, pad, dil, groups })
    }

    fn fwd(&self, x: &Tensor) -> Result<Tensor> {
        let y = x.conv2d(&self.w, self.pad, self.stride, self.dil, self.groups)?;
        let c = self.b.dim(0)?;
        Ok(y.broadcast_add(&self.b.reshape((1, c, 1, 1))?)?)
    }
}

fn hardswish(x: &Tensor) -> Result<Tensor> {
    Ok((x * ((x + 3.0)?.clamp(0f32, 6f32)? / 6.0)?)?)
}

fn hardsigmoid(x: &Tensor) -> Result<Tensor> {
    Ok(((x + 3.0)?.clamp(0f32, 6f32)? / 6.0)?)
}

#[derive(Clone, Copy)]
struct Cfg {
    k: usize,
    exp: usize,
    out: usize,
    se: bool,
    hs: bool,
    stride: usize,
    dil: usize,
}

const fn c(k: usize, exp: usize, out: usize, se: bool, hs: bool, stride: usize, dil: usize) -> Cfg {
    Cfg { k, exp, out, se, hs, stride, dil }
}

/// MobileNetV3-Large blocks 1..=15 (block 13 is the dilated one: stride 1, dilation 2 in the stages after it).
const BLOCKS: [Cfg; 15] = [
    c(3, 16, 16, false, false, 1, 1),
    c(3, 64, 24, false, false, 2, 1),
    c(3, 72, 24, false, false, 1, 1),
    c(5, 72, 40, true, false, 2, 1),
    c(5, 120, 40, true, false, 1, 1),
    c(5, 120, 40, true, false, 1, 1),
    c(3, 240, 80, false, true, 2, 1),
    c(3, 200, 80, false, true, 1, 1),
    c(3, 184, 80, false, true, 1, 1),
    c(3, 184, 80, false, true, 1, 1),
    c(3, 480, 112, true, true, 1, 1),
    c(3, 672, 112, true, true, 1, 1),
    c(5, 672, 160, true, true, 1, 2),
    c(5, 960, 160, true, true, 1, 2),
    c(5, 960, 160, true, true, 1, 2),
];

struct Block {
    expand: Option<Cv>,
    dw: Cv,
    se: Option<(Cv, Cv)>,
    project: Cv,
    hs: bool,
    residual: bool,
}

impl Block {
    fn load(w: &W, idx: usize, cfg: Cfg, cin: usize) -> Result<Self> {
        const EPS: f64 = 1e-3; // torchvision's MobileNetV3 batch norms
        let p = format!("backbone.features.{idx}.block");
        let mut n = 0;
        let expand = if cfg.exp != cin {
            let e = Cv::bn(w, &format!("{p}.{n}.0"), &format!("{p}.{n}.1"), EPS, (1, 0, 1, 1))?;
            n += 1;
            Some(e)
        } else {
            None
        };
        let pad = (cfg.k - 1) / 2 * cfg.dil;
        let dw = Cv::bn(w, &format!("{p}.{n}.0"), &format!("{p}.{n}.1"), EPS, (cfg.stride, pad, cfg.dil, cfg.exp))?;
        n += 1;
        let se = if cfg.se {
            let s = (Cv::plain(w, &format!("{p}.{n}.fc1"), 1, 0, 1)?, Cv::plain(w, &format!("{p}.{n}.fc2"), 1, 0, 1)?);
            n += 1;
            Some(s)
        } else {
            None
        };
        let project = Cv::bn(w, &format!("{p}.{n}.0"), &format!("{p}.{n}.1"), EPS, (1, 0, 1, 1))?;
        Ok(Self { expand, dw, se, project, hs: cfg.hs, residual: cfg.stride == 1 && cin == cfg.out })
    }

    fn act(&self, x: &Tensor) -> Result<Tensor> {
        if self.hs { hardswish(x) } else { Ok(x.relu()?) }
    }

    fn fwd(&self, x: &Tensor) -> Result<Tensor> {
        let mut y = x.clone();
        if let Some(e) = &self.expand {
            y = self.act(&e.fwd(&y)?)?;
        }
        y = self.act(&self.dw.fwd(&y)?)?;
        if let Some((a, b)) = &self.se {
            let s = y.mean_keepdim(2)?.mean_keepdim(3)?;
            let s = hardsigmoid(&b.fwd(&a.fwd(&s)?.relu()?)?)?;
            y = y.broadcast_mul(&s)?;
        }
        y = self.project.fwd(&y)?;
        if self.residual { Ok((y + x)?) } else { Ok(y) }
    }
}

struct Gru {
    ih: Cv,
    hh: Cv,
    ch: usize,
}

impl Gru {
    fn load(w: &W, p: &str) -> Result<Self> {
        let ih = Cv::plain(w, &format!("{p}.gru.ih.0"), 1, 1, 1)?;
        let ch = ih.w.dim(0)? / 2;
        Ok(Self { ih, hh: Cv::plain(w, &format!("{p}.gru.hh.0"), 1, 1, 1)?, ch })
    }

    fn fwd(&self, x: &Tensor, h: &Tensor) -> Result<Tensor> {
        let g = candle_nn::ops::sigmoid(&self.ih.fwd(&Tensor::cat(&[x, h], 1)?)?)?;
        let (r, z) = (g.narrow(1, 0, self.ch)?, g.narrow(1, self.ch, self.ch)?);
        let cand = self.hh.fwd(&Tensor::cat(&[x, &(r * h)?], 1)?)?.tanh()?;
        Ok((((Tensor::ones_like(&z)? - &z)? * h)? + (z * cand)?)?)
    }
}

/// Recurrent state of the four decoder levels (None before the first frame).
#[derive(Default)]
pub struct State {
    r: [Option<Tensor>; 4],
}

pub struct Rvm {
    stem: Cv,
    blocks: Vec<Block>,
    last: Cv,
    aspp1: Cv,
    aspp2: Cv,
    gru4: Gru,
    up: [(Cv, Gru); 3],
    out0: (Cv, Cv),
    proj: Cv,
    ref_conv: [Cv; 3],
    ref_box: Tensor,
    dev: Device,
}

fn up2(x: &Tensor) -> Result<Tensor> {
    let (_, _, h, w) = x.dims4()?;
    Ok(x.upsample_bilinear2d(h * 2, w * 2, false)?)
}

fn crop(x: &Tensor, h: usize, w: usize) -> Result<Tensor> {
    Ok(x.narrow(2, 0, h)?.narrow(3, 0, w)?)
}

impl Rvm {
    pub fn load(dir: &Path) -> Result<Self> {
        let dev = pick_device();
        let p = dir.join(WEIGHTS);
        let raw = filmcraft_mlrestore::pt::read(&read_file(&p)?).map_err(|e| Error::Model(format!("{}: {e}", p.display())))?;
        let mut w = W::new();
        for (k, v) in raw {
            w.insert(k, Tensor::from_vec(v.data, v.shape, &dev)?);
        }
        let stem = Cv::bn(&w, "backbone.features.0.0", "backbone.features.0.1", 1e-3, (2, 1, 1, 1))?;
        let mut blocks = Vec::new();
        let mut cin = 16;
        for (i, cfg) in BLOCKS.iter().enumerate() {
            blocks.push(Block::load(&w, i + 1, *cfg, cin)?);
            cin = cfg.out;
        }
        let last = Cv::bn(&w, "backbone.features.16.0", "backbone.features.16.1", 1e-3, (1, 0, 1, 1))?;
        let (aspp1, aspp2) = (Cv::bn(&w, "aspp.aspp1.0", "aspp.aspp1.1", 1e-5, (1, 0, 1, 1))?, Cv::plain(&w, "aspp.aspp2.1", 1, 0, 1)?);
        let up = [3, 2, 1].map(|i| -> Result<(Cv, Gru)> {
            Ok((
                Cv::bn(&w, &format!("decoder.decode{i}.conv.0"), &format!("decoder.decode{i}.conv.1"), 1e-5, (1, 1, 1, 1))?,
                Gru::load(&w, &format!("decoder.decode{i}"))?,
            ))
        });
        let [u3, u2, u1] = up;
        let out0 = (
            Cv::bn(&w, "decoder.decode0.conv.0", "decoder.decode0.conv.1", 1e-5, (1, 1, 1, 1))?,
            Cv::bn(&w, "decoder.decode0.conv.3", "decoder.decode0.conv.4", 1e-5, (1, 1, 1, 1))?,
        );
        let ref_conv = [
            Cv::bn(&w, "refiner.conv.0", "refiner.conv.1", 1e-5, (1, 0, 1, 1))?,
            Cv::bn(&w, "refiner.conv.3", "refiner.conv.4", 1e-5, (1, 0, 1, 1))?,
            Cv::plain(&w, "refiner.conv.6", 1, 0, 1)?,
        ];
        Ok(Self {
            stem,
            blocks,
            last,
            aspp1,
            aspp2,
            gru4: Gru::load(&w, "decoder.decode4")?,
            up: [u3?, u2?, u1?],
            out0,
            proj: Cv::plain(&w, "project_mat.conv", 1, 0, 1)?,
            ref_conv,
            ref_box: t(&w, "refiner.box_filter.weight")?,
            dev,
        })
    }

    pub fn device(&self) -> &Device {
        &self.dev
    }

    fn box_filter(&self, x: &Tensor) -> Result<Tensor> {
        Ok(x.conv2d(&self.ref_box, 1, 1, 1, 4)?)
    }

    /// One frame (interleaved RGB 0..1, `h` x `w`) -> (foreground RGB, alpha) both full-size, and the next state.
    /// The frame must be at most [`WORK`] x [`WORK`] after reduction by `ratio` (see [`Rvm::ratio`]).
    pub fn frame(&self, rgb: &[f32], h: usize, w: usize, ratio: f64, state: &mut State) -> Result<(Vec<f32>, Vec<f32>)> {
        let src = Tensor::from_vec(rgb.to_vec(), (1, h, w, 3), &self.dev)?.permute((0, 3, 1, 2))?.contiguous()?;
        let (hs, ws) = (((h as f64 * ratio).round() as usize).max(16), ((w as f64 * ratio).round() as usize).max(16));
        let small = if ratio < 1.0 { src.upsample_bilinear2d(hs, ws, false)? } else { src.clone() };
        // work on a canvas whose sides are multiples of 16 (edge-replicated)
        let (hp, wp) = (hs.div_ceil(16) * 16, ws.div_ceil(16) * 16);
        let sp = if hp > hs { small.pad_with_same(2, 0, hp - hs)? } else { small.clone() };
        let sp = if wp > ws { sp.pad_with_same(3, 0, wp - ws)? } else { sp };
        let mean = Tensor::from_vec(vec![0.485f32, 0.456, 0.406], (1, 3, 1, 1), &self.dev)?;
        let std = Tensor::from_vec(vec![0.229f32, 0.224, 0.225], (1, 3, 1, 1), &self.dev)?;
        let x = sp.broadcast_sub(&mean)?.broadcast_div(&std)?;
        // encoder
        let mut y = hardswish(&self.stem.fwd(&x)?)?;
        let (mut f1, mut f2, mut f3) = (None, None, None);
        for (i, b) in self.blocks.iter().enumerate() {
            y = b.fwd(&y)?;
            match i + 1 {
                1 => f1 = Some(y.clone()),
                3 => f2 = Some(y.clone()),
                6 => f3 = Some(y.clone()),
                _ => {}
            }
        }
        let f4 = hardswish(&self.last.fwd(&y)?)?;
        let enc = || Error::Model("encoder".into());
        let (f1, f2, f3) = (f1.ok_or_else(enc)?, f2.ok_or_else(enc)?, f3.ok_or_else(enc)?);
        // LR-ASPP
        let a1 = self.aspp1.fwd(&f4)?.relu()?;
        let a2 = candle_nn::ops::sigmoid(&self.aspp2.fwd(&f4.mean_keepdim(2)?.mean_keepdim(3)?)?)?;
        let f4 = a1.broadcast_mul(&a2)?;
        // decoder
        let s1 = sp.avg_pool2d(2)?;
        let s2 = s1.avg_pool2d(2)?;
        let s3 = s2.avg_pool2d(2)?;
        let zeros = |c: usize, hh: usize, ww: usize| -> Result<Tensor> { Ok(Tensor::zeros((1, c, hh, ww), DType::F32, &self.dev)?) };
        let split_gru = |x: &Tensor, g: &Gru, r: &Option<Tensor>| -> Result<(Tensor, Tensor)> {
            let c = x.dim(1)?;
            let (a, b) = (x.narrow(1, 0, c / 2)?, x.narrow(1, c / 2, c - c / 2)?);
            let h = match r {
                Some(h) if h.dims4()?.2 == b.dims4()?.2 && h.dims4()?.3 == b.dims4()?.3 => h.clone(),
                _ => zeros(g.ch, b.dim(2)?, b.dim(3)?)?,
            };
            let nb = g.fwd(&b, &h)?;
            Ok((Tensor::cat(&[&a, &nb], 1)?, nb))
        };
        let (x4, r4) = split_gru(&f4, &self.gru4, &state.r[3])?;
        let mut x = x4;
        state.r[3] = Some(r4);
        let skips = [(&f3, &s3), (&f2, &s2), (&f1, &s1)];
        for (lvl, ((f, s), (cv, g))) in skips.iter().zip(self.up.iter()).enumerate() {
            let u = crop(&up2(&x)?, f.dim(2)?, f.dim(3)?)?;
            let cat = Tensor::cat(&[&u, *f, *s], 1)?;
            let z = cv.fwd(&cat)?.relu()?;
            let (nx, nr) = split_gru(&z, g, &state.r[2 - lvl])?;
            x = nx;
            state.r[2 - lvl] = Some(nr);
        }
        let u = crop(&up2(&x)?, sp.dim(2)?, sp.dim(3)?)?;
        let hid = self.out0.1.fwd(&self.out0.0.fwd(&Tensor::cat(&[&u, &sp], 1)?)?.relu()?)?.relu()?;
        let mat = self.proj.fwd(&hid)?;
        let (fgr_res, pha) = (mat.narrow(1, 0, 3)?, mat.narrow(1, 3, 1)?);
        // crop the canvas back, then refine at full size when the network ran on a reduced frame
        let (fgr_res, pha, hid) = (crop(&fgr_res, hs, ws)?, crop(&pha, hs, ws)?, crop(&hid, hs, ws)?);
        let (fgr_res, pha) = if ratio < 1.0 { self.refine(&src, &small, &fgr_res, &pha, &hid)? } else { (fgr_res, pha) };
        let fgr = (fgr_res + &src)?.clamp(0f32, 1f32)?;
        let pha = pha.clamp(0f32, 1f32)?;
        let fgr = fgr.permute((0, 2, 3, 1))?.contiguous()?.flatten_all()?.to_vec1::<f32>()?;
        Ok((fgr, pha.flatten_all()?.to_vec1::<f32>()?))
    }

    /// Deep guided filter: a per-pixel affine map (A, b) learned at the reduced size is applied to the full-size frame.
    fn refine(&self, fine: &Tensor, base: &Tensor, base_fgr: &Tensor, base_pha: &Tensor, hid: &Tensor) -> Result<(Tensor, Tensor)> {
        let aug = |s: &Tensor| -> Result<Tensor> { Ok(Tensor::cat(&[s, &s.mean_keepdim(1)?], 1)?) };
        let (fx, bx) = (aug(fine)?, aug(base)?);
        let by = Tensor::cat(&[base_fgr, base_pha], 1)?;
        let (mx, my) = (self.box_filter(&bx)?, self.box_filter(&by)?);
        let cov = (self.box_filter(&(&bx * &by)?)? - (&mx * &my)?)?;
        let var = (self.box_filter(&(&bx * &bx)?)? - (&mx * &mx)?)?;
        let mut a = Tensor::cat(&[&cov, &var, hid], 1)?;
        a = self.ref_conv[0].fwd(&a)?.relu()?;
        a = self.ref_conv[1].fwd(&a)?.relu()?;
        a = self.ref_conv[2].fwd(&a)?;
        let b = (my - (&a * &mx)?)?;
        let (h, w) = (fine.dim(2)?, fine.dim(3)?);
        let (a, b) = (a.upsample_bilinear2d(h, w, false)?, b.upsample_bilinear2d(h, w, false)?);
        let out = ((a * fx)? + b)?;
        Ok((out.narrow(1, 0, 3)?, out.narrow(1, 3, 1)?))
    }

    /// The reduction used for a `w` x `h` frame: its long side brought to [`WORK`] pixels (never enlarged).
    pub fn ratio(w: usize, h: usize) -> f64 {
        (WORK as f64 / w.max(h) as f64).min(1.0)
    }
}
