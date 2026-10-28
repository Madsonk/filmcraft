//! A mixed-radix complex FFT for the sizes DeepFilterNet uses (960 = 2^6 · 3 · 5), in `f64`.

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct C {
    pub re: f64,
    pub im: f64,
}

impl C {
    pub fn new(re: f64, im: f64) -> C {
        C { re, im }
    }
    fn mul(self, o: C) -> C {
        C { re: self.re * o.re - self.im * o.im, im: self.re * o.im + self.im * o.re }
    }
    fn add(self, o: C) -> C {
        C { re: self.re + o.re, im: self.im + o.im }
    }
}

pub(crate) struct Fft {
    n: usize,
    factors: Vec<usize>,
    /// `exp(-2πi k / n)`
    tw: Vec<C>,
}

impl Fft {
    pub fn new(n: usize) -> Fft {
        let n = n.max(1);
        let mut factors = Vec::new();
        let mut m = n;
        let mut p = 2;
        while m > 1 {
            if m.is_multiple_of(p) {
                factors.push(p);
                m /= p;
            } else {
                p += if p == 2 { 1 } else { 2 };
                if p * p > m && m > 1 {
                    factors.push(m);
                    break;
                }
            }
        }
        let tw = (0..n)
            .map(|k| {
                let a = -2.0 * std::f64::consts::PI * k as f64 / n as f64;
                C::new(a.cos(), a.sin())
            })
            .collect();
        Fft { n, factors, tw }
    }

    /// `X[k] = Σ x[j] e^{-2πi jk/n}` (unnormalised).
    pub fn forward(&self, x: &[C]) -> Vec<C> {
        let mut out = vec![C::default(); self.n];
        if x.len() == self.n {
            self.rec(x, 1, self.n, &mut out, 0);
        }
        out
    }

    /// `x[j] = Σ X[k] e^{+2πi jk/n}` (unnormalised).
    pub fn inverse(&self, x: &[C]) -> Vec<C> {
        let conj: Vec<C> = x.iter().map(|c| C::new(c.re, -c.im)).collect();
        let mut y = self.forward(&conj);
        for c in &mut y {
            c.im = -c.im;
        }
        y
    }

    fn rec(&self, input: &[C], stride: usize, n: usize, out: &mut [C], level: usize) {
        if n == 1 {
            if let (Some(o), Some(i)) = (out.first_mut(), input.first()) {
                *o = *i;
            }
            return;
        }
        let p = self.factors.get(level).copied().unwrap_or(n);
        let m = n / p;
        for r in 0..p {
            if let (Some(sub_in), Some(sub_out)) = (input.get(r * stride..), out.get_mut(r * m..(r + 1) * m)) {
                self.rec(sub_in, stride * p, m, sub_out, level + 1);
            }
        }
        let step = self.n / n;
        let mut t = vec![C::default(); p];
        for k in 0..m {
            for (r, slot) in t.iter_mut().enumerate() {
                let v = out.get(r * m + k).copied().unwrap_or_default();
                let w = self.tw.get((r * k * step) % self.n).copied().unwrap_or_default();
                *slot = v.mul(w);
            }
            for q in 0..p {
                let mut acc = C::default();
                for (r, v) in t.iter().enumerate() {
                    let w = self.tw.get((r * q * m * step) % self.n).copied().unwrap_or_default();
                    acc = acc.add(v.mul(w));
                }
                if let Some(o) = out.get_mut(k + q * m) {
                    *o = acc;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dft(x: &[C]) -> Vec<C> {
        let n = x.len();
        (0..n)
            .map(|k| {
                let mut acc = C::default();
                for (j, v) in x.iter().enumerate() {
                    let a = -2.0 * std::f64::consts::PI * ((j * k) % n) as f64 / n as f64;
                    acc = acc.add(v.mul(C::new(a.cos(), a.sin())));
                }
                acc
            })
            .collect()
    }

    #[test]
    fn matches_the_direct_transform_for_960_and_odd_sizes() {
        for n in [960usize, 15, 7, 12, 1, 2, 49] {
            let x: Vec<C> = (0..n).map(|i| C::new((i as f64 * 0.37).sin(), (i as f64 * 0.11).cos())).collect();
            let f = Fft::new(n);
            let (a, b) = (f.forward(&x), dft(&x));
            for (p, q) in a.iter().zip(&b) {
                assert!((p.re - q.re).abs() < 1e-8 && (p.im - q.im).abs() < 1e-8, "n={n}");
            }
            let back = f.inverse(&a);
            for (p, q) in back.iter().zip(&x) {
                assert!((p.re / n as f64 - q.re).abs() < 1e-9 && (p.im / n as f64 - q.im).abs() < 1e-9, "n={n}");
            }
        }
    }
}
