//! Hybrid Transformer Demucs (Rouard, Massa, Défossez, ICASSP 2023; MIT) on candle: splits a stereo
//! mix into drums, bass, other and vocals.
//!
//! A port to Rust of `demucs/htdemucs.py`, `hdemucs.py`, `demucs.py` and `transformer.py` (MIT,
//! (c) Meta Platforms, Inc.) for the released checkpoint `955717e8` (`htdemucs`, the 4-source model):
//! a waveform U-Net and a spectrogram U-Net (4096-point STFT, complex-as-channels) whose bottoms
//! meet in a 5-layer cross-domain Transformer with 2-D / 1-D sinusoidal positions. The weights are
//! read straight from the official `.th` file (a PyTorch pickle) by [`filmcraft_mlrestore::pt`].
//!
//! [`Htdemucs::separate`] does what `demucs.apply.apply_model(split=True, overlap=0.25, shifts=0)` does
//! on a stereo signal at 44.1 kHz, including the input normalisation of `demucs.separate`.

use std::collections::HashMap;
use std::path::Path;

use candle_core::{D, DType, Device, Tensor};

use crate::{Error, Result, pick_device, read_file};

pub const SAMPLE_RATE: u32 = 44_100;
pub const SOURCES: [&str; 4] = ["drums", "bass", "other", "vocals"];
/// The official checkpoint (file name of `dl.fbaipublicfiles.com/demucs/hybrid_transformer/`).
pub const WEIGHTS: &str = "955717e8-8726e21a.th";
const SEGMENT: usize = 343_980; // 7.8 s
const NFFT: usize = 4096;
const HOP: usize = 1024;
const DEPTH: usize = 4;
const EPS: f64 = 1e-5;

type W = HashMap<String, Tensor>;

fn t(w: &W, k: &str) -> Result<Tensor> {
    w.get(k).cloned().ok_or_else(|| Error::Model(format!("{WEIGHTS}: missing tensor {k}")))
}

// ------------------------------------------------------------------------------------------- ops

fn conv1d(x: &Tensor, w: &W, p: &str, pad: usize, stride: usize, dil: usize) -> Result<Tensor> {
    let y = x.conv1d(&t(w, &format!("{p}.weight"))?, pad, stride, dil, 1)?;
    let b = t(w, &format!("{p}.bias"))?;
    let c = b.dim(0)?;
    Ok(y.broadcast_add(&b.reshape((1, c, 1))?)?)
}

/// `GroupNorm(1, C)` on (B, C, T) with affine parameters.
fn group_norm1(x: &Tensor, w: &W, p: &str) -> Result<Tensor> {
    let (_, c, _) = x.dims3()?;
    let n = x.mean_keepdim(D::Minus1)?.mean_keepdim(1)?;
    let xc = x.broadcast_sub(&n)?;
    let var = xc.sqr()?.mean_keepdim(D::Minus1)?.mean_keepdim(1)?;
    let y = xc.broadcast_div(&(var + EPS)?.sqrt()?)?;
    let (g, b) = (t(w, &format!("{p}.weight"))?.reshape((1, c, 1))?, t(w, &format!("{p}.bias"))?.reshape((1, c, 1))?);
    Ok(y.broadcast_mul(&g)?.broadcast_add(&b)?)
}

/// `LayerNorm` over the last dimension.
fn layer_norm(x: &Tensor, w: &W, p: &str) -> Result<Tensor> {
    let m = x.mean_keepdim(D::Minus1)?;
    let xc = x.broadcast_sub(&m)?;
    let var = xc.sqr()?.mean_keepdim(D::Minus1)?;
    let y = xc.broadcast_div(&(var + EPS)?.sqrt()?)?;
    Ok(y.broadcast_mul(&t(w, &format!("{p}.weight"))?)?.broadcast_add(&t(w, &format!("{p}.bias"))?)?)
}

fn glu1(x: &Tensor) -> Result<Tensor> {
    let c = x.dim(1)? / 2;
    Ok((x.narrow(1, 0, c)? * candle_nn::ops::sigmoid(&x.narrow(1, c, c)?)?)?)
}

fn linear(x: &Tensor, w: &W, p: &str) -> Result<Tensor> {
    Ok(x.broadcast_matmul(&t(w, &format!("{p}.weight"))?.t()?)?.broadcast_add(&t(w, &format!("{p}.bias"))?)?)
}

/// `DConv`: residual dilated-convolution branch on (B', C, T).
fn dconv(x: &Tensor, w: &W, p: &str) -> Result<Tensor> {
    let mut x = x.clone();
    for d in 0..2 {
        let l = format!("{p}.layers.{d}");
        let dil = 1usize << d;
        let y = conv1d(&x, w, &format!("{l}.0"), dil, 1, dil)?;
        let y = group_norm1(&y, w, &format!("{l}.1"))?.gelu_erf()?;
        let y = conv1d(&y, w, &format!("{l}.3"), 0, 1, 1)?;
        let y = glu1(&group_norm1(&y, w, &format!("{l}.4"))?)?;
        let s = t(w, &format!("{l}.6.scale"))?;
        let c = s.dim(0)?;
        x = (x + y.broadcast_mul(&s.reshape((1, c, 1))?)?)?;
    }
    Ok(x)
}

/// (B, C, F, T) convolution with a (k, 1) kernel over F: stride `stride`, padding `pad`.
fn conv_f(x: &Tensor, w: &W, p: &str, pad: usize, stride: usize) -> Result<Tensor> {
    let (b, c, f, tt) = x.dims4()?;
    let xt = x.permute((0, 3, 1, 2))?.contiguous()?.reshape((b * tt, c, f))?;
    let k = t(w, &format!("{p}.weight"))?.squeeze(3)?;
    let y = xt.conv1d(&k, pad, stride, 1, 1)?;
    let bias = t(w, &format!("{p}.bias"))?;
    let (co, f2) = (y.dim(1)?, y.dim(2)?);
    let y = y.broadcast_add(&bias.reshape((1, co, 1))?)?;
    Ok(y.reshape((b, tt, co, f2))?.permute((0, 2, 3, 1))?.contiguous()?)
}

/// (B, C, F, T) transposed convolution with a (k, 1) kernel over F.
fn convtr_f(x: &Tensor, w: &W, p: &str, stride: usize) -> Result<Tensor> {
    let (b, c, f, tt) = x.dims4()?;
    let xt = x.permute((0, 3, 1, 2))?.contiguous()?.reshape((b * tt, c, f))?;
    let k = t(w, &format!("{p}.weight"))?.squeeze(3)?;
    let y = xt.conv_transpose1d(&k, 0, 0, stride, 1, 1)?;
    let bias = t(w, &format!("{p}.bias"))?;
    let (co, f2) = (y.dim(1)?, y.dim(2)?);
    let y = y.broadcast_add(&bias.reshape((1, co, 1))?)?;
    Ok(y.reshape((b, tt, co, f2))?.permute((0, 2, 3, 1))?.contiguous()?)
}

/// DConv applied to each frequency bin: (B, C, F, T) -> same.
fn dconv_f(y: &Tensor, w: &W, p: &str) -> Result<Tensor> {
    let (b, c, f, tt) = y.dims4()?;
    let z = y.permute((0, 2, 1, 3))?.contiguous()?.reshape((b * f, c, tt))?;
    let z = dconv(&z, w, p)?;
    Ok(z.reshape((b, f, c, tt))?.permute((0, 2, 1, 3))?.contiguous()?)
}

fn conv2d_1x1_or_3x3(x: &Tensor, w: &W, p: &str, pad: usize) -> Result<Tensor> {
    let y = x.conv2d(&t(w, &format!("{p}.weight"))?, pad, 1, 1, 1)?;
    let b = t(w, &format!("{p}.bias"))?;
    let c = b.dim(0)?;
    Ok(y.broadcast_add(&b.reshape((1, c, 1, 1))?)?)
}

// ------------------------------------------------------------------------------------ the layers

fn henc_freq(x: &Tensor, w: &W, i: usize) -> Result<Tensor> {
    let p = format!("encoder.{i}");
    let y = conv_f(x, w, &format!("{p}.conv"), 2, 4)?.gelu_erf()?;
    let y = dconv_f(&y, w, &format!("{p}.dconv"))?;
    glu1(&conv2d_1x1_or_3x3(&y, w, &format!("{p}.rewrite"), 0)?)
}

fn henc_time(x: &Tensor, w: &W, i: usize) -> Result<Tensor> {
    let p = format!("tencoder.{i}");
    let le = x.dim(D::Minus1)?;
    let x = if le % 4 != 0 { x.pad_with_zeros(D::Minus1, 0, 4 - le % 4)? } else { x.clone() };
    let y = conv1d(&x, w, &format!("{p}.conv"), 2, 4, 1)?.gelu_erf()?;
    let y = dconv(&y, w, &format!("{p}.dconv"))?;
    glu1(&conv1d(&y, w, &format!("{p}.rewrite"), 0, 1, 1)?)
}

fn hdec_freq(x: &Tensor, skip: &Tensor, w: &W, j: usize, last: bool) -> Result<(Tensor, Tensor)> {
    let p = format!("decoder.{j}");
    let x = (x + skip)?;
    let y = glu1(&conv2d_1x1_or_3x3(&x, w, &format!("{p}.rewrite"), 1)?)?;
    let y = dconv_f(&y, w, &format!("{p}.dconv"))?;
    let z = convtr_f(&y, w, &format!("{p}.conv_tr"), 4)?;
    let f = z.dim(2)?;
    let z = z.narrow(2, 2, f - 4)?;
    Ok((if last { z } else { z.gelu_erf()? }, y))
}

fn hdec_time(x: &Tensor, skip: &Tensor, w: &W, j: usize, last: bool, length: usize) -> Result<Tensor> {
    let p = format!("tdecoder.{j}");
    let x = (x + skip)?;
    let y = glu1(&conv1d(&x, w, &format!("{p}.rewrite"), 1, 1, 1)?)?;
    let y = dconv(&y, w, &format!("{p}.dconv"))?;
    let z = y.conv_transpose1d(&t(w, &format!("{p}.conv_tr.weight"))?, 0, 0, 4, 1, 1)?;
    let b = t(w, &format!("{p}.conv_tr.bias"))?;
    let c = b.dim(0)?;
    let z = z.broadcast_add(&b.reshape((1, c, 1))?)?;
    let z = z.narrow(D::Minus1, 2, length)?;
    Ok(if last { z } else { z.gelu_erf()? })
}

// -------------------------------------------------------------------------------- the transformer

fn sin_embedding_1d(len: usize, dim: usize, dev: &Device) -> Result<Tensor> {
    let half = dim / 2;
    let mut v = vec![0f32; len * dim];
    for pos in 0..len {
        for a in 0..half {
            let phase = pos as f64 / 10000f64.powf(a as f64 / (half - 1) as f64);
            v[pos * dim + a] = phase.cos() as f32;
            v[pos * dim + half + a] = phase.sin() as f32;
        }
    }
    Ok(Tensor::from_vec(v, (1, len, dim), dev)?)
}

/// (1, width*height, d_model) in the order the model flattens (t1 fr).
fn sin_embedding_2d(d_model: usize, height: usize, width: usize, dev: &Device) -> Result<Tensor> {
    let half = d_model / 2;
    let div: Vec<f64> = (0..half / 2).map(|i| (-(2.0 * i as f64) * (10000f64.ln() / half as f64)).exp()).collect();
    let mut v = vec![0f32; width * height * d_model];
    for w_ in 0..width {
        for h in 0..height {
            let row = (w_ * height + h) * d_model;
            for (i, d) in div.iter().enumerate() {
                v[row + 2 * i] = (w_ as f64 * d).sin() as f32;
                v[row + 2 * i + 1] = (w_ as f64 * d).cos() as f32;
                v[row + half + 2 * i] = (h as f64 * d).sin() as f32;
                v[row + half + 2 * i + 1] = (h as f64 * d).cos() as f32;
            }
        }
    }
    Ok(Tensor::from_vec(v, (1, width * height, d_model), dev)?)
}

/// `nn.MultiheadAttention` (batch first, 8 heads) with queries from `q`, keys and values from `kv`.
fn mha(q: &Tensor, kv: &Tensor, w: &W, p: &str) -> Result<Tensor> {
    let (b, n, d) = q.dims3()?;
    let m = kv.dim(1)?;
    let h = 8;
    let hd = d / h;
    let wi = t(w, &format!("{p}.in_proj_weight"))?;
    let bi = t(w, &format!("{p}.in_proj_bias"))?;
    let proj = |x: &Tensor, i: usize| -> Result<Tensor> {
        let wt = wi.narrow(0, i * d, d)?.t()?;
        Ok(x.broadcast_matmul(&wt)?.broadcast_add(&bi.narrow(0, i * d, d)?)?)
    };
    let split = |x: Tensor, len: usize| -> Result<Tensor> { Ok(x.reshape((b, len, h, hd))?.permute((0, 2, 1, 3))?.contiguous()?.reshape((b * h, len, hd))?) };
    let (qq, kk, vv) = (split(proj(q, 0)?, n)?, split(proj(kv, 1)?, m)?, split(proj(kv, 2)?, m)?);
    let scale = (hd as f64).powf(-0.5);
    let bh = b * h;
    let per = ((32usize << 20) / (n * m).max(1)).clamp(1, bh);
    let mut outs = Vec::new();
    let mut s = 0;
    while s < bh {
        let c = per.min(bh - s);
        let a = candle_nn::ops::softmax_last_dim(&(qq.narrow(0, s, c)?.matmul(&kk.narrow(0, s, c)?.transpose(1, 2)?)? * scale)?)?;
        outs.push(a.matmul(&vv.narrow(0, s, c)?)?);
        s += c;
    }
    let o = Tensor::cat(&outs, 0)?.reshape((b, h, n, hd))?.permute((0, 2, 1, 3))?.contiguous()?.reshape((b, n, d))?;
    linear(&o, w, &format!("{p}.out_proj"))
}

/// `MyGroupNorm(1, C)` on (B, T, C): normalise over (T, C) jointly.
fn group_norm_btc(x: &Tensor, w: &W, p: &str) -> Result<Tensor> {
    let n = x.mean_keepdim(D::Minus1)?.mean_keepdim(1)?;
    let xc = x.broadcast_sub(&n)?;
    let var = xc.sqr()?.mean_keepdim(D::Minus1)?.mean_keepdim(1)?;
    let y = xc.broadcast_div(&(var + EPS)?.sqrt()?)?;
    Ok(y.broadcast_mul(&t(w, &format!("{p}.weight"))?)?.broadcast_add(&t(w, &format!("{p}.bias"))?)?)
}

fn ff(x: &Tensor, w: &W, p: &str) -> Result<Tensor> {
    linear(&linear(x, w, &format!("{p}.linear1"))?.gelu_erf()?, w, &format!("{p}.linear2"))
}

fn scale_cl(x: &Tensor, w: &W, p: &str) -> Result<Tensor> {
    Ok(x.broadcast_mul(&t(w, &format!("{p}.scale"))?)?)
}

/// A self-attention layer (`MyTransformerEncoderLayer`, norm first, layer scale, norm out).
fn self_layer(x: &Tensor, w: &W, p: &str) -> Result<Tensor> {
    let n1 = layer_norm(x, w, &format!("{p}.norm1"))?;
    let mut x = (x + scale_cl(&mha(&n1, &n1, w, &format!("{p}.self_attn"))?, w, &format!("{p}.gamma_1"))?)?;
    let n2 = layer_norm(&x, w, &format!("{p}.norm2"))?;
    x = (&x + scale_cl(&ff(&n2, w, p)?, w, &format!("{p}.gamma_2"))?)?;
    group_norm_btc(&x, w, &format!("{p}.norm_out"))
}

/// A cross-attention layer: queries from `q`, keys and values from `k`.
fn cross_layer(q: &Tensor, k: &Tensor, w: &W, p: &str) -> Result<Tensor> {
    let (nq, nk) = (layer_norm(q, w, &format!("{p}.norm1"))?, layer_norm(k, w, &format!("{p}.norm2"))?);
    let mut x = (q + scale_cl(&mha(&nq, &nk, w, &format!("{p}.cross_attn"))?, w, &format!("{p}.gamma_1"))?)?;
    let n3 = layer_norm(&x, w, &format!("{p}.norm3"))?;
    x = (&x + scale_cl(&ff(&n3, w, p)?, w, &format!("{p}.gamma_2"))?)?;
    group_norm_btc(&x, w, &format!("{p}.norm_out"))
}

// ------------------------------------------------------------------------------------------ STFT

fn fft(re: &mut [f64], im: &mut [f64], tw: &[(f64, f64)], inverse: bool) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let step = n / len;
        for s in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (c, sn) = tw[k * step];
                let sn = if inverse { -sn } else { sn };
                let (a, b) = (s + k, s + k + len / 2);
                let (tr, ti) = (re[b] * c - im[b] * sn, re[b] * sn + im[b] * c);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
}

struct Stft {
    tw: Vec<(f64, f64)>,
    win: Vec<f64>,
}

impl Stft {
    fn new() -> Self {
        let tw = (0..NFFT / 2)
            .map(|k| ((-2.0 * std::f64::consts::PI * k as f64 / NFFT as f64).cos(), (-2.0 * std::f64::consts::PI * k as f64 / NFFT as f64).sin()))
            .collect();
        let win = (0..NFFT).map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / NFFT as f64).cos()).collect();
        Self { tw, win }
    }

    /// `_spec`: (re, im) of shape frames(336) x bins(2048) for one channel, laid out [bin][frame].
    fn spec(&self, x: &[f32]) -> (Vec<f32>, Vec<f32>, usize) {
        let l = x.len();
        let le = l.div_ceil(HOP);
        let pad = HOP / 2 * 3;
        let total = pad + l + (pad + le * HOP - l);
        let refl = |i: isize, n: isize| -> usize {
            let mut k = i;
            if k < 0 {
                k = -k;
            }
            if k > n - 1 {
                k = 2 * (n - 1) - k;
            }
            k.clamp(0, n - 1) as usize
        };
        // x padded by reflection (pad, pad + le*hop - l), then centre-padded by reflection (n_fft / 2)
        let padded = |i: isize| -> f32 { x[refl(i - pad as isize, l as isize)] };
        let frames = 1 + total / HOP;
        let bins = NFFT / 2;
        let mut re_o = vec![0f32; bins * le];
        let mut im_o = vec![0f32; bins * le];
        let norm = 1.0 / (NFFT as f64).sqrt();
        use rayon::prelude::*;
        let cols: Vec<(Vec<f32>, Vec<f32>)> = (2..2 + le)
            .into_par_iter()
            .map(|f| {
                let mut re = vec![0f64; NFFT];
                let mut im = vec![0f64; NFFT];
                for i in 0..NFFT {
                    let p = (f * HOP + i) as isize - (NFFT / 2) as isize;
                    re[i] = padded(refl(p, total as isize) as isize) as f64 * self.win[i];
                }
                fft(&mut re, &mut im, &self.tw, false);
                (re[..bins].iter().map(|v| (v * norm) as f32).collect(), im[..bins].iter().map(|v| (v * norm) as f32).collect())
            })
            .collect();
        let _ = frames;
        for (f, (r, i)) in cols.iter().enumerate() {
            for b in 0..bins {
                re_o[b * le + f] = r[b];
                im_o[b * le + f] = i[b];
            }
        }
        (re_o, im_o, le)
    }

    /// `_ispec` for one signal given [bin][frame] real/imag parts (2048 bins x `le` frames).
    fn ispec(&self, re_in: &[f32], im_in: &[f32], le: usize, length: usize) -> Vec<f32> {
        let bins = NFFT / 2;
        let pad = HOP / 2 * 3;
        let le_out = HOP * length.div_ceil(HOP) + 2 * pad;
        let frames = le + 4; // two empty frames each side
        let mut ola = vec![0f64; NFFT + (frames - 1) * HOP];
        let mut env = vec![0f64; ola.len()];
        let norm = 1.0 / (NFFT as f64).sqrt();
        for f in 0..frames {
            let mut re = vec![0f64; NFFT];
            let mut im = vec![0f64; NFFT];
            if f >= 2 && f < 2 + le {
                let c = f - 2;
                for b in 0..bins {
                    let (r, i) = (re_in[b * le + c] as f64, im_in[b * le + c] as f64);
                    re[b] = r;
                    im[b] = i;
                    if b > 0 {
                        re[NFFT - b] = r;
                        im[NFFT - b] = -i;
                    }
                }
            }
            fft(&mut re, &mut im, &self.tw, true);
            for n in 0..NFFT {
                ola[f * HOP + n] += re[n] * norm * self.win[n];
                env[f * HOP + n] += self.win[n] * self.win[n];
            }
        }
        let start = NFFT / 2;
        let mut y = vec![0f32; le_out];
        for (i, o) in y.iter_mut().enumerate() {
            let k = start + i;
            if k < ola.len() && env[k] > 1e-11 {
                *o = (ola[k] / env[k]) as f32;
            }
        }
        y[pad..pad + length].to_vec()
    }
}

// ------------------------------------------------------------------------------------- the model

pub struct Htdemucs {
    w: W,
    stft: Stft,
    dev: Device,
}

fn std_unbiased(x: &Tensor) -> Result<f64> {
    let n = x.elem_count() as f64;
    let m = x.mean_all()?.to_scalar::<f32>()? as f64;
    let ss = (x - m)?.sqr()?.sum_all()?.to_scalar::<f32>()? as f64;
    Ok((ss / (n - 1.0)).sqrt())
}

impl Htdemucs {
    /// `dir` holds [`WEIGHTS`].
    pub fn load(dir: &Path) -> Result<Self> {
        let dev = pick_device();
        let p = dir.join(WEIGHTS);
        let raw = filmcraft_mlrestore::pt::read(&read_file(&p)?).map_err(|e| Error::Model(format!("{}: {e}", p.display())))?;
        let mut w = W::new();
        for (k, v) in raw {
            w.insert(k, Tensor::from_vec(v.data, v.shape, &dev)?);
        }
        if !w.contains_key("encoder.0.conv.weight") {
            return Err(Error::Model(format!("{}: not the htdemucs checkpoint", p.display())));
        }
        Ok(Self { w, stft: Stft::new(), dev })
    }

    pub fn device(&self) -> &Device {
        &self.dev
    }

    /// One 7.8 s segment, (2, 343980) stereo (already normalised) -> (4, 2, 343980).
    pub fn forward(&self, mix: &[Vec<f32>; 2]) -> Result<Vec<Vec<Vec<f32>>>> {
        let w = &self.w;
        let length = SEGMENT;
        // spectrogram, complex as channels: [ch0 re, ch0 im, ch1 re, ch1 im]
        let mut planes: Vec<f32> = Vec::new();
        let mut le = 0;
        let mut zs = Vec::new();
        for ch in mix {
            let (re, im, l) = self.stft.spec(ch);
            le = l;
            zs.push((re, im));
        }
        for (re, im) in &zs {
            planes.extend_from_slice(re);
            planes.extend_from_slice(im);
        }
        let bins = NFFT / 2;
        let x = Tensor::from_vec(planes, (1, 4, bins, le), &self.dev)?;
        let (mean, std) = (x.mean_all()?.to_scalar::<f32>()? as f64, std_unbiased(&x)?);
        let x = ((x - mean)? / (EPS + std))?;
        let xt = Tensor::from_vec(mix.iter().flatten().copied().collect::<Vec<f32>>(), (1, 2, length), &self.dev)?;
        let (meant, stdt) = (xt.mean_all()?.to_scalar::<f32>()? as f64, std_unbiased(&xt)?);
        let xt = ((xt - meant)? / (EPS + stdt))?;

        let (mut x, mut xt) = (x, xt);
        let (mut saved, mut saved_t, mut lengths, mut lengths_t) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for i in 0..DEPTH {
            lengths.push(x.dim(D::Minus1)?);
            lengths_t.push(xt.dim(D::Minus1)?);
            xt = henc_time(&xt, w, i)?;
            saved_t.push(xt.clone());
            x = henc_freq(&x, w, i)?;
            if i == 0 {
                // frequency embedding (ScaledEmbedding, scale 10), weight 0.2
                let e = t(w, "freq_emb.embedding.weight")?; // (F, C)
                let emb = (e.t()?.unsqueeze(0)?.unsqueeze(3)? * (10.0 * 0.2))?; // (1, C, F, 1)
                x = x.broadcast_add(&emb)?;
            }
            saved.push(x.clone());
        }
        // bottleneck: 384 -> 512, cross transformer, 512 -> 384
        let (b, c, fr, t1) = x.dims4()?;
        let xf = x.reshape((b, c, fr * t1))?;
        let xf = conv1d(&xf, w, "channel_upsampler", 0, 1, 1)?;
        let xs = xf.reshape((b, 512, fr, t1))?;
        let xt_up = conv1d(&xt, w, "channel_upsampler_t", 0, 1, 1)?;
        // (b c fr t1) -> (b (t1 fr) c)
        let mut xa = xs.permute((0, 3, 2, 1))?.contiguous()?.reshape((b, t1 * fr, 512))?;
        let t2 = xt_up.dim(2)?;
        let mut xb = xt_up.permute((0, 2, 1))?.contiguous()?;
        xa = layer_norm(&xa, w, "crosstransformer.norm_in")?;
        xa = (xa + sin_embedding_2d(512, fr, t1, &self.dev)?)?;
        xb = layer_norm(&xb, w, "crosstransformer.norm_in_t")?;
        xb = (xb + sin_embedding_1d(t2, 512, &self.dev)?)?;
        for i in 0..5 {
            if i % 2 == 0 {
                xa = self_layer(&xa, w, &format!("crosstransformer.layers.{i}"))?;
                xb = self_layer(&xb, w, &format!("crosstransformer.layers_t.{i}"))?;
            } else {
                let old = xa.clone();
                xa = cross_layer(&xa, &xb, w, &format!("crosstransformer.layers.{i}"))?;
                xb = cross_layer(&xb, &old, w, &format!("crosstransformer.layers_t.{i}"))?;
            }
        }
        let xs = xa.reshape((b, t1, fr, 512))?.permute((0, 3, 2, 1))?.contiguous()?.reshape((b, 512, fr * t1))?;
        x = conv1d(&xs, w, "channel_downsampler", 0, 1, 1)?.reshape((b, c, fr, t1))?;
        xt = conv1d(&xb.permute((0, 2, 1))?.contiguous()?, w, "channel_downsampler_t", 0, 1, 1)?;

        for j in 0..DEPTH {
            let skip = saved.pop().ok_or_else(|| Error::Model("skip stack".into()))?;
            lengths.pop();
            let (nx, _) = hdec_freq(&x, &skip, w, j, j == DEPTH - 1)?;
            x = nx;
            let skip_t = saved_t.pop().ok_or_else(|| Error::Model("skip stack".into()))?;
            let lt = lengths_t.pop().ok_or_else(|| Error::Model("length stack".into()))?;
            xt = hdec_time(&xt, &skip_t, w, j, j == DEPTH - 1, lt)?;
        }
        // frequency output: (1, 16, F, T) = sources x [ch0 re, ch0 im, ch1 re, ch1 im]
        let x = ((x * std)? + mean)?;
        let xv = x.flatten_all()?.to_vec1::<f32>()?; // [16][F][T]
        let xtv = ((xt * stdt)? + meant)?.reshape((4, 2, length))?.to_vec3::<f32>()?;
        let mut out = Vec::new();
        for s in 0..4 {
            let mut chans = Vec::new();
            for ch in 0..2 {
                let re_c = 4 * s + 2 * ch;
                let plane = bins * le;
                let re = xv[re_c * plane..(re_c + 1) * plane].to_vec();
                let im = xv[(re_c + 1) * plane..(re_c + 2) * plane].to_vec();
                let wav = self.stft.ispec(&re, &im, le, length);
                chans.push(wav.iter().zip(&xtv[s][ch]).map(|(a, b)| a + b).collect::<Vec<f32>>());
            }
            out.push(chans);
        }
        Ok(out)
    }

    /// Separate a stereo signal at 44.1 kHz: four stems of the same length (drums, bass, other, vocals).
    /// `progress(done, total)` returns false to cancel.
    pub fn separate(&self, mix: &[Vec<f32>; 2], progress: &mut dyn FnMut(usize, usize) -> bool) -> Result<Vec<[Vec<f32>; 2]>> {
        let n = mix[0].len().min(mix[1].len());
        if n < 4410 {
            return Err(Error::Input("the audio is shorter than 0.1 s".into()));
        }
        // demucs.separate: reference = mono mean, normalised by its mean and standard deviation
        let mono: Vec<f32> = (0..n).map(|i| (mix[0][i] + mix[1][i]) * 0.5).collect();
        let mean = mono.iter().map(|v| *v as f64).sum::<f64>() / n as f64;
        let var = mono.iter().map(|v| (*v as f64 - mean).powi(2)).sum::<f64>() / (n as f64 - 1.0);
        let std = var.sqrt().max(1e-8);
        let norm: [Vec<f32>; 2] = [0, 1].map(|c| mix[c][..n].iter().map(|v| ((*v as f64 - mean) / std) as f32).collect());
        let stride = ((1.0 - 0.25) * SEGMENT as f64) as usize;
        let weight: Vec<f32> = {
            let half = SEGMENT / 2;
            let mut v: Vec<f32> = (1..=half).map(|i| i as f32).collect();
            v.extend((1..=SEGMENT - half).rev().map(|i| i as f32));
            let m = v.iter().cloned().fold(0f32, f32::max);
            v.iter().map(|x| x / m).collect()
        };
        let mut out: Vec<[Vec<f32>; 2]> = (0..4).map(|_| [vec![0f32; n], vec![0f32; n]]).collect();
        let mut sum_w = vec![0f32; n];
        let offsets: Vec<usize> = (0..n).step_by(stride).collect();
        for (k, &off) in offsets.iter().enumerate() {
            if !progress(k, offsets.len()) {
                return Err(Error::Cancelled);
            }
            let len = SEGMENT.min(n - off);
            // TensorChunk.padded(SEGMENT): zeros around the chunk, centred
            let delta = SEGMENT - len;
            let start = off as isize - (delta / 2) as isize;
            let chunk: [Vec<f32>; 2] = [0, 1].map(|c| {
                (0..SEGMENT)
                    .map(|i| {
                        let p = start + i as isize;
                        if p >= 0 && (p as usize) < n { norm[c][p as usize] } else { 0.0 }
                    })
                    .collect()
            });
            let y = self.forward(&chunk)?;
            // center_trim to `len`
            let trim = (SEGMENT - len) / 2;
            for s in 0..4 {
                for c in 0..2 {
                    for i in 0..len {
                        out[s][c][off + i] += y[s][c][trim + i] * weight[i];
                    }
                }
            }
            for i in 0..len {
                sum_w[off + i] += weight[i];
            }
        }
        progress(offsets.len(), offsets.len());
        for s in out.iter_mut() {
            for c in 0..2 {
                for i in 0..n {
                    s[c][i] = (s[c][i] / sum_w[i].max(1e-12)) * std as f32 + mean as f32;
                }
            }
        }
        let _ = DType::F32;
        Ok(out)
    }
}
