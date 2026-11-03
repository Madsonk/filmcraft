//! Beat grid for Beat Edit: one steady tempo, its beats, and (optionally) where the bars start.
//!
//! The beat tracker of Clip ▸ Remix ([`crate::remix`]) takes the tempo from the strongest peak of
//! the onset-envelope autocorrelation, which on real drums is often the wrong octave (hi-hats on the
//! eighths read as double tempo, a kick that is stronger than the snare as half tempo). This module
//! keeps the same onset envelope and the same dynamic-programming tracker but chooses the tempo
//! differently:
//!
//! ```text
//! onset envelope ─► autocorrelation peaks (the few strongest)
//!                ─► candidates: each peak, half and double of it, inside the tempo range
//!                ─► per candidate: track the beats, then measure how well they are *aligned*:
//!                     on  = mean onset strength on the tracked beats
//!                     off = mean onset strength half-way between two tracked beats
//!                   a level is a pulse when the strength is on the beat and not in between
//!                   (on/off high): a half-tempo candidate has the snare half-way (on/off ≈ 1), a
//!                   double-tempo candidate on hi-hats has the same hats on and off the beat
//!                ─► pick the best on/off ratio, with a weak preference for the usual tempo range
//!                ─► optional: the bar start among the four possible phases (accent + harmonic change)
//! ```
//!
//! The usable tempo range is [`TEMPO_MIN`]…[`TEMPO_MAX`] BPM unless the caller gives a tempo hint
//! (a hint selects the octave, it does not force the beat positions: those are still tracked).
//! What cannot be known from audio alone: whether a track at 80 BPM is "really" 160 in the
//! musician's count, and which of the two kick beats of a bar is the first when nothing else
//! (a chord change, a bass note, a crash) marks the bar start.

use crate::remix::{RemixError, beat_features, cos_sim, local_max, onset_envelope, onset_frame, trimmed_beats};

/// Slowest tempo considered without a hint (BPM).
pub const TEMPO_MIN: f64 = 65.0;
/// Fastest tempo considered without a hint (BPM).
pub const TEMPO_MAX: f64 = 172.0;
/// A hint is accepted from this tempo up/down (BPM).
pub const HINT_MIN: f64 = 30.0;
pub const HINT_MAX: f64 = 300.0;
/// Share of the comb's teeth that must meet an onset, and share of the onset strength the comb must explain.
const MIN_PRECISION: f64 = 0.6;
const MIN_RECALL: f64 = 0.3;
/// Stiffness of the beat tracker (Remix uses 100): a beat interval that differs from the period by a
/// ratio r costs `STIFFNESS · ln²(r)`, so the path stays on the beat ridge instead of hopping to a
/// click or a pre-hit a few frames away.
const STIFFNESS: f64 = 800.0;

/// What the caller can ask for.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GridOptions {
    /// The tempo the user expects (BPM, any octave inside [`HINT_MIN`]…[`HINT_MAX`]): the tracker uses
    /// the tempo of the audio nearest to it.
    pub tempo_hint: Option<f64>,
    /// Look for the first beat of the bars.
    pub downbeat: bool,
}

/// One tempo the analysis weighed.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub bpm: f64,
    /// On-beat strength over half-way strength.
    pub precision: f64,
    pub recall: f64,
    /// The final score (higher wins).
    pub score: f64,
}

/// The tempo and the beats of a piece of audio.
#[derive(Clone, Debug, PartialEq)]
pub struct Grid {
    pub sample_rate: u32,
    /// Source length (samples).
    pub len: i64,
    /// Attack of each beat (samples), increasing.
    pub beats: Vec<i64>,
    pub bpm: f64,
    /// Index in `beats` of the first beat of a bar (0…3) when the bars were looked for.
    pub bar_start: Option<usize>,
    /// 0 … 1: how clearly the bar start stood out (0 = a guess between equals).
    pub bar_confidence: f64,
    /// The other tempi that were weighed, best first.
    pub candidates: Vec<Candidate>,
}

fn prior(bpm: f64) -> f64 {
    // weak: a tempo an octave away from 115 BPM loses 0.35
    -0.35 * ((bpm / 115.0).log2()).powi(2)
}

/// Lag (frames) → BPM.
fn bpm_of(lag: f64, frame_rate: f64) -> f64 {
    60.0 * frame_rate / lag
}

/// Strongest autocorrelation peaks of `env`: (lag in frames refined, normalised value).
fn ac_peaks(env: &[f32], frame_rate: f64, lo_bpm: f64, hi_bpm: f64, keep: usize) -> Vec<(f64, f64)> {
    let mean = env.iter().sum::<f32>() / env.len().max(1) as f32;
    let e: Vec<f32> = env.iter().map(|v| v - mean).collect();
    let lo = (frame_rate * 60.0 / hi_bpm).floor().max(2.0) as usize;
    let hi = ((frame_rate * 60.0 / lo_bpm).ceil() as usize).min(e.len() / 2);
    if hi <= lo + 3 {
        return Vec::new();
    }
    let ac = |lag: usize| -> f64 { e.iter().zip(e.iter().skip(lag)).map(|(a, b)| (*a as f64) * (*b as f64)).sum::<f64>() / (e.len() - lag).max(1) as f64 };
    let zero = ac(0).max(1e-12);
    let raw: Vec<f64> = (lo - 1..=hi + 1).map(|l| ac(l) / zero).collect();
    let mut peaks: Vec<(f64, f64)> = Vec::new();
    for k in 1..raw.len().saturating_sub(1) {
        if raw[k] > raw[k - 1] && raw[k] >= raw[k + 1] && raw[k] > 0.0 {
            let (a, b, c) = (raw[k - 1], raw[k], raw[k + 1]);
            let den = a - 2.0 * b + c;
            let d = if den < 0.0 { (0.5 * (a - c) / den).clamp(-0.5, 0.5) } else { 0.0 };
            peaks.push(((lo - 1 + k) as f64 + d, b));
        }
    }
    peaks.sort_by(|a, b| b.1.total_cmp(&a.1));
    peaks.truncate(keep);
    peaks
}

/// Onset peaks of `env`: (frame, strength) of the local maxima that stand clearly above the rest.
fn onset_peaks(env: &[f32]) -> Vec<(usize, f64)> {
    let mean = env.iter().sum::<f32>() / env.len().max(1) as f32;
    let thr = (mean + 0.5).max(0.5);
    (2..env.len().saturating_sub(2)).filter(|&t| env[t] > thr && local_max(env, t, 2) == env[t]).map(|t| (t, env[t] as f64)).collect()
}

/// How well a rigid comb (`period` frames, first tooth at `phase`) explains the onset peaks:
/// (precision, recall, F-score). A tooth is a hit when a peak lies within 1.5 frames; recall is
/// weighted by the strength of the peaks.
fn comb_fit(peaks: &[(usize, f64)], total: f64, len: usize, period: f64, phase: f64) -> (f64, f64, f64) {
    let teeth = ((len as f64 - phase) / period).floor().max(1.0);
    let (mut hit_w, mut hit_teeth) = (0.0, 0usize);
    let mut last_tooth = -1i64;
    for (t, w) in peaks {
        let k = ((*t as f64 - phase) / period).round();
        let d = (*t as f64 - phase - k * period).abs();
        if d <= 1.5 && k >= 0.0 {
            hit_w += w;
            if k as i64 != last_tooth {
                hit_teeth += 1;
                last_tooth = k as i64;
            }
        }
    }
    let p = (hit_teeth as f64 / teeth).min(1.0);
    let r = if total > 0.0 { hit_w / total } else { 0.0 };
    let f = if p + r > 0.0 { 2.0 * p * r / (p + r) } else { 0.0 };
    (p, r, f)
}

/// The tempo near `period0` (frames) and the phase whose comb fits the onset peaks best:
/// (period, phase, precision, recall, F).
fn best_comb(peaks: &[(usize, f64)], len: usize, period0: f64) -> (f64, f64, f64, f64, f64) {
    let total: f64 = peaks.iter().map(|p| p.1).sum();
    let mut best = (period0, 0.0, 0.0, 0.0, -1.0);
    for step in -15..=15 {
        let period = period0 * (1.0 + step as f64 * 0.002);
        let mut phase = 0.0;
        while phase < period {
            let (p, r, f) = comb_fit(peaks, total, len, period, phase);
            if f > best.4 {
                best = (period, phase, p, r, f);
            }
            phase += 0.5;
        }
    }
    best
}

/// Least-squares line through the beat positions: (first beat, period) in the same unit.
fn fit_period(pos: &[f64]) -> Option<f64> {
    let n = pos.len() as f64;
    if pos.len() < 2 {
        return None;
    }
    let mx = (n - 1.0) / 2.0;
    let my = pos.iter().sum::<f64>() / n;
    let (mut num, mut den) = (0.0, 0.0);
    for (i, y) in pos.iter().enumerate() {
        num += (i as f64 - mx) * (y - my);
        den += (i as f64 - mx) * (i as f64 - mx);
    }
    (den > 0.0).then(|| num / den)
}

/// The attack of a beat near sample `at` when a clearly steeper one lies 25 to 60 ms away: the steepest
/// rise of the level (dB over 1 ms, 1.5 ms blocks every 0.5 ms). The onset envelope is coarse (10 ms
/// hops, 43 ms windows) and can lock on a click or a pre-hit a few frames before the real attack
/// (the hard cut of a note, a ghost note): when a rise at least 6 dB steeper than any within 25 ms of
/// `at` exists, it is the attack. Otherwise `at` is kept (within 25 ms the envelope is as good as
/// anything in a dense mix).
fn steeper_attack(x: &[f32], sr: u32, at: i64) -> i64 {
    let ms = sr as f64 / 1000.0;
    let block = ((1.5 * ms) as usize).max(4);
    let step = ((0.5 * ms) as usize).max(1);
    let from = (at - (60.0 * ms) as i64).max(0) as usize;
    let to = ((at + (60.0 * ms) as i64).max(0) as usize).min(x.len());
    if to <= from + 3 * block {
        return at;
    }
    let mut db: Vec<(usize, f32)> = Vec::new();
    let mut p = from;
    while p + block <= to {
        let e: f32 = x.get(p..p + block).unwrap_or_default().iter().map(|v| v * v).sum::<f32>() / block as f32;
        db.push((p, 10.0 * (e + 1e-9).log10()));
        p += step;
    }
    let lag = (1.0 * ms / step as f64).round().max(1.0) as usize;
    let (mut near, mut far): (f32, Option<(usize, f32)>) = (f32::MIN, None);
    for k in 0..db.len().saturating_sub(lag) {
        let rise = db[k + lag].1 - db[k].1;
        if (db[k].0 as f64 + block as f64 / 2.0 - at as f64).abs() <= 25.0 * ms {
            near = near.max(rise);
        } else if far.is_none_or(|b| rise > b.1) {
            far = Some((k, rise));
        }
    }
    match far {
        Some((k, rise)) if rise >= 6.0 && rise > near + 6.0 => (db[k].0 + block / 2) as i64,
        _ => at,
    }
}

/// Pull isolated outliers back onto the local beat line. The tracker can follow a click or a pre-hit
/// a few frames before the real attack for a few beats; a steady tempo means the beats lie on a
/// line, so each beat that is more than 15 ms from the robust line (Theil-Sen over the 8 beats on
/// each side) is put on that line. Beats within 15 ms keep their detected position.
fn regularise(beats: &mut [(usize, i64)], sr: u32) {
    let tol = 0.015 * sr as f64;
    let orig: Vec<f64> = beats.iter().map(|b| b.1 as f64).collect();
    let m = orig.len();
    for i in 0..m {
        let (lo, hi) = (i.saturating_sub(8), (i + 8).min(m - 1));
        let mut slopes: Vec<f64> = Vec::new();
        for a in lo..=hi {
            for b in a + 1..=hi {
                slopes.push((orig[b] - orig[a]) / (b - a) as f64);
            }
        }
        if slopes.len() < 3 {
            continue;
        }
        slopes.sort_by(f64::total_cmp);
        let slope = slopes[slopes.len() / 2];
        let mut icpts: Vec<f64> = (lo..=hi).map(|j| orig[j] - slope * j as f64).collect();
        icpts.sort_by(f64::total_cmp);
        let pred = slope * i as f64 + icpts[icpts.len() / 2];
        if (orig[i] - pred).abs() > tol {
            beats[i].1 = pred.round() as i64;
        }
    }
}

/// Edge beats that are not beats: the onset is much weaker than the typical beat (the tail of the
/// last note, the start of a fade-in), or the beat sits more than 15 ms off the line the other
/// beats make (a click after the last note, a note before the music starts). `beats` are
/// (onset frame, beat sample).
fn drop_weak_edges(env: &[f32], sr: u32, mut beats: Vec<(usize, i64)>) -> Vec<(usize, i64)> {
    let mut strengths: Vec<f32> = beats.iter().map(|&(t, _)| local_max(env, t, 1)).collect();
    strengths.sort_by(f32::total_cmp);
    let median = strengths.get(strengths.len() / 2).copied().unwrap_or(0.0);
    let tol = 0.015 * sr as f64;
    let off_line = |beats: &[(usize, i64)], idx: usize| -> bool {
        // the line through all the other beats
        let others: Vec<(f64, f64)> = beats.iter().enumerate().filter(|(i, _)| *i != idx).map(|(i, b)| (i as f64, b.1 as f64)).collect();
        let n = others.len() as f64;
        let (mx, my) = (others.iter().map(|p| p.0).sum::<f64>() / n, others.iter().map(|p| p.1).sum::<f64>() / n);
        let den: f64 = others.iter().map(|p| (p.0 - mx) * (p.0 - mx)).sum();
        if den <= 0.0 {
            return false;
        }
        let slope = others.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum::<f64>() / den;
        let at = my + slope * (idx as f64 - mx);
        (beats.get(idx).map_or(0, |b| b.1) as f64 - at).abs() > tol
    };
    for _ in 0..8 {
        let last = beats.len().saturating_sub(1);
        if beats.len() > 6 && (beats.last().is_some_and(|b| local_max(env, b.0, 1) < 0.5 * median) || off_line(&beats, last)) {
            beats.pop();
        } else if beats.len() > 6 && (beats.first().is_some_and(|b| local_max(env, b.0, 1) < 0.5 * median) || off_line(&beats, 0)) {
            beats.remove(0);
        } else {
            break;
        }
    }
    beats
}

/// Find the tempo and the beats of planar audio (channels are summed to mono).
pub fn analyze_grid(channels: &[&[f32]], sample_rate: u32, opts: GridOptions) -> Result<Grid, RemixError> {
    let len = channels.iter().map(|c| c.len()).max().unwrap_or(0);
    let k = 1.0 / channels.len().max(1) as f32;
    let mono: Vec<f32> = (0..len).map(|i| channels.iter().map(|c| c.get(i).copied().unwrap_or(0.0)).sum::<f32>() * k).collect();
    let (env, n, hop) = onset_envelope(&mono, sample_rate);
    let frame_rate = sample_rate as f64 / hop.max(1) as f64;
    let hint = opts.tempo_hint.filter(|h| h.is_finite() && (HINT_MIN..=HINT_MAX).contains(h));
    let (lo, hi) = if hint.is_some() { (HINT_MIN, HINT_MAX) } else { (TEMPO_MIN, TEMPO_MAX) };
    // peaks over a wide range so that the octaves of a peak are all known
    let peaks = ac_peaks(&env, frame_rate, 40.0, 340.0, 6);
    let strongest = peaks.first().map(|p| p.1).unwrap_or(0.0);
    if strongest < 0.1 {
        return Err(RemixError::NoBeat);
    }
    // candidate tempi
    let mut tempi: Vec<f64> = Vec::new();
    for (lag, _) in &peaks {
        let b = bpm_of(*lag, frame_rate);
        for m in [0.5, 1.0, 2.0] {
            tempi.push(b * m);
        }
    }
    let target = hint.map(|h| h.clamp(HINT_MIN, HINT_MAX));
    let mut cands: Vec<f64> = Vec::new();
    for b in tempi {
        let ok = match target {
            Some(h) => (b / h).log2().abs() < 0.12,
            None => (lo..=hi).contains(&b),
        };
        if ok && !cands.iter().any(|c| (c / b - 1.0).abs() < 0.04) {
            cands.push(b);
        }
    }
    if cands.is_empty() {
        if let Some(h) = target {
            cands.push(h);
        } else {
            return Err(RemixError::NoBeat);
        }
    }
    let peaks_on = onset_peaks(&env);
    let mut scored: Vec<(Candidate, Vec<usize>)> = Vec::new();
    for b in cands {
        let (period, _, prec, rec, f) = best_comb(&peaks_on, env.len(), 60.0 * frame_rate / b);
        let frames = trimmed_beats(&env, period, STIFFNESS);
        if frames.len() < 4 {
            continue;
        }
        let pos: Vec<f64> = frames.iter().map(|&t| t as f64).collect();
        let bpm = fit_period(&pos).filter(|p| *p > 0.0).map(|p| bpm_of(p, frame_rate)).unwrap_or(b);
        let score = f + if hint.is_some() { 0.0 } else { 0.5 * prior(bpm) };
        scored.push((Candidate { bpm, precision: prec, recall: rec, score }, frames));
    }
    scored.sort_by(|a, b| b.0.score.total_cmp(&a.0.score));
    let Some((best, frames)) = scored.first().cloned() else { return Err(RemixError::NoBeat) };
    // a pulse explains most of its own teeth and a good part of the onsets; a pad or noise does not
    // (with a hint the user says there is a pulse at that tempo: only the teeth must meet onsets)
    if best.precision < MIN_PRECISION || (hint.is_none() && best.recall < MIN_RECALL) {
        return Err(RemixError::NoBeat);
    }
    let candidates: Vec<Candidate> = scored.iter().map(|(c, _)| c.clone()).collect();
    // the flux of frame t peaks as soon as an attack enters the newer window, at about t·hop + n − ¾·hop
    let mut pairs: Vec<(usize, i64)> = frames
        .iter()
        .map(|&t| (t, (t * hop + n) as i64 - 3 * hop as i64 / 4))
        .filter(|&(_, b)| b > 0 && b < len as i64)
        .map(|(t, b)| (t, steeper_attack(&mono, sample_rate, b)))
        .collect();
    pairs.dedup_by_key(|p| p.1);
    let mut pairs = drop_weak_edges(&env, sample_rate, pairs);
    regularise(&mut pairs, sample_rate);
    let frames: Vec<usize> = pairs.iter().map(|p| p.0).collect();
    let beats: Vec<i64> = pairs.iter().map(|p| p.1).collect();
    if beats.len() < 4 {
        return Err(RemixError::NoBeat);
    }
    let mut grid = Grid { sample_rate, len: len as i64, beats, bpm: best.bpm, bar_start: None, bar_confidence: 0.0, candidates };
    if opts.downbeat {
        let (start, conf) = bar_start(&mono, sample_rate, &env, &frames, &grid.beats);
        grid.bar_start = Some(start);
        grid.bar_confidence = conf;
    }
    let _ = onset_frame;
    Ok(grid)
}

/// Which of the first four beats starts a bar: the phase whose beats carry the most accent (onset
/// strength), the most low-frequency weight (the kick) and the most harmonic change (chroma of the
/// beat against the one before). (phase, confidence 0 … 1).
fn bar_start(mono: &[f32], sr: u32, env: &[f32], frames: &[usize], beats: &[i64]) -> (usize, f64) {
    let (chroma, _) = beat_features(mono, sr, beats);
    let m = beats.len().min(chroma.len()).min(frames.len());
    if m < 8 {
        return (0, 0.0);
    }
    // low-passed copy (one pole, about 150 Hz): its energy in the 80 ms after each beat
    let a = 1.0 - (-std::f32::consts::TAU * 150.0 / sr as f32).exp();
    let mut y = 0f32;
    let low: Vec<f32> = mono
        .iter()
        .map(|v| {
            y += a * (v - y);
            y
        })
        .collect();
    let win = (0.08 * sr as f64) as usize;
    let weight: Vec<f64> = beats
        .iter()
        .take(m)
        .map(|&b| {
            let from = b.max(0) as usize;
            let seg = low.get(from..(from + win).min(low.len())).unwrap_or_default();
            (seg.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / seg.len().max(1) as f64 + 1e-12).ln()
        })
        .collect();
    let accent: Vec<f64> = frames.iter().take(m).map(|&t| local_max(env, t, 1) as f64).collect();
    let change: Vec<f64> = (0..m).map(|i| if i == 0 { 0.0 } else { 1.0 - cos_sim(&chroma[i - 1], &chroma[i]) as f64 }).collect();
    let z = |v: &[f64]| -> Vec<f64> {
        let mean = v.iter().sum::<f64>() / v.len().max(1) as f64;
        let sd = (v.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / v.len().max(1) as f64).sqrt().max(1e-9);
        v.iter().map(|x| (x - mean) / sd).collect()
    };
    let (za, zl, zc) = (z(&accent), z(&weight), z(&change));
    let score: Vec<f64> = (0..m).map(|i| (za[i] + zl[i] + zc[i]) / 3.0).collect();
    // mean score per phase; the confidence is the gap to the runner-up in standard errors
    let mut stat = [(0f64, 0f64, 0f64); 4]; // (count, sum, sum of squares)
    for (i, v) in score.iter().enumerate().skip(1) {
        let s = &mut stat[i % 4];
        s.0 += 1.0;
        s.1 += v;
        s.2 += v * v;
    }
    let mean = |s: &(f64, f64, f64)| s.1 / s.0.max(1.0);
    let var = |s: &(f64, f64, f64)| (s.2 / s.0.max(1.0) - mean(s) * mean(s)).max(1e-6);
    let mut order: Vec<usize> = (0..4).collect();
    order.sort_by(|a, b| mean(&stat[*b]).total_cmp(&mean(&stat[*a])));
    let (b, r) = (&stat[order[0]], &stat[order[1]]);
    let t = (mean(b) - mean(r)) / (var(b) / b.0.max(1.0) + var(r) / r.0.max(1.0)).sqrt();
    (order[0], (t / 4.0).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drums like a simple pattern: kick on 1 and 3, snare on 2 and 4, optional hats on the eighths.
    pub(crate) fn drums(sr: u32, bpm: f64, bars: usize, hats: bool, seed: u64, snare_gain: f32) -> (Vec<f32>, Vec<i64>) {
        let beat = sr as f64 * 60.0 / bpm;
        let n = (bars as f64 * 4.0 * beat) as usize;
        let mut x = vec![0f32; n];
        let mut s = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        let mut noise = move || {
            s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            ((s >> 40) as f32 / (1u32 << 24) as f32) * 2.0 - 1.0
        };
        let tau = std::f64::consts::TAU;
        let mut truth = Vec::new();
        for k in 0..bars * 4 {
            let st = (k as f64 * beat).round() as usize;
            truth.push(st as i64);
            let l = ((beat * 0.9) as usize).min(n.saturating_sub(st));
            for i in 0..l {
                let t = i as f64 / sr as f64;
                if k % 2 == 0 {
                    x[st + i] += (0.8 * (-t * 9.0).exp() * (tau * (50.0 + 90.0 * (-t * 25.0).exp()) * t).sin()) as f32;
                } else if t < 0.2 {
                    x[st + i] += snare_gain * 0.35 * (-t as f32 * 22.0).exp() * noise();
                }
                x[st + i] += (0.15 * (-t * 3.0).exp() * (tau * 110.0 * t).sin()) as f32;
            }
            if hats {
                for h in 0..2 {
                    let hs = st + (h as f64 * beat / 2.0) as usize;
                    for i in 0..(sr as usize / 25).min(n.saturating_sub(hs)) {
                        let t = i as f32 / sr as f32;
                        x[hs + i] += 0.08 * (-t * 120.0).exp() * noise();
                    }
                }
            }
        }
        x.iter_mut().for_each(|v| *v = (*v * 0.8).clamp(-1.0, 1.0));
        (x, truth)
    }

    fn err_ms(g: &Grid, truth: &[i64]) -> f64 {
        g.beats.iter().map(|b| truth.iter().map(|t| (t - b).abs()).min().unwrap_or(i64::MAX)).max().unwrap_or(0) as f64 * 1000.0 / g.sample_rate as f64
    }

    fn worst(g: &Grid, truth: &[i64]) -> (f64, usize) {
        let e = err_ms(g, truth);
        (e, g.beats.len())
    }

    /// Kick on 1 and 3 and snare on 2 and 4 (with or without hats), 90 … 160 BPM, three snare levels,
    /// two sample rates: the tempo octave is the right one, the beats are on the beats.
    #[test]
    fn drums_with_and_without_hats_get_the_right_tempo_octave() {
        let mut bad = Vec::new();
        let mut max_err: f64 = 0.0;
        for sr in [48_000u32, 44_100] {
            for hats in [true, false] {
                for snare in [1.0f32, 0.5, 1.6] {
                    for bpm in [90.0, 100.0, 110.0, 120.0, 128.0, 140.0, 150.0] {
                        let (x, truth) = drums(sr, bpm, 16, hats, 5, snare);
                        match analyze_grid(&[&x], sr, GridOptions::default()) {
                            Ok(g) => {
                                let (e, n) = worst(&g, &truth);
                                if (g.bpm / bpm - 1.0).abs() > 0.01 || n + 2 < truth.len() || n > truth.len() || e > 20.0 {
                                    bad.push(format!(
                                        "{bpm} BPM hats={hats} snare={snare} sr={sr}: found {:.2} BPM, {n} beats of {}, worst {e:.1} ms",
                                        g.bpm,
                                        truth.len()
                                    ));
                                }
                                max_err = max_err.max(e);
                            }
                            Err(e) => bad.push(format!("{bpm} BPM hats={hats} snare={snare} sr={sr}: {e}")),
                        }
                    }
                }
            }
        }
        eprintln!("worst beat error over the sweep: {max_err:.1} ms");
        assert!(bad.is_empty(), "{} failures:\n{}", bad.len(), bad.join("\n"));
    }

    #[test]
    fn full_music_with_bass_and_pads_keeps_its_tempo() {
        for sr in [22_050u32, 48_000] {
            for bpm in [90.0, 100.0, 120.0, 128.0, 140.0] {
                let (m, truth) = crate::remix::test_music(sr, bpm, 16, 3);
                let refs: Vec<&[f32]> = m.iter().map(Vec::as_slice).collect();
                let g = analyze_grid(&refs, sr, GridOptions::default()).expect("grid");
                let e = err_ms(&g, &truth);
                eprintln!("music {bpm} @ {sr}: {:.2} BPM, {} beats of {}, worst {e:.1} ms", g.bpm, g.beats.len(), truth.len());
                assert!((g.bpm / bpm - 1.0).abs() < 0.01, "{bpm}: {}", g.bpm);
                assert!(g.beats.len() + 2 >= truth.len() && g.beats.len() <= truth.len(), "{} beats of {}", g.beats.len(), truth.len());
                assert!(e < 20.0, "{bpm}: worst {e} ms");
            }
        }
    }

    #[test]
    fn nothing_rhythmic_is_refused() {
        let sr = 48_000u32;
        let n = sr as usize * 20;
        let mut s = 12345u64;
        let noise: Vec<f32> = (0..n)
            .map(|_| {
                s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
                ((s >> 40) as f32 / (1u32 << 24) as f32) * 2.0 - 1.0
            })
            .collect();
        let silence = vec![0f32; n];
        // a slow pad: two sines with a 7 s swell
        let pad: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f64 / sr as f64;
                (0.3 * (0.5 - 0.5 * (std::f64::consts::TAU * t / 7.0).cos())
                    * ((std::f64::consts::TAU * 220.0 * t).sin() + (std::f64::consts::TAU * 330.0 * t).sin())) as f32
            })
            .collect();
        for (name, x) in [("noise", &noise), ("silence", &silence), ("pad", &pad)] {
            let r = analyze_grid(&[x.as_slice()], sr, GridOptions::default());
            assert!(r.is_err(), "{name} gave {:?}", r.map(|g| (g.bpm, g.beats.len())));
        }
    }

    #[test]
    fn a_tempo_hint_picks_the_octave() {
        let sr = 48_000;
        let (x, _) = drums(sr, 120.0, 16, true, 5, 1.0);
        let by = |h: Option<f64>| analyze_grid(&[&x], sr, GridOptions { tempo_hint: h, downbeat: false }).expect("grid");
        assert!((by(None).bpm - 120.0).abs() < 1.0);
        let half = by(Some(60.0));
        assert!((half.bpm - 60.0).abs() < 1.0, "{}", half.bpm);
        let double = by(Some(240.0));
        assert!((double.bpm - 240.0).abs() < 2.0, "{}", double.bpm);
        assert!((by(Some(118.0)).bpm - 120.0).abs() < 1.0);
    }

    /// Chord changes mark the bars of the generated music: the bar start is found.
    /// Bare drums carry no bar information: the bar start lands on a kick (beat 1 or 3), half a bar off about half
    /// of the time, and the confidence says so.
    #[test]
    fn on_bare_drums_the_bar_start_is_a_kick_and_the_confidence_is_low() {
        for hats in [true, false] {
            for bpm in [90.0, 120.0, 128.0, 140.0] {
                let (x, truth) = drums(48_000, bpm, 16, hats, 5, 1.0);
                let g = analyze_grid(&[&x], 48_000, GridOptions { tempo_hint: None, downbeat: true }).unwrap();
                let first = g.beats[g.bar_start.unwrap_or(0)];
                let near = truth.iter().position(|t| (t - first).abs() < 2400).unwrap_or(usize::MAX);
                assert!(near % 2 == 0, "{bpm} BPM hats={hats}: the bar starts on a snare (beat {near})");
                assert!(g.bar_confidence < 0.6, "{bpm} BPM hats={hats}: confidence {} for a coin toss", g.bar_confidence);
            }
        }
    }

    #[test]
    fn bar_starts_follow_the_chords() {
        let sr = 22_050;
        let mut hit = 0;
        let mut total = 0;
        for bpm in [90.0, 100.0, 120.0, 128.0, 140.0] {
            let (m, truth) = crate::remix::test_music(sr, bpm, 16, 3);
            let refs: Vec<&[f32]> = m.iter().map(Vec::as_slice).collect();
            let g = analyze_grid(&refs, sr, GridOptions { tempo_hint: None, downbeat: true }).expect("grid");
            let first = g.beats[g.bar_start.unwrap_or(0)];
            let near = truth.iter().position(|t| (t - first).abs() < sr as i64 / 25).unwrap_or(usize::MAX);
            eprintln!("bar start of {bpm} BPM: beat {:?} -> true beat index {near} (confidence {:.2})", g.bar_start, g.bar_confidence);
            total += 1;
            if near % 4 == 0 {
                hit += 1;
            }
        }
        assert!(hit >= 4, "{hit}/{total}");
    }
}
