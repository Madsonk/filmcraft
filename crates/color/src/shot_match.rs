//! Shot-to-shot colour matching ("calage couleur"): make shot B look like shot A by measuring the
//! colour statistics of a few sampled frames of each and baking the correction into a 3D LUT.
//!
//! Clean-room implementation from the published papers (no third-party code was read):
//!
//! * **Reinhard** — E. Reinhard, M. Ashikhmin, B. Gooch, P. Shirley, *Color Transfer between
//!   Images*, IEEE Computer Graphics and Applications 21(5), 2001. Per-channel mean and standard
//!   deviation transfer in a decorrelated perceptual space: `out = (x − μ_t)·σ_r/σ_t + μ_r`. The
//!   paper uses Ruderman's lαβ; we use **Oklab** (B. Ottosson, *A perceptual color space for image
//!   processing*, 2020), which plays the same role and is what the rest of FilmCraft measures in.
//! * **Monge–Kantorovich linear (MKL)** — F. Pitié, A. Kokaram, *The linear Monge-Kantorovitch
//!   linear colour mapping for example-based colour transfer*, IET CVMP 2007. The affine map
//!   `out = T(x − μ_t) + μ_r` with `T = Σ_t^{-1/2} (Σ_t^{1/2} Σ_r Σ_t^{1/2})^{1/2} Σ_t^{-1/2}`, which
//!   carries the target's covariance exactly onto the reference's with the least displacement.
//!   Here too in Oklab; matrix square roots by Jacobi eigen-decomposition of the symmetric 3×3.
//! * **Histogram** — classic per-channel histogram specification (cumulative distribution matching,
//!   e.g. Gonzalez & Woods, *Digital Image Processing*, § "Histogram matching"): each Oklab channel
//!   of the target is mapped through `F_r⁻¹(F_t(x))`, with 257 quantiles per channel.
//!
//! Samples are **display-referred values** (the grading signal: sRGB-encoded Rec.709, 0…1) — the
//! same values a Lumetri Creative Look LUT receives — so the baked LUT plugs into
//! `lumetri.setLook`. Crushed blacks and clipped whites are left out of the statistics
//! ([`usable`]). The LUT is `strength`-mixed with the identity in the signal domain.

use crate::lut::Lut3d;
use crate::{linear_to_srgb, srgb_to_linear};

/// The grid size of the baked LUT (33³ = 35 937 entries, the common "33-point cube").
pub const LUT_SIZE: usize = 33;
/// Fewer usable samples than this on either side and the statistics are not trusted.
pub const MIN_SAMPLES: usize = 64;
/// Samples above this per side are decimated (statistics don't need more).
pub const MAX_SAMPLES: usize = 400_000;
/// A sample whose brightest channel is at or below this is a crushed black.
pub const BLACK: f32 = 0.02;
/// A sample with any channel at or above this is a clipped white.
pub const WHITE: f32 = 0.985;
const QUANTILES: usize = 256;
/// Bounds on Reinhard's σ_r / σ_t (a flat target channel must not explode).
const MAX_GAIN: f32 = 4.0;
/// Regularisation added to the MKL covariances (≈ a standard deviation of 0.003 in Oklab).
const COV_EPS: f64 = 1e-5;

/// A matching method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Reinhard,
    Mkl,
    Histogram,
}

impl Method {
    pub const ALL: [Method; 3] = [Method::Reinhard, Method::Mkl, Method::Histogram];

    pub fn id(self) -> &'static str {
        match self {
            Method::Reinhard => "reinhard",
            Method::Mkl => "mkl",
            Method::Histogram => "histogram",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Method::Reinhard => "Mean and spread (Reinhard)",
            Method::Mkl => "Linear transport 3×3 (Monge–Kantorovich)",
            Method::Histogram => "Histogram per channel",
        }
    }

    pub fn parse(s: &str) -> Option<Method> {
        Method::ALL.into_iter().find(|m| m.id().eq_ignore_ascii_case(s))
    }
}

/// Linear Rec.709 / sRGB-primaries RGB → Oklab.
pub fn oklab(c: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = c.map(|v| v.max(0.0));
    let l = 0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b;
    let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
    let s = 0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b;
    let (l, m, s) = (l.cbrt(), m.cbrt(), s.cbrt());
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

/// Oklab → linear Rec.709 RGB (may be negative / above 1 outside the gamut).
pub fn oklab_to_linear(c: [f32; 3]) -> [f32; 3] {
    let [ll, a, b] = c;
    let l = ll + 0.396_337_78 * a + 0.215_803_76 * b;
    let m = ll - 0.105_561_346 * a - 0.063_854_17 * b;
    let s = ll - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l, m, s) = (l * l * l, m * m * m, s * s * s);
    [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ]
}

/// Display signal (sRGB-encoded, 0…1) → Oklab.
pub fn signal_to_oklab(v: [f32; 3]) -> [f32; 3] {
    oklab(v.map(|q| srgb_to_linear(q.clamp(0.0, 1.0))))
}

/// Oklab → display signal, clipped to 0…1.
pub fn oklab_to_signal(c: [f32; 3]) -> [f32; 3] {
    oklab_to_linear(c).map(|q| linear_to_srgb(q.max(0.0)).clamp(0.0, 1.0))
}

/// Whether a display-signal sample takes part in the statistics: finite, not a crushed black
/// (brightest channel ≤ [`BLACK`]) and not a clipped white (a channel ≥ [`WHITE`]).
pub fn usable(v: [f32; 3]) -> bool {
    v.iter().all(|q| q.is_finite()) && v.iter().copied().fold(f32::MIN, f32::max) > BLACK && v.iter().all(|&q| q < WHITE)
}

/// The usable samples of a set of display-signal values, in Oklab (decimated past
/// [`MAX_SAMPLES`]).
pub fn samples(signal: &[[f32; 3]]) -> Vec<[f32; 3]> {
    let step = signal.len().div_ceil(MAX_SAMPLES).max(1);
    signal.iter().step_by(step).copied().filter(|v| usable(*v)).map(signal_to_oklab).collect()
}

/// Mean of Oklab samples (zero for none).
pub fn mean(lab: &[[f32; 3]]) -> [f32; 3] {
    let n = lab.len().max(1) as f64;
    let mut m = [0f64; 3];
    for p in lab {
        for k in 0..3 {
            m[k] += p[k] as f64;
        }
    }
    m.map(|v| (v / n) as f32)
}

/// Distance between the mean colours of two sample sets, in Oklab units × 100 (so 1 ≈ a just
/// noticeable difference, like a CIE ΔE).
pub fn mean_distance(a: &[[f32; 3]], b: &[[f32; 3]]) -> f32 {
    let (ma, mb) = (mean(a), mean(b));
    100.0 * ((ma[0] - mb[0]).powi(2) + (ma[1] - mb[1]).powi(2) + (ma[2] - mb[2]).powi(2)).sqrt()
}

type M3 = [[f64; 3]; 3];

fn covariance(lab: &[[f32; 3]], mu: [f32; 3]) -> M3 {
    let n = lab.len().max(1) as f64;
    let mut c = [[0f64; 3]; 3];
    for p in lab {
        let d = [(p[0] - mu[0]) as f64, (p[1] - mu[1]) as f64, (p[2] - mu[2]) as f64];
        for i in 0..3 {
            for j in 0..3 {
                c[i][j] += d[i] * d[j];
            }
        }
    }
    for (i, row) in c.iter_mut().enumerate() {
        for v in row.iter_mut() {
            *v /= n;
        }
        row[i] += COV_EPS;
    }
    c
}

fn mul(a: &M3, b: &M3) -> M3 {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}

/// Eigen-decomposition of a symmetric 3×3 (cyclic Jacobi): eigenvalues and eigenvectors as the
/// columns of `v`.
fn sym_eigen(m: &M3) -> ([f64; 3], M3) {
    let mut a = *m;
    let mut v: M3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for _sweep in 0..50 {
        let off = a[0][1].abs() + a[0][2].abs() + a[1][2].abs();
        if off < 1e-18 {
            break;
        }
        for (p, q) in [(0usize, 1usize), (0, 2), (1, 2)] {
            if a[p][q].abs() < 1e-300 {
                continue;
            }
            let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
            let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
            let t = if theta == 0.0 { 1.0 } else { t };
            let c = 1.0 / (t * t + 1.0).sqrt();
            let s = t * c;
            // A ← Jᵀ A J
            for k in 0..3 {
                let (akp, akq) = (a[k][p], a[k][q]);
                a[k][p] = c * akp - s * akq;
                a[k][q] = s * akp + c * akq;
            }
            for k in 0..3 {
                let (apk, aqk) = (a[p][k], a[q][k]);
                a[p][k] = c * apk - s * aqk;
                a[q][k] = s * apk + c * aqk;
            }
            for row in v.iter_mut() {
                let (vkp, vkq) = (row[p], row[q]);
                row[p] = c * vkp - s * vkq;
                row[q] = s * vkp + c * vkq;
            }
        }
    }
    ([a[0][0], a[1][1], a[2][2]], v)
}

/// `V · diag(f(λ)) · Vᵀ` of a symmetric matrix.
fn sym_fn(m: &M3, f: impl Fn(f64) -> f64) -> M3 {
    let (w, v) = sym_eigen(m);
    let d = w.map(|x| f(x.max(COV_EPS * 1e-3)));
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| v[i][k] * d[k] * v[j][k]).sum()))
}

/// Pitié & Kokaram's linear Monge–Kantorovich matrix from the target covariance `st` to the
/// reference covariance `sr`.
fn mkl_matrix(st: &M3, sr: &M3) -> M3 {
    let st_h = sym_fn(st, f64::sqrt);
    let st_ih = sym_fn(st, |x| 1.0 / x.sqrt());
    let inner = mul(&mul(&st_h, sr), &st_h);
    // symmetrise against rounding before the square root
    let inner: M3 = std::array::from_fn(|i| std::array::from_fn(|j| 0.5 * (inner[i][j] + inner[j][i])));
    let mid = sym_fn(&inner, f64::sqrt);
    mul(&mul(&st_ih, &mid), &st_ih)
}

/// `QUANTILES + 1` quantiles of one channel.
fn quantiles(lab: &[[f32; 3]], ch: usize) -> Vec<f32> {
    let mut v: Vec<f32> = lab.iter().map(|p| p[ch]).filter(|q| q.is_finite()).collect();
    v.sort_by(f32::total_cmp);
    let n = v.len();
    (0..=QUANTILES)
        .map(|i| {
            let pos = i as f64 / QUANTILES as f64 * n.saturating_sub(1) as f64;
            let lo = pos.floor() as usize;
            let hi = (lo + 1).min(n.saturating_sub(1));
            let f = (pos - lo as f64) as f32;
            match (v.get(lo), v.get(hi)) {
                (Some(a), Some(b)) => a + (b - a) * f,
                _ => 0.0,
            }
        })
        .collect()
}

/// Map `x` through `F_r⁻¹(F_t(x))` given the quantile tables (beyond the measured range the map
/// continues with slope 1 from its end points).
fn histogram_map(src: &[f32], dst: &[f32], x: f32) -> f32 {
    let (Some(&s0), Some(&s1), Some(&d0), Some(&d1)) = (src.first(), src.last(), dst.first(), dst.last()) else { return x };
    if x <= s0 {
        return d0 + (x - s0);
    }
    if x >= s1 {
        return d1 + (x - s1);
    }
    // first quantile above x
    let i = src.partition_point(|&q| q <= x).clamp(1, src.len().saturating_sub(1));
    let (a, b) = (src.get(i - 1).copied().unwrap_or(s0), src.get(i).copied().unwrap_or(s1));
    let f = if b - a > 1e-9 { (x - a) / (b - a) } else { 0.0 };
    let (da, db) = (dst.get(i - 1).copied().unwrap_or(d0), dst.get(i).copied().unwrap_or(d1));
    da + (db - da) * f
}

#[derive(Clone, Debug, PartialEq)]
enum Transfer {
    Reinhard { mu_t: [f32; 3], mu_r: [f32; 3], gain: [f32; 3] },
    Mkl { mu_t: [f32; 3], mu_r: [f32; 3], t: [[f32; 3]; 3] },
    Histogram { src: [Vec<f32>; 3], dst: [Vec<f32>; 3] },
}

/// A solved shot match: maps the target shot's colours onto the reference's.
#[derive(Clone, Debug, PartialEq)]
pub struct ShotMatch {
    pub method: Method,
    /// Usable samples on each side.
    pub target_samples: usize,
    pub reference_samples: usize,
    transfer: Transfer,
}

/// Solve the match from Oklab samples (see [`samples`]) of the target and the reference shots.
pub fn solve(target: &[[f32; 3]], reference: &[[f32; 3]], method: Method) -> Result<ShotMatch, String> {
    if target.len() < MIN_SAMPLES || reference.len() < MIN_SAMPLES {
        return Err(format!(
            "not enough usable pixels to measure the shots (target {}, reference {}, need {MIN_SAMPLES}); the frames may be black or clipped",
            target.len(),
            reference.len()
        ));
    }
    let (mu_t, mu_r) = (mean(target), mean(reference));
    let transfer = match method {
        Method::Reinhard => {
            let sd = |lab: &[[f32; 3]], mu: [f32; 3], k: usize| {
                let n = lab.len().max(1) as f64;
                (lab.iter().map(|p| ((p[k] - mu[k]) as f64).powi(2)).sum::<f64>() / n).sqrt() as f32
            };
            let gain = std::array::from_fn(|k| {
                let (st, sr) = (sd(target, mu_t, k), sd(reference, mu_r, k));
                if st > 1e-6 { (sr / st).clamp(1.0 / MAX_GAIN, MAX_GAIN) } else { 1.0 }
            });
            Transfer::Reinhard { mu_t, mu_r, gain }
        }
        Method::Mkl => {
            let t = mkl_matrix(&covariance(target, mu_t), &covariance(reference, mu_r));
            if t.iter().flatten().any(|v| !v.is_finite()) {
                return Err("the colour statistics are degenerate (could not solve the 3×3 transport)".into());
            }
            Transfer::Mkl { mu_t, mu_r, t: t.map(|r| r.map(|v| v as f32)) }
        }
        Method::Histogram => Transfer::Histogram { src: std::array::from_fn(|k| quantiles(target, k)), dst: std::array::from_fn(|k| quantiles(reference, k)) },
    };
    Ok(ShotMatch { method, target_samples: target.len(), reference_samples: reference.len(), transfer })
}

impl ShotMatch {
    /// Map one Oklab colour.
    pub fn map_lab(&self, c: [f32; 3]) -> [f32; 3] {
        match &self.transfer {
            Transfer::Reinhard { mu_t, mu_r, gain } => std::array::from_fn(|k| (c[k] - mu_t[k]) * gain[k] + mu_r[k]),
            Transfer::Mkl { mu_t, mu_r, t } => {
                let d = [c[0] - mu_t[0], c[1] - mu_t[1], c[2] - mu_t[2]];
                std::array::from_fn(|i| t[i][0] * d[0] + t[i][1] * d[1] + t[i][2] * d[2] + mu_r[i])
            }
            Transfer::Histogram { src, dst } => std::array::from_fn(|k| histogram_map(&src[k], &dst[k], c[k])),
        }
    }

    /// Map one display-signal value (0…1 in, 0…1 out).
    pub fn map_signal(&self, v: [f32; 3]) -> [f32; 3] {
        oklab_to_signal(self.map_lab(signal_to_oklab(v)))
    }

    /// Bake the match into a `size³` LUT on the display signal, mixed with the identity by
    /// `strength` (0 = identity, 1 = the full match).
    pub fn lut(&self, size: usize, strength: f32, title: &str) -> Lut3d {
        let size = size.clamp(2, 256);
        let k = if strength.is_finite() { strength.clamp(0.0, 1.0) } else { 1.0 };
        let mut cube = Lut3d::from_fn(size, |v| {
            if k <= 0.0 {
                return v;
            }
            let m = self.map_signal(v);
            std::array::from_fn(|i| v[i] + (m[i] - v[i]) * k)
        });
        cube.title = title.to_string();
        cube
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lut::Lut;

    /// Deterministic pseudo-random display-signal pixels of a varied scene (skin-ish, foliage,
    /// sky, greys), kept inside 0.08…0.8 so a moderate cast never clips.
    fn scene(n: usize, seed: u64) -> Vec<[f32; 3]> {
        let mut x = seed;
        let mut rnd = move || {
            x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            ((x >> 33) as f32) / (1u64 << 31) as f32
        };
        let bases = [[0.62, 0.45, 0.36], [0.25, 0.42, 0.2], [0.45, 0.6, 0.78], [0.4, 0.4, 0.4], [0.7, 0.66, 0.6], [0.15, 0.14, 0.16]];
        (0..n)
            .map(|i| {
                let b = bases[i % bases.len()];
                let j = 0.12 * (rnd() - 0.5);
                [0, 1, 2].map(|k| (b[k] + j + 0.06 * (rnd() - 0.5)).clamp(0.08, 0.8))
            })
            .collect()
    }

    /// Apply a camera-like cast in linear light: per-channel gains and an exposure.
    fn cast(px: &[[f32; 3]], gains: [f32; 3], exposure: f32) -> Vec<[f32; 3]> {
        px.iter().map(|v| std::array::from_fn(|k| linear_to_srgb((srgb_to_linear(v[k]) * gains[k] * exposure).max(0.0)).clamp(0.0, 1.0))).collect()
    }

    fn through(lut: &Lut3d, px: &[[f32; 3]]) -> Vec<[f32; 3]> {
        px.iter().map(|v| lut.apply(*v)).collect()
    }

    #[test]
    fn oklab_round_trips() {
        for c in [[0.2, 0.5, 0.9], [1.0, 1.0, 1.0], [0.01, 0.3, 0.02], [0.0, 0.0, 0.0]] {
            let back = oklab_to_linear(oklab(c));
            for k in 0..3 {
                assert!((back[k] - c[k]).abs() < 2e-4, "{c:?} → {back:?}");
            }
        }
        // white has L = 1, a = b = 0
        let w = oklab([1.0; 3]);
        assert!((w[0] - 1.0).abs() < 1e-3 && w[1].abs() < 1e-3 && w[2].abs() < 1e-3, "{w:?}");
    }

    #[test]
    fn crushed_and_clipped_pixels_are_left_out() {
        assert!(!usable([0.0, 0.01, 0.02]));
        assert!(!usable([0.5, 0.99, 0.4]));
        assert!(!usable([f32::NAN, 0.5, 0.5]));
        assert!(usable([0.5, 0.4, 0.3]));
        let px = vec![[0.0; 3], [1.0; 3], [0.5, 0.4, 0.3]];
        assert_eq!(samples(&px).len(), 1);
    }

    #[test]
    fn same_reference_and_target_is_the_identity() {
        let px = scene(6000, 7);
        let lab = samples(&px);
        for m in Method::ALL {
            let sm = solve(&lab, &lab, m).unwrap();
            let lut = sm.lut(LUT_SIZE, 1.0, "id");
            let worst = lut
                .data
                .iter()
                .enumerate()
                .map(|(i, o)| {
                    let n = (LUT_SIZE - 1) as f32;
                    let v = [(i % LUT_SIZE) as f32 / n, (i / LUT_SIZE % LUT_SIZE) as f32 / n, (i / (LUT_SIZE * LUT_SIZE)) as f32 / n];
                    (0..3).map(|k| (o[k] - v[k]).abs()).fold(0f32, f32::max)
                })
                .fold(0f32, f32::max);
            // the identity up to the Oklab round trip (and out-of-gamut nodes clipping to 0…1)
            assert!(worst < 2e-3, "{m:?}: worst deviation {worst}");
        }
    }

    #[test]
    fn a_known_cast_is_matched() {
        let base = scene(8000, 11);
        // reference: neutral-ish; target: a warm, darker camera (like a flat profile shot)
        let reference = cast(&base, [1.0, 1.0, 1.0], 1.0);
        let target = cast(&base, [1.25, 1.0, 0.72], 0.8);
        let (lt, lr) = (samples(&target), samples(&reference));
        let before = mean_distance(&lt, &lr);
        assert!(before > 3.0, "the synthetic cast is visible: {before}");
        for m in Method::ALL {
            let lut = solve(&lt, &lr, m).unwrap().lut(LUT_SIZE, 1.0, "t");
            let after = mean_distance(&samples(&through(&lut, &target)), &lr);
            assert!(after < before * 0.1, "{m:?}: {before:.2} → {after:.2}");
            eprintln!("{m:?}: mean distance {before:.2} → {after:.2}");
        }
    }

    #[test]
    fn a_green_shift_with_more_contrast_is_matched() {
        let base = scene(8000, 3);
        let reference = cast(&base, [0.95, 1.0, 1.08], 1.1);
        let target = cast(&base, [0.85, 1.12, 0.9], 0.9);
        let (lt, lr) = (samples(&target), samples(&reference));
        let before = mean_distance(&lt, &lr);
        for m in Method::ALL {
            let lut = solve(&lt, &lr, m).unwrap().lut(LUT_SIZE, 1.0, "t");
            let after = samples(&through(&lut, &target));
            let d = mean_distance(&after, &lr);
            assert!(d < before * 0.15, "{m:?}: {before:.2} → {d:.2}");
            // the spread of lightness follows the reference too
            let sd = |v: &[[f32; 3]]| {
                let mu = mean(v)[0];
                (v.iter().map(|p| (p[0] - mu).powi(2)).sum::<f32>() / v.len() as f32).sqrt()
            };
            assert!((sd(&after) - sd(&lr)).abs() < 0.25 * (sd(&lt) - sd(&lr)).abs().max(0.01), "{m:?}");
        }
    }

    #[test]
    fn strength_zero_is_the_identity_and_half_is_half_way() {
        let base = scene(4000, 5);
        let (lt, lr) = (samples(&cast(&base, [1.2, 1.0, 0.8], 1.0)), samples(&base));
        let sm = solve(&lt, &lr, Method::Reinhard).unwrap();
        let zero = sm.lut(LUT_SIZE, 0.0, "zero");
        assert_eq!(zero.data, Lut3d::identity(LUT_SIZE).data);
        let (full, half) = (sm.lut(9, 1.0, "f"), sm.lut(9, 0.5, "h"));
        let id = Lut3d::identity(9);
        for i in 0..id.data.len() {
            for k in 0..3 {
                let want = id.data[i][k] + 0.5 * (full.data[i][k] - id.data[i][k]);
                assert!((half.data[i][k] - want).abs() < 1e-6);
            }
        }
        // hostile strength values do not escape 0…1
        assert_eq!(sm.lut(9, -3.0, "n").data, id.data);
        assert_eq!(sm.lut(9, f32::NAN, "nan").data, full.data);
    }

    #[test]
    fn the_cube_file_is_well_formed() {
        let base = scene(4000, 9);
        let (lt, lr) = (samples(&cast(&base, [1.1, 0.95, 0.85], 0.9)), samples(&base));
        for m in Method::ALL {
            let cube = solve(&lt, &lr, m).unwrap().lut(LUT_SIZE, 0.8, "FilmCraft shot match \"A\" → B");
            let text = cube.to_cube();
            assert!(text.contains("LUT_3D_SIZE 33"), "{m:?}");
            assert!(text.starts_with("TITLE \""), "{m:?}");
            let rows = text.lines().filter(|l| l.split_whitespace().count() == 3 && l.split_whitespace().all(|w| w.parse::<f32>().is_ok())).count();
            assert_eq!(rows, 33 * 33 * 33, "{m:?}");
            let back = Lut::parse_cube(&text).unwrap();
            let c = back.cube.unwrap();
            assert_eq!(c.size, 33);
            assert!(c.data.iter().flatten().all(|v| (0.0..=1.0).contains(v)), "{m:?}");
            // round trip through the file keeps the values (6 decimals)
            for (a, b) in c.data.iter().zip(&cube.data) {
                for k in 0..3 {
                    assert!((a[k] - b[k]).abs() < 1e-5);
                }
            }
        }
    }

    #[test]
    fn too_few_samples_or_flat_shots_fail_or_stay_bounded() {
        let few = samples(&scene(10, 1));
        assert!(solve(&few, &few, Method::Reinhard).is_err());
        // a perfectly flat target: Reinhard keeps a gain of 1, MKL stays finite, nothing is NaN
        let flat = vec![signal_to_oklab([0.4, 0.4, 0.4]); 500];
        let varied = samples(&scene(2000, 2));
        for m in Method::ALL {
            let sm = solve(&flat, &varied, m).unwrap();
            let lut = sm.lut(9, 1.0, "flat");
            assert!(lut.data.iter().flatten().all(|v| v.is_finite() && (0.0..=1.0).contains(v)), "{m:?}");
        }
        assert_eq!(Method::parse("MKL"), Some(Method::Mkl));
        assert_eq!(Method::parse("nope"), None);
    }

    #[test]
    fn jacobi_square_root_squares_back() {
        let m: M3 = [[0.04, 0.01, -0.002], [0.01, 0.003, 0.0005], [-0.002, 0.0005, 0.006]];
        let r = sym_fn(&m, f64::sqrt);
        let rr = mul(&r, &r);
        for i in 0..3 {
            for j in 0..3 {
                assert!((rr[i][j] - m[i][j]).abs() < 1e-9, "{rr:?}");
            }
        }
        // MKL carries the target covariance onto the reference covariance: T Σt Tᵀ = Σr
        let st: M3 = [[0.02, 0.003, 0.0], [0.003, 0.004, 0.001], [0.0, 0.001, 0.005]];
        let t = mkl_matrix(&st, &m);
        let tt: M3 = std::array::from_fn(|i| std::array::from_fn(|j| t[j][i]));
        let got = mul(&mul(&t, &st), &tt);
        for i in 0..3 {
            for j in 0..3 {
                assert!((got[i][j] - m[i][j]).abs() < 1e-8, "{got:?}");
            }
        }
    }
}
