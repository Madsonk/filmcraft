//! L1-optimal camera path smoothing (Grundmann, Kwatra, Essa: "Auto-Directed Video Stabilization with
//! Robust L1 Optimal Camera Paths", CVPR 2011), re-implemented from the paper's formulation.
//!
//! For one parameter of the camera path `c` (translation, angle, log-scale per frame) the smooth path
//! `p` minimises `l1 |D1 p| + l2 |D2 p| + l3 |D3 p|` (velocity, acceleration, jerk: the optimum is made of
//! constant, linear and parabolic segments, i.e. a still camera, a pan, an ease in/out) subject to
//! `|p - c| <= bound` (the correction stays inside the crop window). This is a linear programme; it is
//! solved here with ADMM, whose only linear system is a symmetric banded matrix (bandwidth 3) factorised
//! once, so a path of n frames costs O(n) per iteration.

/// Weights of the velocity, acceleration and jerk terms (the paper's 10, 1, 100).
pub const WEIGHTS: [f64; 3] = [10.0, 1.0, 100.0];

/// Rows of the difference operators.
const D: [&[f64]; 3] = [&[-1.0, 1.0], &[1.0, -2.0, 1.0], &[-1.0, 3.0, -3.0, 1.0]];

/// Banded Cholesky factor (lower, bandwidth 3) of `I + sum Di^T Di`.
struct Band {
    l: Vec<[f64; 4]>,
}

impl Band {
    fn new(n: usize) -> Self {
        // m[i][d] = M[i][i-d]
        let mut m = vec![[0.0f64; 4]; n];
        for row in m.iter_mut() {
            row[0] = 1.0;
        }
        for d in D {
            if n < d.len() {
                continue;
            }
            for j in 0..=n - d.len() {
                for (a, ca) in d.iter().enumerate() {
                    for (b, cb) in d.iter().enumerate() {
                        if a >= b {
                            m[j + a][a - b] += ca * cb;
                        }
                    }
                }
            }
        }
        // Cholesky
        let mut l = vec![[0.0f64; 4]; n];
        for i in 0..n {
            for d in (0..=3usize.min(i)).rev() {
                let j = i - d;
                let mut s = m[i][d];
                // sum over columns c < j within both bands
                let lo = j.saturating_sub(3).max(i.saturating_sub(3));
                for c in lo..j {
                    s -= l[i][i - c] * l[j][j - c];
                }
                if d == 0 {
                    l[i][0] = s.max(1e-12).sqrt();
                } else {
                    l[i][d] = s / l[j][0];
                }
            }
        }
        Self { l }
    }

    fn solve(&self, b: &[f64]) -> Vec<f64> {
        let n = b.len();
        let mut y = vec![0.0; n];
        for i in 0..n {
            let mut s = b[i];
            for d in 1..=3usize.min(i) {
                s -= self.l[i][d] * y[i - d];
            }
            y[i] = s / self.l[i][0];
        }
        let mut x = vec![0.0; n];
        for i in (0..n).rev() {
            let mut s = y[i];
            for d in 1..=3usize {
                if i + d < n {
                    s -= self.l[i + d][d] * x[i + d];
                }
            }
            x[i] = s / self.l[i][0];
        }
        x
    }
}

fn soft(v: f64, t: f64) -> f64 {
    if v > t {
        v - t
    } else if v < -t {
        v + t
    } else {
        0.0
    }
}

fn diff(d: &[f64], p: &[f64]) -> Vec<f64> {
    if p.len() < d.len() {
        return Vec::new();
    }
    (0..=p.len() - d.len()).map(|j| d.iter().enumerate().map(|(a, c)| c * p[j + a]).sum()).collect()
}

/// `D^T v` for a difference operator of `n` columns.
fn diff_t(d: &[f64], v: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![0.0; n];
    for (j, x) in v.iter().enumerate() {
        for (a, c) in d.iter().enumerate() {
            out[j + a] += c * x;
        }
    }
    out
}

/// The smooth path for `c` staying within `bound` of it (`bound` > 0), `iters` ADMM iterations.
pub fn smooth(c: &[f64], bound: f64, weights: [f64; 3], iters: usize) -> Vec<f64> {
    let n = c.len();
    if n < 4 || bound <= 0.0 {
        return c.to_vec();
    }
    let band = Band::new(n);
    let mut p = c.to_vec();
    let mut q = c.to_vec();
    let mut uq = vec![0.0; n];
    let mut z: Vec<Vec<f64>> = D.iter().map(|d| diff(d, &p)).collect();
    let mut u: Vec<Vec<f64>> = z.iter().map(|v| vec![0.0; v.len()]).collect();
    // penalty parameter 1: thresholds are the weights
    for _ in 0..iters {
        let mut rhs: Vec<f64> = q.iter().zip(&uq).map(|(a, b)| a - b).collect();
        for k in 0..3 {
            let v: Vec<f64> = z[k].iter().zip(&u[k]).map(|(a, b)| a - b).collect();
            for (r, t) in rhs.iter_mut().zip(diff_t(D[k], &v, n)) {
                *r += t;
            }
        }
        p = band.solve(&rhs);
        for k in 0..3 {
            let dp = diff(D[k], &p);
            for j in 0..dp.len() {
                z[k][j] = soft(dp[j] + u[k][j], weights[k]);
                u[k][j] += dp[j] - z[k][j];
            }
        }
        for i in 0..n {
            q[i] = (p[i] + uq[i]).clamp(c[i] - bound, c[i] + bound);
            uq[i] += p[i] - q[i];
        }
    }
    // q satisfies the box exactly; p is the unconstrained-consensus solution: return q
    q
}

/// Smooth every column of `params` (frames x channels), channel `k` within `bounds[k]`.
pub fn smooth_params(params: &[[f64; 4]], bounds: [f64; 4], weights: [f64; 3], iters: usize) -> Vec<[f64; 4]> {
    let n = params.len();
    let mut out = vec![[0.0; 4]; n];
    for k in 0..4 {
        // normalise so the bound is 1 in every channel: weights then mean the same for all
        let b = bounds[k].max(1e-9);
        let col: Vec<f64> = params.iter().map(|p| p[k] / b).collect();
        let s = smooth(&col, 1.0, weights, iters);
        for i in 0..n {
            out[i][k] = s[i] * b;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jitter(n: usize) -> Vec<f64> {
        // a slow pan (0.5 px/frame) plus shake (deterministic pseudo-noise, +-6 px)
        (0..n)
            .map(|i| {
                let x = (i as f64 * 12.9898).sin() * 43758.5453;
                0.5 * i as f64 + 6.0 * (x - x.floor() - 0.5) * 2.0
            })
            .collect()
    }

    fn roughness(p: &[f64]) -> f64 {
        p.windows(3).map(|w| (w[0] - 2.0 * w[1] + w[2]).abs()).sum::<f64>() / (p.len() - 2) as f64
    }

    #[test]
    fn band_solve_matches_dense() {
        let n = 12;
        let band = Band::new(n);
        let b: Vec<f64> = (0..n).map(|i| (i as f64).sin() + 1.0).collect();
        let x = band.solve(&b);
        // (I + sum Di^T Di) x == b
        let mut ax = x.clone();
        for d in D {
            let dx = diff(d, &x);
            for (a, t) in ax.iter_mut().zip(diff_t(d, &dx, n)) {
                *a += t;
            }
        }
        for (a, b) in ax.iter().zip(&b) {
            assert!((a - b).abs() < 1e-9, "{a} vs {b}");
        }
    }

    #[test]
    fn stays_in_box_and_is_smoother() {
        let c = jitter(300);
        let bound = 8.0;
        let s = smooth(&c, bound, WEIGHTS, 400);
        assert!(s.iter().zip(&c).all(|(a, b)| (a - b).abs() <= bound + 1e-9));
        let (r0, r1) = (roughness(&c), roughness(&s));
        assert!(r1 < r0 / 8.0, "acceleration {r0} -> {r1}");
        // follows the pan: end-to-end drift preserved within the box
        assert!((s[299] - s[0] - 0.5 * 299.0).abs() < 2.0 * bound);
    }

    #[test]
    fn still_camera_stays_still_and_a_pan_gets_no_rougher() {
        // a camera that does not move is not touched
        let flat = vec![3.0; 40];
        assert!(smooth(&flat, 2.0, WEIGHTS, 200).iter().all(|v| (v - 3.0).abs() < 1e-3));
        // a steady pan: the result stays in the box and has no more total variation (the optimum may rest
        // inside the box instead of following the ramp: that is what the L1 objective prefers)
        let c: Vec<f64> = (0..50).map(|i| 0.3 * i as f64).collect();
        let s = smooth(&c, 5.0, WEIGHTS, 400);
        assert!(s.iter().zip(&c).all(|(a, b)| (a - b).abs() <= 5.0 + 1e-9));
        let tv = |p: &[f64]| p.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f64>();
        assert!(tv(&s) <= tv(&c) + 1e-6, "{} vs {}", tv(&s), tv(&c));
        // inputs too short to smooth come back unchanged
        assert_eq!(smooth(&[1.0, 2.0], 1.0, WEIGHTS, 10), vec![1.0, 2.0]);
    }
}
