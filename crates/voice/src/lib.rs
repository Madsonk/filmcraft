//! Neural voice clean-up (layer L2): DeepFilterNet3 on the pure-Rust ONNX interpreter.
//!
//! DeepFilterNet3 (Schröter et al., MIT OR Apache-2.0, <https://github.com/Rikorose/DeepFilterNet>)
//! enhances 48 kHz speech in two stages: a gain per ERB band (32 bands) on the whole spectrum, then
//! "deep filtering" (a 5-tap complex filter over time) on the lowest 96 bins (0–4.6 kHz). The three
//! ONNX graphs (`enc`, `erb_dec`, `df_dec`) are downloaded on demand (`filmcraft-modelstore`); this
//! crate does what surrounds them, ported from the algorithm of `libDF`: the 960-point STFT with a
//! Vorbis window and 480-sample hop, the ERB features with exponential mean normalisation, the
//! unit-normalised complex features, and the application of the gains and filter taps with the
//! stage gating on the local SNR the encoder predicts.
//!
//! The whole clip is processed offline, in chunks of 20 s that start 2 s early (the GRU state
//! warms up on the extra frames, which are then dropped), so memory stays bounded for long clips.
//!
//! [`VoiceModel::denoise`] takes mono samples at any rate (resampled to 48 kHz and back with a
//! windowed-sinc filter) and returns the same number of samples.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod fft;
mod resample;

use filmcraft_nn::{Model, Tensor};

use fft::{C, Fft};

pub const SAMPLE_RATE: u32 = 48_000;
const FFT: usize = 960;
const HOP: usize = 480;
const BINS: usize = FFT / 2 + 1;
const NB_ERB: usize = 32;
const NB_DF: usize = 96;
const DF_ORDER: usize = 5;
const LOOKAHEAD: usize = 2;
/// Local-SNR gates (dB) of libDF's default runtime parameters: below `MIN_DB` the frame is zeroed
/// (no speech), above `MAX_DB_ERB` it is left alone (clean speech). libDF also skips the deep
/// filter above 20 dB; here it runs on every processed frame, because the band gains alone are poor
/// on the low bins (measured: 14 dB noisy -> 8.7 dB with the skip, 22 dB without).
const MIN_DB: f32 = -10.0;
const MAX_DB_ERB: f32 = 30.0;
const NORM_ALPHA: f32 = 0.99;
const CHUNK: usize = 2000;
const WARMUP: usize = 200;
/// Longest clip processed in one go (the spectra of a clip are kept in memory: about 0.8 GB at this length).
pub const MAX_SECONDS: f64 = 900.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceError(pub String);

impl std::fmt::Display for VoiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for VoiceError {}

impl From<filmcraft_nn::NnError> for VoiceError {
    fn from(e: filmcraft_nn::NnError) -> Self {
        VoiceError(e.0)
    }
}

type Result<T> = std::result::Result<T, VoiceError>;

fn fail<T>(m: impl Into<String>) -> Result<T> {
    Err(VoiceError(m.into()))
}

/// How much of the noise to remove.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    /// 0 … 1. 1 removes as much noise as the model finds; below 1 the noisy signal is mixed back
    /// in the spectrum (`1 - amount` of it), which caps the noise reduction at `20 log10(1 - amount)`
    /// dB (0.5 → 6 dB, 0.8 → 14 dB, 0.9 → 20 dB).
    pub amount: f32,
}

impl Default for Options {
    fn default() -> Self {
        Options { amount: 1.0 }
    }
}

/// The three graphs of DeepFilterNet3.
pub struct VoiceModel {
    enc: Model,
    erb_dec: Model,
    df_dec: Model,
    erb: Vec<usize>,
    stft: Stft,
}

struct Stft {
    window: Vec<f64>,
}

impl Stft {
    fn new() -> Stft {
        let pi = std::f64::consts::PI;
        let window = (0..FFT)
            .map(|i| {
                let s = (0.5 * pi * (i as f64 + 0.5) / (FFT / 2) as f64).sin();
                (0.5 * pi * s * s).sin()
            })
            .collect();
        Stft { window }
    }

    /// STFT of `x` (48 kHz): `(re, im)` planes of `frames × BINS`.
    fn analysis(&self, x: &[f32], frames: usize) -> (Vec<f32>, Vec<f32>) {
        let fft = Fft::new(FFT);
        let mut re = vec![0f32; frames * BINS];
        let mut im = vec![0f32; frames * BINS];
        let norm = 1.0 / FFT as f64;
        let mut buf = vec![C::default(); FFT];
        for i in 0..frames {
            for (j, b) in buf.iter_mut().enumerate() {
                // frame i covers input samples [(i-1)*HOP, (i+1)*HOP)
                let pos = (i * HOP + j) as isize - HOP as isize;
                let v = if pos >= 0 { x.get(pos as usize).copied().unwrap_or(0.0) } else { 0.0 };
                *b = C::new(f64::from(v) * self.window.get(j).copied().unwrap_or(0.0), 0.0);
            }
            let spec = fft.forward(&buf);
            for k in 0..BINS {
                if let (Some(c), Some(r), Some(m)) = (spec.get(k), re.get_mut(i * BINS + k), im.get_mut(i * BINS + k)) {
                    *r = (c.re * norm) as f32;
                    *m = (c.im * norm) as f32;
                }
            }
        }
        (re, im)
    }

    /// Overlap-add synthesis of `frames` spectra; the result lags the analysed signal by `HOP`.
    fn synthesis(&self, re: &[f32], im: &[f32], frames: usize) -> Vec<f32> {
        let fft = Fft::new(FFT);
        let mut out = vec![0f32; frames * HOP];
        let mut mem = vec![0f64; HOP];
        let mut full = vec![C::default(); FFT];
        for i in 0..frames {
            for k in 0..BINS {
                let c = C::new(f64::from(re.get(i * BINS + k).copied().unwrap_or(0.0)), f64::from(im.get(i * BINS + k).copied().unwrap_or(0.0)));
                if let Some(slot) = full.get_mut(k) {
                    *slot = c;
                }
                if k > 0
                    && k < FFT / 2
                    && let Some(slot) = full.get_mut(FFT - k)
                {
                    *slot = C::new(c.re, -c.im);
                }
            }
            if let Some(nyq) = full.get_mut(FFT / 2) {
                nyq.im = 0.0;
            }
            if let Some(dc) = full.first_mut() {
                dc.im = 0.0;
            }
            let t = fft.inverse(&full);
            for j in 0..HOP {
                let w0 = self.window.get(j).copied().unwrap_or(0.0);
                let w1 = self.window.get(HOP + j).copied().unwrap_or(0.0);
                let a = t.get(j).map_or(0.0, |c| c.re) * w0;
                let b = t.get(HOP + j).map_or(0.0, |c| c.re) * w1;
                if let Some(o) = out.get_mut(i * HOP + j) {
                    *o = (a + mem.get(j).copied().unwrap_or(0.0)) as f32;
                }
                if let Some(m) = mem.get_mut(j) {
                    *m = b;
                }
            }
        }
        out
    }
}

fn freq2erb(f: f32) -> f32 {
    9.265 * (f / (24.7 * 9.265)).ln_1p()
}

fn erb2freq(n: f32) -> f32 {
    24.7 * 9.265 * ((n / 9.265).exp() - 1.0)
}

/// Width in bins of each ERB band (libDF `erb_fb`, 48 kHz, 960-point FFT, 32 bands, at least 2 bins).
pub fn erb_widths() -> Vec<usize> {
    let (sr, fft, bands, min_freqs) = (48_000usize, FFT, NB_ERB, 2i32);
    let freq_width = sr as f32 / fft as f32;
    let erb_low = freq2erb(0.0);
    let erb_high = freq2erb((sr / 2) as f32);
    let mut erb = vec![0usize; bands];
    let step = (erb_high - erb_low) / bands as f32;
    let mut prev_freq = 0i32;
    let mut freq_over = 0i32;
    for i in 1..=bands {
        let f = erb2freq(erb_low + i as f32 * step);
        let fb = (f / freq_width).round() as i32;
        let mut nb = fb - prev_freq - freq_over;
        if nb < min_freqs {
            freq_over = min_freqs - nb;
            nb = min_freqs;
        } else {
            freq_over = 0;
        }
        if let Some(e) = erb.get_mut(i - 1) {
            *e = nb.max(0) as usize;
        }
        prev_freq = fb;
    }
    if let Some(last) = erb.last_mut() {
        *last += 1;
    }
    let total: usize = erb.iter().sum();
    if total > fft / 2 + 1
        && let Some(last) = erb.last_mut()
    {
        *last = last.saturating_sub(total - (fft / 2 + 1));
    }
    erb
}

fn frame_count(n_samples: usize) -> usize {
    // frames needed to cover the samples, the filter delay (HOP) and the model lookahead
    (n_samples + FFT + HOP * (LOOKAHEAD + 1)).div_ceil(HOP)
}

impl VoiceModel {
    /// Reads the three ONNX files.
    pub fn from_bytes(enc: &[u8], erb_dec: &[u8], df_dec: &[u8]) -> Result<VoiceModel> {
        Ok(VoiceModel { enc: Model::parse(enc)?, erb_dec: Model::parse(erb_dec)?, df_dec: Model::parse(df_dec)?, erb: erb_widths(), stft: Stft::new() })
    }

    /// ERB (`frames × 32`) and unit-normalised complex (`2 × frames × 96`) features.
    fn features(&self, re: &[f32], im: &[f32], frames: usize) -> (Vec<f32>, Vec<f32>) {
        let mut erb_state: Vec<f32> = (0..NB_ERB).map(|i| -60.0 + i as f32 * (-90.0 - -60.0) / (NB_ERB - 1) as f32).collect();
        let mut unit_state: Vec<f32> = (0..NB_DF).map(|i| 0.001 + i as f32 * (0.0001 - 0.001) / (NB_DF - 1) as f32).collect();
        let mut fe = vec![0f32; frames * NB_ERB];
        let mut fs = vec![0f32; 2 * frames * NB_DF];
        let a = NORM_ALPHA;
        for t in 0..frames {
            let mut start = 0;
            for (b, &w) in self.erb.iter().enumerate() {
                let mut acc = 0f32;
                for k in start..start + w {
                    let (r, m) = (re.get(t * BINS + k).copied().unwrap_or(0.0), im.get(t * BINS + k).copied().unwrap_or(0.0));
                    acc += (r * r + m * m) * (1.0 / w as f32);
                }
                start += w;
                let db = (acc + 1e-10).log10() * 10.0;
                if let (Some(s), Some(o)) = (erb_state.get_mut(b), fe.get_mut(t * NB_ERB + b)) {
                    *s = db * (1.0 - a) + *s * a;
                    *o = (db - *s) / 40.0;
                }
            }
            for k in 0..NB_DF {
                let (r, m) = (re.get(t * BINS + k).copied().unwrap_or(0.0), im.get(t * BINS + k).copied().unwrap_or(0.0));
                if let Some(s) = unit_state.get_mut(k) {
                    *s = (r * r + m * m).sqrt() * (1.0 - a) + *s * a;
                    let d = s.sqrt().max(1e-12);
                    if let Some(o) = fs.get_mut(t * NB_DF + k) {
                        *o = r / d;
                    }
                    if let Some(o) = fs.get_mut(frames * NB_DF + t * NB_DF + k) {
                        *o = m / d;
                    }
                }
            }
        }
        (fe, fs)
    }

    /// Runs the graphs on frames `[a, b)`; returns `(lsnr, gains, taps)` for those frames.
    fn run_frames(&self, fe: &[f32], fs: &[f32], frames: usize, a: usize, b: usize) -> Result<(Vec<f32>, Vec<f32>, Vec<f32>)> {
        let s = b - a;
        let erb_in = Tensor::f32(vec![1, 1, s, NB_ERB], fe.get(a * NB_ERB..b * NB_ERB).unwrap_or(&[]).to_vec())?;
        let mut spec = Vec::with_capacity(2 * s * NB_DF);
        for plane in 0..2 {
            spec.extend_from_slice(fs.get(plane * frames * NB_DF + a * NB_DF..plane * frames * NB_DF + b * NB_DF).unwrap_or(&[]));
        }
        let spec_in = Tensor::f32(vec![1, 2, s, NB_DF], spec)?;
        let outs = self.enc.run(vec![("feat_erb", erb_in), ("feat_spec", spec_in)])?;
        let by_name = |m: &Model, outs: &[Tensor], name: &str| -> Result<Tensor> {
            m.output_names().iter().position(|n| n == name).and_then(|i| outs.get(i)).cloned().ok_or_else(|| VoiceError(format!("model output {name} missing")))
        };
        let (e0, e1, e2, e3) =
            (by_name(&self.enc, &outs, "e0")?, by_name(&self.enc, &outs, "e1")?, by_name(&self.enc, &outs, "e2")?, by_name(&self.enc, &outs, "e3")?);
        let (emb, c0, lsnr) = (by_name(&self.enc, &outs, "emb")?, by_name(&self.enc, &outs, "c0")?, by_name(&self.enc, &outs, "lsnr")?);
        let m = self.erb_dec.run(vec![("emb", emb.clone()), ("e3", e3), ("e2", e2), ("e1", e1), ("e0", e0)])?;
        let coefs = self.df_dec.run(vec![("emb", emb), ("c0", c0)])?;
        let gains = m.into_iter().next().ok_or_else(|| VoiceError("erb_dec returned nothing".into()))?;
        let taps = by_name(&self.df_dec, &coefs, "coefs")?;
        let (lv, gv, tv) = (lsnr.as_f32()?.to_vec(), gains.as_f32()?.to_vec(), taps.as_f32()?.to_vec());
        if lv.len() != s || gv.len() != s * NB_ERB || tv.len() != s * NB_DF * DF_ORDER * 2 {
            return fail("unexpected model output sizes");
        }
        Ok((lv, gv, tv))
    }

    /// Cleans mono `input` (any sample rate) and returns the same number of samples. `progress`
    /// receives 0 … 1 and returns false to cancel.
    pub fn denoise(&self, input: &[f32], sample_rate: u32, opts: &Options, progress: &mut dyn FnMut(f32) -> bool) -> Result<Vec<f32>> {
        if input.is_empty() {
            return Ok(Vec::new());
        }
        if sample_rate == 0 {
            return fail("sample rate 0");
        }
        if input.len() as f64 / f64::from(sample_rate) > MAX_SECONDS {
            return fail(format!("clip longer than {} min: split it first", MAX_SECONDS / 60.0));
        }
        let x48: Vec<f32> = if sample_rate == SAMPLE_RATE { input.to_vec() } else { resample::resample(input, sample_rate, SAMPLE_RATE) };
        let frames = frame_count(x48.len());
        let (re, im) = self.stft.analysis(&x48, frames);
        let (fe, fs) = self.features(&re, &im, frames);
        let mix = (1.0 - opts.amount.clamp(0.0, 1.0)).clamp(0.0, 1.0);
        let mut out_re = vec![0f32; frames * BINS];
        let mut out_im = vec![0f32; frames * BINS];
        // frame i is enhanced with the model outputs of frame i + LOOKAHEAD
        let out_frames = frames.saturating_sub(LOOKAHEAD);
        let mut start = 0;
        while start < out_frames {
            let end = (start + CHUNK).min(out_frames);
            let a = start.saturating_sub(WARMUP);
            // model outputs are needed for frames [start + LOOKAHEAD, end + LOOKAHEAD)
            let b = (end + LOOKAHEAD).min(frames);
            let (lsnr, gains, taps) = self.run_frames(&fe, &fs, frames, a, b)?;
            for i in start..end {
                let t = i + LOOKAHEAD;
                let rel = t - a;
                let l = lsnr.get(rel).copied().unwrap_or(0.0);
                let nre = |k: usize, f: usize| re.get(f * BINS + k).copied().unwrap_or(0.0);
                let nim = |k: usize, f: usize| im.get(f * BINS + k).copied().unwrap_or(0.0);
                for k in 0..BINS {
                    let (mut yr, mut yi) = (nre(k, i), nim(k, i));
                    if l < MIN_DB {
                        yr = 0.0;
                        yi = 0.0;
                    } else if l <= MAX_DB_ERB {
                        let mut band = 0;
                        let mut acc = 0;
                        for (bi, w) in self.erb.iter().enumerate() {
                            if k < acc + w {
                                band = bi;
                                break;
                            }
                            acc += w;
                            band = bi;
                        }
                        let g = gains.get(rel * NB_ERB + band).copied().unwrap_or(1.0);
                        yr *= g;
                        yi *= g;
                    }
                    if (MIN_DB..=MAX_DB_ERB).contains(&l) && k < NB_DF {
                        let (mut sr, mut si) = (0f32, 0f32);
                        for tap in 0..DF_ORDER {
                            // taps pair with the noisy frames i-2 … i+2
                            let f = (i + tap) as isize - 2;
                            if f < 0 || f as usize >= frames {
                                continue;
                            }
                            let f = f as usize;
                            let base = ((rel * NB_DF + k) * DF_ORDER + tap) * 2;
                            let (cr, ci) = (taps.get(base).copied().unwrap_or(0.0), taps.get(base + 1).copied().unwrap_or(0.0));
                            let (xr, xi) = (nre(k, f), nim(k, f));
                            sr += xr * cr - xi * ci;
                            si += xr * ci + xi * cr;
                        }
                        yr = sr;
                        yi = si;
                    }
                    if mix > 0.0 {
                        yr = yr * (1.0 - mix) + nre(k, i) * mix;
                        yi = yi * (1.0 - mix) + nim(k, i) * mix;
                    }
                    if let (Some(r), Some(m)) = (out_re.get_mut(i * BINS + k), out_im.get_mut(i * BINS + k)) {
                        *r = yr;
                        *m = yi;
                    }
                }
            }
            start = end;
            if !progress(0.9 * start as f32 / out_frames.max(1) as f32) {
                return fail("cancelled");
            }
        }
        let stream = self.stft.synthesis(&out_re, &out_im, out_frames);
        let y48: Vec<f32> = (0..x48.len()).map(|j| stream.get(j + HOP).copied().unwrap_or(0.0)).collect();
        let y = if sample_rate == SAMPLE_RATE { y48 } else { resample::resample(&y48, SAMPLE_RATE, sample_rate) };
        let mut y = y;
        y.resize(input.len(), 0.0);
        progress(1.0);
        Ok(y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erb_bands_cover_every_bin() {
        let w = erb_widths();
        assert_eq!(w.len(), NB_ERB);
        assert_eq!(w.iter().sum::<usize>(), BINS);
        assert!(w.iter().all(|x| *x >= 2));
    }

    #[test]
    fn stft_round_trips() {
        let st = Stft::new();
        let x: Vec<f32> = (0..9600).map(|i| (i as f32 * 0.05).sin() * 0.5 + (i as f32 * 0.31).cos() * 0.2).collect();
        let frames = frame_count(x.len());
        let (re, im) = st.analysis(&x, frames);
        let y = st.synthesis(&re, &im, frames);
        for j in 0..x.len() {
            assert!((y[j + HOP] - x[j]).abs() < 1e-4, "sample {j}: {} vs {}", y[j + HOP], x[j]);
        }
    }
}
