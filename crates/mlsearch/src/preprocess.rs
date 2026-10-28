//! SigLIP's image preprocessing: the photo is stretched (aspect ratio not kept) to 224 × 224 with
//! the resampling the checkpoint was prepared with, then scaled to −1..1. The resampling is
//! Pillow's (a separable convolution whose kernel widens when shrinking, so it antialiases), each
//! pass rounded to 8 bits as Pillow does, which is what the reference preprocessing runs.

use crate::model::{IMAGE, Resample};

/// One axis of Pillow's resampling: for each output pixel, the first input pixel and the weights.
fn coefficients(input: usize, output: usize, kind: Resample) -> Vec<(usize, Vec<f32>)> {
    let scale = input as f64 / output as f64;
    let filter_scale = scale.max(1.0);
    let support = match kind {
        Resample::Bilinear => 1.0,
        Resample::Bicubic => 2.0,
    } * filter_scale;
    let kernel = |x: f64| -> f64 {
        match kind {
            Resample::Bilinear => (1.0 - x.abs()).max(0.0),
            Resample::Bicubic => {
                let (a, x) = (-0.5, x.abs());
                if x < 1.0 {
                    ((a + 2.0) * x - (a + 3.0)) * x * x + 1.0
                } else if x < 2.0 {
                    (((x - 5.0) * x + 8.0) * x - 4.0) * a
                } else {
                    0.0
                }
            }
        }
    };
    (0..output)
        .map(|o| {
            let center = (o as f64 + 0.5) * scale;
            let lo = ((center - support + 0.5).floor().max(0.0)) as usize;
            let hi = (((center + support + 0.5).floor()).min(input as f64)) as usize;
            let mut w: Vec<f64> = (lo..hi.max(lo + 1)).map(|i| kernel((i as f64 - center + 0.5) / filter_scale)).collect();
            let sum: f64 = w.iter().sum();
            if sum.abs() > 1e-12 {
                w.iter_mut().for_each(|v| *v /= sum);
            }
            (lo, w.into_iter().map(|v| v as f32).collect())
        })
        .collect()
}

fn round8(v: f32) -> f32 {
    v.round().clamp(0.0, 255.0)
}

/// `rgb`: 8-bit sRGB, interleaved, `w × h` → `3 × 224 × 224` floats (CHW, −1..1). `None` when
/// the buffer doesn't match its size.
pub fn preprocess(rgb: &[u8], w: usize, h: usize, kind: Resample) -> Option<Vec<f32>> {
    if w == 0 || h == 0 || rgb.len() != w * h * 3 {
        return None;
    }
    let cx = coefficients(w, IMAGE, kind);
    let cy = coefficients(h, IMAGE, kind);
    // horizontal pass: h rows × IMAGE columns × 3
    let mut tmp = vec![0f32; h * IMAGE * 3];
    for y in 0..h {
        let row = &rgb[y * w * 3..(y + 1) * w * 3];
        for (x, (lo, ws)) in cx.iter().enumerate() {
            let mut acc = [0f32; 3];
            for (k, wk) in ws.iter().enumerate() {
                let p = (lo + k).min(w - 1) * 3;
                for c in 0..3 {
                    acc[c] += wk * f32::from(row[p + c]);
                }
            }
            for c in 0..3 {
                tmp[(y * IMAGE + x) * 3 + c] = round8(acc[c]);
            }
        }
    }
    let mut out = vec![0f32; 3 * IMAGE * IMAGE];
    for (y, (lo, ws)) in cy.iter().enumerate() {
        for x in 0..IMAGE {
            let mut acc = [0f32; 3];
            for (k, wk) in ws.iter().enumerate() {
                let r = (lo + k).min(h - 1);
                for c in 0..3 {
                    acc[c] += wk * tmp[(r * IMAGE + x) * 3 + c];
                }
            }
            for c in 0..3 {
                out[c * IMAGE * IMAGE + y * IMAGE + x] = round8(acc[c]) / 127.5 - 1.0;
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flat_colour_stays_flat_and_is_scaled_to_minus_one_one() {
        for kind in [Resample::Bilinear, Resample::Bicubic] {
            let img: Vec<u8> = std::iter::repeat_n([255u8, 0, 51], 300 * 200).flatten().collect();
            let out = preprocess(&img, 300, 200, kind).unwrap();
            assert_eq!(out.len(), 3 * IMAGE * IMAGE);
            assert!(out[..IMAGE * IMAGE].iter().all(|v| (*v - 1.0).abs() < 1e-6));
            assert!(out[IMAGE * IMAGE..2 * IMAGE * IMAGE].iter().all(|v| (*v + 1.0).abs() < 1e-6));
            assert!(out[2 * IMAGE * IMAGE..].iter().all(|v| (*v - (51.0 / 127.5 - 1.0)).abs() < 1e-6));
        }
    }

    #[test]
    fn a_wrong_buffer_size_is_refused() {
        assert!(preprocess(&[0; 10], 2, 2, Resample::Bilinear).is_none());
        assert!(preprocess(&[], 0, 0, Resample::Bilinear).is_none());
    }
}
