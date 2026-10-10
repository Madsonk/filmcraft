//! Beat This! (Foscarin, Schlüter, Widmer, ISMIR 2024, MIT) beat and downbeat tracker on candle.
//!
//! A re-implementation, from the paper's published model definition (`CPJKU/beat_this`, MIT), of
//! the network: a convolutional frontend with frequency- and time-direction partial transformers,
//! six rotary-embedding transformer blocks and a two-output head (the beat logit is the sum of the
//! two outputs, so a downbeat is always also a beat). Input is a 22 050 Hz mono log-mel spectrogram
//! (1024-point FFT, hop 441, 128 Slaney mel bands, `log1p(1000 x)`: [`log_mel`]); the filter bank is
//! the one shipped with the weights. Long pieces are cut in windows of 1500 frames (30 s) with a 6 frame
//! border, as the reference does, and the first window's prediction wins where they overlap.
//! Post-processing is the reference's `minimal` one: logit peaks above 0 inside +-70 ms, adjacent
//! peaks merged, downbeats moved to the nearest beat. The `dbn` post-processing needs madmom
//! (non-commercial weights, GPL-compatible code) and is not implemented.
//!
//! Weights: `model.safetensors` (checkpoint `final0`, 81 MB, MIT), `mel-filterbank.bin`.

use std::collections::HashMap;
use std::path::Path;

use candle_core::{D, Device, Tensor};

use crate::{Error, Result, pick_device, read_file};

pub const SAMPLE_RATE: u32 = 22_050;
const N_FFT: usize = 1024;
const HOP: usize = 441;
const MELS: usize = 128;
pub const FPS: f64 = 50.0;
const CHUNK: usize = 1500;
const BORDER: usize = 6;

/// Beat times and downbeat times in seconds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tracked {
    pub beats: Vec<f64>,
    pub downbeats: Vec<f64>,
}

// ---------------------------------------------------------------------------------------------
// log-mel front end

fn fft(re: &mut [f32], im: &mut [f32], tw: &[(f32, f32)]) {
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

/// `log1p(1000 * mel)` frames (row-major `frames x 128`) of mono 22 050 Hz `samples`, `filterbank` being
/// the `513 x 128` matrix of the weights (row-major).
pub fn log_mel(samples: &[f32], filterbank: &[f32]) -> Result<(Vec<f32>, usize)> {
    if filterbank.len() != (N_FFT / 2 + 1) * MELS {
        return Err(Error::Model(format!("mel filter bank has {} values, expected {}", filterbank.len(), (N_FFT / 2 + 1) * MELS)));
    }
    let n = samples.len();
    if n < N_FFT / 2 + 1 {
        return Err(Error::Input("the audio is shorter than 0.03 s".into()));
    }
    let frames = 1 + n / HOP;
    let pad = N_FFT / 2;
    // reflect padding (torch.stft center=True)
    let at = |i: isize| -> f32 {
        let mut k = i;
        let last = n as isize - 1;
        if k < 0 {
            k = -k;
        }
        if k > last {
            k = 2 * last - k;
        }
        samples[k.clamp(0, last) as usize]
    };
    let window: Vec<f32> = (0..N_FFT).map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / N_FFT as f64).cos() as f32).collect();
    let tw: Vec<(f32, f32)> = (0..N_FFT / 2)
        .map(|k| ((-2.0 * std::f64::consts::PI * k as f64 / N_FFT as f64).cos() as f32, (-2.0 * std::f64::consts::PI * k as f64 / N_FFT as f64).sin() as f32))
        .collect();
    let norm = 1.0 / (N_FFT as f32).sqrt();
    let mut out = vec![0f32; frames * MELS];
    use rayon::prelude::*;
    out.par_chunks_mut(MELS).enumerate().for_each(|(f, row)| {
        let mut re = vec![0f32; N_FFT];
        let mut im = vec![0f32; N_FFT];
        let start = (f * HOP) as isize - pad as isize;
        for i in 0..N_FFT {
            re[i] = at(start + i as isize) * window[i];
        }
        fft(&mut re, &mut im, &tw);
        for (m, o) in row.iter_mut().enumerate() {
            let mut acc = 0f32;
            for k in 0..=N_FFT / 2 {
                let mag = (re[k] * re[k] + im[k] * im[k]).sqrt() * norm;
                acc += mag * filterbank[k * MELS + m];
            }
            *o = (1000.0 * acc).ln_1p();
        }
    });
    Ok((out, frames))
}

// ---------------------------------------------------------------------------------------------
// the network

type W = HashMap<String, Tensor>;

fn t(w: &W, k: &str) -> Result<Tensor> {
    w.get(k).cloned().ok_or_else(|| Error::Model(format!("model.safetensors: missing tensor {k}")))
}

struct Linear {
    w: Tensor,
    b: Option<Tensor>,
}

impl Linear {
    fn load(w: &W, p: &str, bias: bool) -> Result<Self> {
        Ok(Self { w: t(w, &format!("{p}.weight"))?.t()?.contiguous()?, b: if bias { Some(t(w, &format!("{p}.bias"))?) } else { None } })
    }
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let y = x.broadcast_matmul(&self.w)?;
        Ok(match &self.b {
            Some(b) => y.broadcast_add(b)?,
            None => y,
        })
    }
}

/// `F.normalize(x, dim=-1) * sqrt(dim) * gamma`.
struct RmsNorm {
    gamma: Tensor,
    scale: f64,
}

impl RmsNorm {
    fn load(w: &W, p: &str) -> Result<Self> {
        let g = t(w, &format!("{p}.gamma"))?;
        let d = g.dim(0)?;
        Ok(Self { gamma: g, scale: (d as f64).sqrt() })
    }
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let n = x.sqr()?.sum_keepdim(D::Minus1)?.sqrt()?.clamp(1e-12, f64::MAX)?;
        Ok(x.broadcast_div(&n)?.affine(self.scale, 0.0)?.broadcast_mul(&self.gamma)?)
    }
}

struct FeedForward {
    norm: RmsNorm,
    l1: Linear,
    l2: Linear,
}

impl FeedForward {
    fn load(w: &W, p: &str) -> Result<Self> {
        Ok(Self {
            norm: RmsNorm::load(w, &format!("{p}.net.0"))?,
            l1: Linear::load(w, &format!("{p}.net.1"), true)?,
            l2: Linear::load(w, &format!("{p}.net.4"), true)?,
        })
    }
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        self.l2.forward(&self.l1.forward(&self.norm.forward(x)?)?.gelu_erf()?)
    }
}

struct Attention {
    norm: RmsNorm,
    qkv: Linear,
    gates: Linear,
    out: Linear,
    heads: usize,
    dim_head: usize,
}

/// cos and sin of `position x frequency`, each frequency repeated for the two members of its pair.
struct Rotary {
    freqs: Vec<f32>,
}

impl Rotary {
    fn tables(&self, n: usize, dev: &Device) -> Result<(Tensor, Tensor)> {
        let d = self.freqs.len();
        let mut cos = Vec::with_capacity(n * d * 2);
        let mut sin = Vec::with_capacity(n * d * 2);
        for pos in 0..n {
            for f in &self.freqs {
                let a = pos as f32 * f;
                let (s, c) = a.sin_cos();
                cos.extend([c, c]);
                sin.extend([s, s]);
            }
        }
        Ok((Tensor::from_vec(cos, (n, d * 2), dev)?, Tensor::from_vec(sin, (n, d * 2), dev)?))
    }
}

/// `x * cos + rotate_half(x) * sin` with `rotate_half` turning the pair (a, b) into (-b, a).
fn rotate(x: &Tensor, cos: &Tensor, sin: &Tensor) -> Result<Tensor> {
    let (b, h, n, d) = x.dims4()?;
    let p = x.reshape((b, h, n, d / 2, 2))?;
    let (x1, x2) = (p.narrow(4, 0, 1)?, p.narrow(4, 1, 1)?);
    let rot = Tensor::cat(&[&x2.neg()?, &x1], 4)?.reshape((b, h, n, d))?;
    Ok((x.broadcast_mul(cos)? + rot.broadcast_mul(sin)?)?)
}

impl Attention {
    fn load(w: &W, p: &str, dim_head: usize) -> Result<Self> {
        let g = t(w, &format!("{p}.to_gates.weight"))?;
        Ok(Self {
            norm: RmsNorm::load(w, &format!("{p}.norm"))?,
            qkv: Linear::load(w, &format!("{p}.to_qkv"), false)?,
            gates: Linear::load(w, &format!("{p}.to_gates"), true)?,
            out: Linear::load(w, &format!("{p}.to_out.0"), false)?,
            heads: g.dim(0)?,
            dim_head,
        })
    }

    /// `x`: (batch, n, dim).
    fn forward(&self, x: &Tensor, rot: &Rotary) -> Result<Tensor> {
        let (b, n, _) = x.dims3()?;
        let x = self.norm.forward(x)?;
        let (h, d) = (self.heads, self.dim_head);
        let qkv = self.qkv.forward(&x)?.reshape((b, n, 3, h, d))?;
        let pick = |i: usize| -> Result<Tensor> { Ok(qkv.narrow(2, i, 1)?.squeeze(2)?.permute((0, 2, 1, 3))?.contiguous()?) };
        let (q, k, v) = (pick(0)?, pick(1)?, pick(2)?);
        let (cos, sin) = rot.tables(n, x.device())?;
        let (q, k) = (rotate(&q, &cos, &sin)?, rotate(&k, &cos, &sin)?);
        let scale = (d as f64).powf(-0.5);
        // scores are (batch*heads) x n x n floats: do them in slices so a 1500-step window stays small
        let bh = b * h;
        let (q, k, v) = (q.reshape((bh, n, d))?, k.reshape((bh, n, d))?, v.reshape((bh, n, d))?);
        let per = ((48usize << 20) / (n * n).max(1)).clamp(1, bh);
        let mut outs = Vec::new();
        let mut s = 0;
        while s < bh {
            let m = per.min(bh - s);
            let (qs, ks, vs) = (q.narrow(0, s, m)?, k.narrow(0, s, m)?, v.narrow(0, s, m)?);
            let a = candle_nn::ops::softmax_last_dim(&(qs.matmul(&ks.transpose(1, 2)?)? * scale)?)?;
            outs.push(a.matmul(&vs)?);
            s += m;
        }
        let o = Tensor::cat(&outs, 0)?.reshape((b, h, n, d))?;
        let gates = candle_nn::ops::sigmoid(&self.gates.forward(&x)?)?.permute((0, 2, 1))?.unsqueeze(3)?;
        let o = o.broadcast_mul(&gates)?.permute((0, 2, 1, 3))?.reshape((b, n, h * d))?;
        self.out.forward(&o)
    }
}

struct Partial {
    attn_f: Attention,
    ff_f: FeedForward,
    attn_t: Attention,
    ff_t: FeedForward,
}

impl Partial {
    fn load(w: &W, p: &str) -> Result<Self> {
        Ok(Self {
            attn_f: Attention::load(w, &format!("{p}.attnF"), 32)?,
            ff_f: FeedForward::load(w, &format!("{p}.ffF"))?,
            attn_t: Attention::load(w, &format!("{p}.attnT"), 32)?,
            ff_t: FeedForward::load(w, &format!("{p}.ffT"))?,
        })
    }
    /// `x`: (1, c, f, t).
    fn forward(&self, x: &Tensor, rot: &Rotary) -> Result<Tensor> {
        let (b, c, f, tt) = x.dims4()?;
        // frequency direction: (b t) f c
        let mut y = x.permute((0, 3, 2, 1))?.reshape((b * tt, f, c))?.contiguous()?;
        y = (&y + self.attn_f.forward(&y, rot)?)?;
        y = (&y + self.ff_f.forward(&y)?)?;
        // time direction: (b f) t c
        let mut y = y.reshape((b, tt, f, c))?.permute((0, 2, 1, 3))?.reshape((b * f, tt, c))?.contiguous()?;
        y = (&y + self.attn_t.forward(&y, rot)?)?;
        y = (&y + self.ff_t.forward(&y)?)?;
        Ok(y.reshape((b, f, tt, c))?.permute((0, 3, 1, 2))?.contiguous()?)
    }
}

/// Eval-mode batch norm over the channel axis `axis`, as `x * scale + shift`.
struct Bn {
    scale: Tensor,
    shift: Tensor,
}

impl Bn {
    fn load(w: &W, p: &str, shape: &[usize]) -> Result<Self> {
        let (g, b, m, v) =
            (t(w, &format!("{p}.weight"))?, t(w, &format!("{p}.bias"))?, t(w, &format!("{p}.running_mean"))?, t(w, &format!("{p}.running_var"))?);
        let scale = (g / (v + 1e-5)?.sqrt()?)?;
        let shift = (b - (m * &scale)?)?;
        Ok(Self { scale: scale.reshape(shape)?, shift: shift.reshape(shape)? })
    }
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        Ok(x.broadcast_mul(&self.scale)?.broadcast_add(&self.shift)?)
    }
}

/// Convolution with a `(kf, 3)` kernel, stride `(kf, 1)`, padding `(0, 1)`, as one matrix product.
struct FConv {
    w: Tensor, // (c*kf*3, out)
    kf: usize,
}

impl FConv {
    fn load(w: &W, p: &str) -> Result<Self> {
        let k = t(w, &format!("{p}.weight"))?;
        let (o, c, kf, kt) = k.dims4()?;
        if kt != 3 {
            return Err(Error::Model("unexpected frontend kernel".into()));
        }
        Ok(Self { w: k.reshape((o, c * kf * 3))?.t()?.contiguous()?, kf })
    }
    /// `x`: (b, c, f, t) -> (b, out, f / kf, t).
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let (b, c, f, tt) = x.dims4()?;
        let kf = self.kf;
        let xp = x.pad_with_zeros(3, 1, 1)?; // (b,c,f,t+2)
        let shifts: Vec<Tensor> = (0..3).map(|s| xp.narrow(3, s, tt)).collect::<candle_core::Result<_>>()?;
        let st = Tensor::stack(&shifts, 3)?; // (b,c,f,3,t)
        let st = st.reshape((b, c, f / kf, kf, 3, tt))?; // (b,c,f',kf,3,t)
        let st = st.permute((0, 2, 5, 1, 3, 4))?.contiguous()?.reshape((b, f / kf, tt, c * kf * 3))?;
        let y = st.broadcast_matmul(&self.w)?; // (b,f',t,out)
        Ok(y.permute((0, 3, 1, 2))?.contiguous()?)
    }
}

struct FrontBlock {
    partial: Partial,
    conv: FConv,
    bn: Bn,
}

/// The loaded model.
pub struct BeatThis {
    bn1: Bn,
    stem: FConv,
    bn2: Bn,
    blocks: Vec<FrontBlock>,
    linear: Linear,
    layers: Vec<(Attention, FeedForward)>,
    norm: RmsNorm,
    head: Linear,
    rot: Rotary,
    fb: Vec<f32>,
    dev: Device,
}

impl BeatThis {
    pub const WEIGHTS: &'static str = "model.safetensors";
    pub const FILTERBANK: &'static str = "mel-filterbank.bin";

    pub fn load(dir: &Path) -> Result<Self> {
        let dev = pick_device();
        let p = dir.join(Self::WEIGHTS);
        let w: W = candle_core::safetensors::load_buffer(&read_file(&p)?, &dev).map_err(|e| Error::Model(format!("{}: {e}", p.display())))?;
        let fbb = read_file(&dir.join(Self::FILTERBANK))?;
        let fb: Vec<f32> = fbb.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
        let freqs = t(&w, "frontend.blocks.0.partial.attnF.rotary_embed.freqs")?.to_vec1::<f32>()?;
        let mut blocks = Vec::new();
        for i in 0..3 {
            let pre = format!("frontend.blocks.{i}");
            let out = t(&w, &format!("{pre}.norm.weight"))?.dim(0)?;
            blocks.push(FrontBlock {
                partial: Partial::load(&w, &format!("{pre}.partial"))?,
                conv: FConv::load(&w, &format!("{pre}.conv2d"))?,
                bn: Bn::load(&w, &format!("{pre}.norm"), &[1, out, 1, 1])?,
            });
        }
        let mut layers = Vec::new();
        let mut i = 0;
        while w.contains_key(&format!("transformer_blocks.layers.{i}.0.to_qkv.weight")) {
            layers.push((
                Attention::load(&w, &format!("transformer_blocks.layers.{i}.0"), 32)?,
                FeedForward::load(&w, &format!("transformer_blocks.layers.{i}.1"))?,
            ));
            i += 1;
        }
        let stem_out = t(&w, "frontend.stem.bn2d.weight")?.dim(0)?;
        Ok(Self {
            bn1: Bn::load(&w, "frontend.stem.bn1d", &[1, 1, MELS, 1])?,
            stem: FConv::load_stem(&w)?,
            bn2: Bn::load(&w, "frontend.stem.bn2d", &[1, stem_out, 1, 1])?,
            blocks,
            linear: Linear::load(&w, "frontend.linear", true)?,
            layers,
            norm: RmsNorm::load(&w, "transformer_blocks.norm")?,
            head: Linear::load(&w, "task_heads.beat_downbeat_lin", true)?,
            rot: Rotary { freqs },
            fb,
            dev,
        })
    }

    pub fn device(&self) -> &Device {
        &self.dev
    }

    /// Logits of one window: `spect` is `frames x 128` row-major; returns (beat, downbeat) logits per frame.
    fn window(&self, spect: &[f32], frames: usize) -> Result<(Vec<f32>, Vec<f32>)> {
        let x = Tensor::from_vec(spect.to_vec(), (1, frames, MELS), &self.dev)?;
        // (b t f) -> (b 1 f t), batch norm over f
        let x = x.permute((0, 2, 1))?.unsqueeze(1)?.contiguous()?;
        let x = self.bn1.forward(&x)?;
        let mut x = self.bn2.forward(&self.stem.forward(&x)?)?.gelu_erf()?;
        for b in &self.blocks {
            x = b.partial.forward(&x, &self.rot)?;
            x = b.bn.forward(&b.conv.forward(&x)?)?.gelu_erf()?;
        }
        let (bb, c, f, tt) = x.dims4()?;
        // b c f t -> b t (c f)
        let x = x.permute((0, 3, 1, 2))?.reshape((bb, tt, c * f))?.contiguous()?;
        let mut x = self.linear.forward(&x)?;
        for (a, ff) in &self.layers {
            x = (a.forward(&x, &self.rot)? + &x)?;
            x = (ff.forward(&x)? + &x)?;
        }
        let x = self.norm.forward(&x)?;
        let o = self.head.forward(&x)?.squeeze(0)?; // (t, 2)
        let v = o.to_vec2::<f32>()?;
        let down: Vec<f32> = v.iter().map(|r| r[1]).collect();
        let beat: Vec<f32> = v.iter().map(|r| r[0] + r[1]).collect();
        Ok((beat, down))
    }

    /// Frame-wise logits for the whole piece (`frames x 128` spectrogram), windows aggregated as the reference does.
    pub fn logits(&self, spect: &[f32], frames: usize, progress: &mut dyn FnMut(usize, usize) -> bool) -> Result<(Vec<f32>, Vec<f32>)> {
        let (chunk, border) = (CHUNK, BORDER);
        let step = chunk - 2 * border;
        let mut starts: Vec<isize> = Vec::new();
        let mut s = -(border as isize);
        while s < frames as isize - border as isize {
            starts.push(s);
            s += step as isize;
        }
        if starts.is_empty() {
            starts.push(-(border as isize));
        }
        if frames > step {
            let last = starts.len() - 1;
            starts[last] = frames as isize - (chunk - border) as isize;
        }
        let mut beat = vec![-1000f32; frames];
        let mut down = vec![-1000f32; frames];
        let total = starts.len();
        // reference order for keep_first: later windows are written first
        let mut results = Vec::new();
        for (i, &st) in starts.iter().enumerate() {
            if !progress(i, total) {
                return Err(Error::Cancelled);
            }
            let lo = st.max(0) as usize;
            let hi = ((st + chunk as isize) as usize).min(frames);
            let left = (-st).max(0) as usize;
            let right = border.min((st + chunk as isize - frames as isize).max(0) as usize);
            let mut buf = vec![0f32; left * MELS];
            buf.extend_from_slice(&spect[lo * MELS..hi * MELS]);
            buf.extend(std::iter::repeat_n(0f32, right * MELS));
            let n = left + (hi - lo) + right;
            results.push((st, self.window(&buf, n)?, n));
        }
        for (st, (b, d), n) in results.into_iter().rev() {
            // drop the border on both sides of the window
            let keep_lo = border;
            let keep_hi = n.saturating_sub(border);
            for k in keep_lo..keep_hi {
                let pos = st + k as isize;
                if pos >= 0 && (pos as usize) < frames {
                    beat[pos as usize] = b[k];
                    down[pos as usize] = d[k];
                }
            }
        }
        progress(total, total);
        Ok((beat, down))
    }

    /// Beat and downbeat times of mono 22 050 Hz audio.
    pub fn track(&self, mono: &[f32], progress: &mut dyn FnMut(usize, usize) -> bool) -> Result<Tracked> {
        let (spect, frames) = log_mel(mono, &self.fb)?;
        let (b, d) = self.logits(&spect, frames, progress)?;
        Ok(postprocess(&b, &d))
    }
}

impl FConv {
    fn load_stem(w: &W) -> Result<Self> {
        Self::load(w, "frontend.stem.conv2d")
    }
}

/// The reference's `minimal` post-processing.
pub fn postprocess(beat: &[f32], down: &[f32]) -> Tracked {
    let peaks = |l: &[f32]| -> Vec<usize> {
        let n = l.len();
        let mut out = Vec::new();
        for i in 0..n {
            if l[i] <= 0.0 {
                continue;
            }
            let (lo, hi) = (i.saturating_sub(3), (i + 3).min(n - 1));
            // the maximum of the +-3 neighbourhood (ties keep every equal frame, as max_pool1d does)
            if l[lo..=hi].iter().all(|v| *v <= l[i]) {
                out.push(i);
            }
        }
        out
    };
    let dedupe = |p: &[usize]| -> Vec<f64> {
        let mut r: Vec<f64> = Vec::new();
        let mut it = p.iter();
        let Some(&first) = it.next() else { return r };
        let (mut cur, mut c) = (first as f64, 1.0);
        for &p2 in it {
            if p2 as f64 - cur <= 1.0 {
                c += 1.0;
                cur += (p2 as f64 - cur) / c;
            } else {
                r.push(cur);
                cur = p2 as f64;
                c = 1.0;
            }
        }
        r.push(cur);
        r
    };
    let beats: Vec<f64> = dedupe(&peaks(beat)).iter().map(|f| f / FPS).collect();
    let mut downs: Vec<f64> = dedupe(&peaks(down)).iter().map(|f| f / FPS).collect();
    if !beats.is_empty() {
        for d in downs.iter_mut() {
            if let Some(nearest) = beats.iter().copied().min_by(|a, b| (a - *d).abs().total_cmp(&(b - *d).abs())) {
                *d = nearest;
            }
        }
    }
    downs.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    Tracked { beats, downbeats: downs }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peaks_are_merged_and_downbeats_move_to_a_beat() {
        // beat logits: peaks at frames 10, 11 (one beat at 10.5), 60, 110; downbeat logit peak at 62
        let n = 200;
        let mut b = vec![-5f32; n];
        let mut d = vec![-5f32; n];
        for (f, v) in [(10, 3.0), (11, 3.0), (60, 4.0), (110, 4.0)] {
            b[f] = v;
        }
        d[62] = 3.0;
        let t = postprocess(&b, &d);
        assert_eq!(t.beats.len(), 3, "{:?}", t.beats);
        assert!((t.beats[0] - 10.5 / FPS).abs() < 1e-9);
        assert_eq!(t.downbeats, vec![60.0 / FPS], "the downbeat lands on the nearest beat");
        assert!(postprocess(&[-1.0; 50], &[-1.0; 50]).beats.is_empty());
    }

    #[test]
    fn log_mel_has_one_frame_per_hop_and_is_silent_on_silence() {
        let fb = vec![0.001f32; (N_FFT / 2 + 1) * MELS];
        let (m, frames) = log_mel(&vec![0.0; 22_050], &fb).unwrap();
        assert_eq!(frames, 1 + 22_050 / HOP);
        assert_eq!(m.len(), frames * MELS);
        assert!(m.iter().all(|v| *v == 0.0));
        let tone: Vec<f32> = (0..22_050).map(|i| (i as f32 * 0.3).sin() * 0.5).collect();
        let (m, _) = log_mel(&tone, &fb).unwrap();
        assert!(m.iter().any(|v| *v > 1.0), "a tone is not silence");
        assert!(log_mel(&[0.0; 100], &fb).is_err());
    }
}
