//! Windowed-sinc sample-rate conversion (offline, whole buffer).

/// Resamples `x` from `from` Hz to `to` Hz with a Hann-windowed sinc (16 zero crossings each side).
pub(crate) fn resample(x: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || x.is_empty() || from == 0 || to == 0 {
        return x.to_vec();
    }
    let ratio = f64::from(to) / f64::from(from);
    let n_out = ((x.len() as f64) * ratio).round() as usize;
    let cutoff = ratio.min(1.0);
    const ZC: f64 = 16.0;
    let half = ZC / cutoff;
    let pi = std::f64::consts::PI;
    (0..n_out)
        .map(|j| {
            let pos = j as f64 / ratio;
            let lo = (pos - half).ceil().max(0.0) as usize;
            let hi = ((pos + half).floor() as usize).min(x.len().saturating_sub(1));
            let mut acc = 0f64;
            for i in lo..=hi {
                let d = i as f64 - pos;
                let a = d * cutoff;
                let sinc = if a.abs() < 1e-12 { 1.0 } else { (pi * a).sin() / (pi * a) };
                let w = 0.5 + 0.5 * (pi * d / half).cos();
                acc += f64::from(x.get(i).copied().unwrap_or(0.0)) * sinc * w * cutoff;
            }
            acc as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_a_tone_and_the_length() {
        let x: Vec<f32> = (0..4410).map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 44100.0).sin()).collect();
        let y = resample(&x, 44100, 48000);
        assert_eq!(y.len(), 4800);
        for (j, v) in y.iter().enumerate().skip(100).take(4600) {
            let want = (2.0 * std::f32::consts::PI * 440.0 * j as f32 / 48000.0).sin();
            assert!((v - want).abs() < 2e-3, "{j}");
        }
    }
}
