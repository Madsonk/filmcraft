//! The crop window's path: from sparse subject samples to one smooth centre per frame.
//!
//! 1. The samples are laid on the frame grid (linear between detections, hold beyond the first and
//!    last; a gap of more than 3 s is bridged by holding each side up to its middle, not by a
//!    drift across the shot).
//! 2. **Dead zone**: the camera does not move while the subject stays inside a window around it
//!    (a fraction of the crop size); once the subject reaches the edge of that window the target
//!    follows it. Fidgeting and head-turns don't pan the frame.
//! 3. **Smoothing spline**: the path `x` minimising `Σ (x - target)² + λ Σ (Δ²x)²` is solved exactly
//!    (a symmetric pentadiagonal system, LDLᵀ). The acceleration penalty makes the pan start and
//!    stop gently; `λ = (smooth · fps)⁴` gives a cut-off period of `2π · smooth` seconds.
//! 4. The targets were clamped so the window stays inside the picture; the result is clamped
//!    again (the spline can overshoot a clamped target by a hair).

/// A subject position at a time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    /// Seconds from the start of the shot.
    pub t: f64,
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    /// Frames per second of the planned path.
    pub fps: f64,
    /// Smoothing time constant in seconds (0 = follow every sample).
    pub smooth: f64,
    /// Dead zone half-size as a fraction of the crop window (0 = none).
    pub dead_zone: f64,
    /// Where the subject should sit in the output frame, from the top (0.5 = middle). Below 0.5
    /// leaves room under the subject (subtitles); only matters when the window is shorter than the picture.
    pub aim_y: f64,
}

impl Default for Options {
    fn default() -> Self {
        Options { fps: 30.0, smooth: 0.3, dead_zone: 0.03, aim_y: 0.5 }
    }
}

/// The window centres, one per frame (`i / fps` seconds).
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub fps: f64,
    pub cx: Vec<f64>,
    pub cy: Vec<f64>,
}

/// Lays `samples` (sorted by time) on `n` frames; `None` when there are no samples.
fn lay_on_grid(samples: &[Sample], n: usize, fps: f64) -> Option<(Vec<f64>, Vec<f64>)> {
    let first = samples.first()?;
    let mut xs = Vec::with_capacity(n);
    let mut ys = Vec::with_capacity(n);
    let mut j = 0usize;
    for i in 0..n {
        let t = i as f64 / fps;
        while samples.get(j + 1).is_some_and(|s| s.t <= t) {
            j += 1;
        }
        let a = samples.get(j).unwrap_or(first);
        let (x, y) = match samples.get(j + 1) {
            Some(b) if t >= a.t => {
                let gap = b.t - a.t;
                if gap > 3.0 {
                    if t - a.t < b.t - t { (a.x, a.y) } else { (b.x, b.y) }
                } else {
                    let u = if gap > 0.0 { (t - a.t) / gap } else { 0.0 };
                    (a.x + (b.x - a.x) * u, a.y + (b.y - a.y) * u)
                }
            }
            _ => (a.x, a.y),
        };
        xs.push(x);
        ys.push(y);
    }
    Some((xs, ys))
}

/// Camera that holds still until the target leaves `±dz` around it.
fn dead_zone(target: &[f64], dz: f64) -> Vec<f64> {
    let mut out = Vec::with_capacity(target.len());
    let mut c = target.first().copied().unwrap_or(0.0);
    for &t in target {
        if t - c > dz {
            c = t - dz;
        } else if c - t > dz {
            c = t + dz;
        }
        out.push(c);
    }
    out
}

/// Solves `(I + λ D'D) x = b` for the second-difference operator `D`, in `O(n)`.
fn smooth_spline(b: &[f64], lambda: f64) -> Vec<f64> {
    let n = b.len();
    if n < 3 || lambda <= 0.0 {
        return b.to_vec();
    }
    // D'D is pentadiagonal: rows of the interior stencil [1 -4 6 -4 1], edges reduced
    let mut a0 = vec![0.0f64; n];
    let mut a1 = vec![0.0f64; n];
    let mut a2 = vec![0.0f64; n];
    for k in 0..n - 2 {
        // row k of D is [1, -2, 1] at columns k, k+1, k+2
        let w = [1.0, -2.0, 1.0];
        for p in 0..3 {
            for q in p..3 {
                let v = lambda * w[p] * w[q];
                match q - p {
                    0 => a0[k + p] += v,
                    1 => a1[k + p] += v,
                    _ => a2[k + p] += v,
                }
            }
        }
    }
    for v in a0.iter_mut() {
        *v += 1.0;
    }
    // LDL' with unit lower bands l1[i] = L[i+1][i], l2[i] = L[i+2][i]
    let mut d = vec![0.0f64; n];
    let mut l1 = vec![0.0f64; n];
    let mut l2 = vec![0.0f64; n];
    for i in 0..n {
        let (l1p, dp) = if i >= 1 { (l1[i - 1], d[i - 1]) } else { (0.0, 0.0) };
        let (l2pp, dpp) = if i >= 2 { (l2[i - 2], d[i - 2]) } else { (0.0, 0.0) };
        d[i] = a0[i] - l1p * l1p * dp - l2pp * l2pp * dpp;
        let l2p = if i >= 1 { l2[i - 1] } else { 0.0 };
        l1[i] = (a1[i] - l2p * l1p * dp) / d[i];
        l2[i] = a2[i] / d[i];
    }
    let mut y = vec![0.0f64; n];
    for i in 0..n {
        let mut v = b[i];
        if i >= 1 {
            v -= l1[i - 1] * y[i - 1];
        }
        if i >= 2 {
            v -= l2[i - 2] * y[i - 2];
        }
        y[i] = v;
    }
    let mut x = vec![0.0f64; n];
    for i in (0..n).rev() {
        let mut v = y[i] / d[i];
        if i + 1 < n {
            v -= l1[i] * x[i + 1];
        }
        if i + 2 < n {
            v -= l2[i] * x[i + 2];
        }
        x[i] = v;
    }
    x
}

fn follow(target: &[f64], dz: f64, lo: f64, hi: f64, lambda: f64) -> Vec<f64> {
    if hi <= lo {
        return vec![(lo + hi) * 0.5; target.len()];
    }
    let held: Vec<f64> = dead_zone(target, dz).into_iter().map(|v| v.clamp(lo, hi)).collect();
    smooth_spline(&held, lambda).into_iter().map(|v| v.clamp(lo, hi)).collect()
}

/// Plans the window centres for a shot of `duration` seconds. `frame` and `crop` are in picture
/// pixels. With no sample the window stays centred.
pub fn plan(samples: &[Sample], duration: f64, frame: (f64, f64), crop: (f64, f64), opts: &Options) -> Plan {
    let fps = opts.fps.max(1.0);
    let n = ((duration.max(0.0) * fps).ceil() as usize).max(1) + 1;
    let (fw, fh) = frame;
    let (cw, ch) = (crop.0.min(fw), crop.1.min(fh));
    let Some((xs, ys)) = lay_on_grid(samples, n, fps) else {
        return Plan { fps, cx: vec![fw / 2.0; n], cy: vec![fh / 2.0; n] };
    };
    let lambda = (opts.smooth.max(0.0) * fps).powi(4);
    // vertical: put the subject at `aim_y` of the window
    let ys: Vec<f64> = ys.into_iter().map(|y| y + (0.5 - opts.aim_y) * ch).collect();
    Plan { fps, cx: follow(&xs, opts.dead_zone * cw, cw / 2.0, fw - cw / 2.0, lambda), cy: follow(&ys, opts.dead_zone * ch, ch / 2.0, fh - ch / 2.0, lambda) }
}

/// `(mean, max)` of the absolute jerk (third derivative) of a path sampled at `fps`, in units/s³.
pub fn jerk(path: &[f64], fps: f64) -> (f64, f64) {
    if path.len() < 4 {
        return (0.0, 0.0);
    }
    let k = fps * fps * fps;
    let v: Vec<f64> = path.windows(4).map(|w| ((w[3] - 3.0 * w[2] + 3.0 * w[1] - w[0]) * k).abs()).collect();
    (v.iter().sum::<f64>() / v.len() as f64, v.iter().copied().fold(0.0, f64::max))
}

/// Frame indices to keyframe: every `every` frames and the last, minus the ones the straight line
/// between their neighbours reproduces, frame by frame, within `tol` (in the units of `path`).
pub fn knots(path: &[f64], every: usize, tol: f64) -> Vec<usize> {
    let n = path.len();
    if n == 0 {
        return Vec::new();
    }
    let mut k: Vec<usize> = (0..n).step_by(every.max(1)).collect();
    if k.last() != Some(&(n - 1)) {
        k.push(n - 1);
    }
    let mut i = 1;
    while i + 1 < k.len() {
        let (a, c) = (k[i - 1], k[i + 1]);
        // the knot goes when the line between its neighbours reproduces every frame in between
        let fits = (a..=c).all(|j| {
            let u = (j - a) as f64 / (c - a) as f64;
            (path[j] - (path[a] + (path[c] - path[a]) * u)).abs() < tol
        });
        if fits {
            k.remove(i);
        } else {
            i += 1;
        }
    }
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spline_with_zero_penalty_is_identity_and_with_a_large_one_is_a_line() {
        let b: Vec<f64> = (0..50).map(|i| (i as f64 * 0.7).sin()).collect();
        assert_eq!(smooth_spline(&b, 0.0), b);
        let ramp: Vec<f64> = (0..50).map(|i| 3.0 + 2.0 * i as f64 + if i % 2 == 0 { 0.5 } else { -0.5 }).collect();
        let s = smooth_spline(&ramp, 1e9);
        for (i, v) in s.iter().enumerate() {
            assert!((v - (3.0 + 2.0 * i as f64)).abs() < 0.2, "{i}: {v}");
        }
    }

    #[test]
    fn dead_zone_ignores_small_moves_and_follows_big_ones() {
        let t: Vec<f64> = vec![0.0, 3.0, -3.0, 4.0, 10.0, 20.0];
        let d = dead_zone(&t, 5.0);
        assert_eq!(d[..4], [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(d[4], 5.0);
        assert_eq!(d[5], 15.0);
    }

    #[test]
    fn plan_stays_inside_the_picture_and_centres_without_samples() {
        let p = plan(&[], 2.0, (1920.0, 1080.0), (607.5, 1080.0), &Options::default());
        assert!(p.cx.iter().all(|v| (*v - 960.0).abs() < 1e-9));
        let s = [Sample { t: 0.0, x: 50.0, y: 500.0 }, Sample { t: 2.0, x: 1900.0, y: 500.0 }];
        let p = plan(&s, 2.0, (1920.0, 1080.0), (607.5, 1080.0), &Options { dead_zone: 0.0, smooth: 0.1, ..Options::default() });
        let (lo, hi) = (607.5 / 2.0, 1920.0 - 607.5 / 2.0);
        assert!(p.cx.iter().all(|v| *v >= lo - 1e-9 && *v <= hi + 1e-9));
        assert!(p.cx[0] <= lo + 1.0 && p.cx[p.cx.len() - 1] >= hi - 1.0);
    }

    #[test]
    fn knots_drop_what_a_line_reproduces() {
        let line: Vec<f64> = (0..100).map(|i| i as f64 * 2.0).collect();
        assert_eq!(knots(&line, 5, 0.1), vec![0, 99]);
        let bend: Vec<f64> = (0..100).map(|i| if i < 50 { 0.0 } else { (i - 50) as f64 }).collect();
        assert_eq!(knots(&bend, 5, 0.1), vec![0, 50, 99], "the bend stays a knot");
    }

    #[test]
    fn jerk_of_a_cubic_is_constant() {
        let p: Vec<f64> = (0..20).map(|i| (i as f64 / 30.0).powi(3)).collect();
        let (mean, max) = jerk(&p, 30.0);
        assert!((mean - 6.0).abs() < 1e-6 && (max - 6.0).abs() < 1e-6);
    }
}
