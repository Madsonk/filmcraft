//! YOLOX person detection and YuNet face detection.

use filmcraft_nn::{Model, Tensor};

use crate::{Detection, Image, ReframeError, Result};

/// Letterboxed network input: BGR planes, 0 … 255, plus the scale that maps network pixels back.
struct Prepared {
    chw: Vec<f32>,
    /// network pixel / picture pixel
    ratio: f32,
}

/// Box-filter reduction by an integer factor (keeps small subjects from aliasing away).
fn reduce(img: &Image, k: usize) -> (usize, usize, Vec<f32>) {
    let (w, h) = (img.width / k, img.height / k);
    let mut out = vec![0f32; w * h * 3];
    let norm = 1.0 / (k * k) as f32;
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0f32; 3];
            for dy in 0..k {
                for dx in 0..k {
                    let i = ((y * k + dy) * img.width + x * k + dx) * 4;
                    for (c, a) in acc.iter_mut().enumerate() {
                        *a += f32::from(img.rgba.get(i + c).copied().unwrap_or(0));
                    }
                }
            }
            for (c, a) in acc.iter().enumerate() {
                if let Some(o) = out.get_mut((y * w + x) * 3 + c) {
                    *o = a * norm;
                }
            }
        }
    }
    (w, h, out)
}

/// Bilinear resize of interleaved RGB floats with half-pixel centres.
fn resize(src: &[f32], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<f32> {
    let mut out = vec![0f32; dw * dh * 3];
    let (fx, fy) = (sw as f32 / dw as f32, sh as f32 / dh as f32);
    for y in 0..dh {
        let sy = ((y as f32 + 0.5) * fy - 0.5).clamp(0.0, (sh - 1) as f32);
        let (y0, wy) = (sy.floor() as usize, sy - sy.floor());
        let y1 = (y0 + 1).min(sh - 1);
        for x in 0..dw {
            let sx = ((x as f32 + 0.5) * fx - 0.5).clamp(0.0, (sw - 1) as f32);
            let (x0, wx) = (sx.floor() as usize, sx - sx.floor());
            let x1 = (x0 + 1).min(sw - 1);
            for c in 0..3 {
                let p = |yy: usize, xx: usize| src.get((yy * sw + xx) * 3 + c).copied().unwrap_or(0.0);
                let top = p(y0, x0) * (1.0 - wx) + p(y0, x1) * wx;
                let bot = p(y1, x0) * (1.0 - wx) + p(y1, x1) * wx;
                if let Some(o) = out.get_mut((y * dw + x) * 3 + c) {
                    *o = top * (1.0 - wy) + bot * wy;
                }
            }
        }
    }
    out
}

/// Fit `img` in a `size × size` square (aspect kept, top-left aligned), pad with `pad`, BGR planes.
fn letterbox(img: &Image, size: usize, pad: f32) -> Result<Prepared> {
    if img.width == 0 || img.height == 0 || img.rgba.len() < img.width * img.height * 4 {
        return Err(ReframeError("empty or truncated picture".into()));
    }
    let ratio = (size as f32 / img.height as f32).min(size as f32 / img.width as f32);
    let k = ((1.0 / ratio).floor() as usize).max(1);
    let (sw, sh, base) = if k > 1 {
        reduce(img, k)
    } else {
        (
            img.width,
            img.height,
            (0..img.width * img.height)
                .flat_map(|i| (0..3).map(move |c| (i, c)))
                .map(|(i, c)| f32::from(img.rgba.get(i * 4 + c).copied().unwrap_or(0)))
                .collect(),
        )
    };
    let nw = ((img.width as f32 * ratio).round() as usize).clamp(1, size);
    let nh = ((img.height as f32 * ratio).round() as usize).clamp(1, size);
    let scaled = if (nw, nh) == (sw, sh) { base } else { resize(&base, sw, sh, nw, nh) };
    let plane = size * size;
    let mut chw = vec![pad; 3 * plane];
    for y in 0..nh {
        for x in 0..nw {
            for c in 0..3 {
                // network order is B, G, R; the picture is R, G, B
                let v = scaled.get((y * nw + x) * 3 + (2 - c)).copied().unwrap_or(pad);
                if let Some(o) = chw.get_mut(c * plane + y * size + x) {
                    *o = v;
                }
            }
        }
    }
    Ok(Prepared { chw, ratio })
}

/// Greedy non-maximum suppression, best score first.
pub fn nms(mut dets: Vec<Detection>, iou: f32) -> Vec<Detection> {
    dets.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut keep: Vec<Detection> = Vec::new();
    for d in dets {
        if keep.iter().all(|k| k.iou(&d) < iou) {
            keep.push(d);
        }
    }
    keep
}

/// YOLOX (COCO, 80 classes): only the `person` class is returned.
pub struct Yolox {
    model: Model,
    size: usize,
}

impl Yolox {
    /// `bytes` is a `yolox_*.onnx` export; the input size is read from the output length.
    pub fn from_bytes(bytes: &[u8]) -> Result<Yolox> {
        let model = Model::parse(bytes)?;
        Ok(Yolox { model, size: 0 })
    }

    /// Network input side: 416 for Nano and Tiny, 640 for S and larger.
    pub fn with_size(mut self, size: usize) -> Yolox {
        self.size = size;
        self
    }

    pub fn size(&self) -> usize {
        if self.size == 0 { 416 } else { self.size }
    }

    pub fn detect(&self, img: &Image, min_score: f32) -> Result<Vec<Detection>> {
        let s = self.size();
        let prep = letterbox(img, s, 114.0)?;
        let input = Tensor::f32(vec![1, 3, s, s], prep.chw)?;
        let name = self.model.input_names().first().cloned().unwrap_or_else(|| "images".into());
        let out = self.model.run(vec![(name.as_str(), input)])?;
        let t = out.first().ok_or_else(|| ReframeError("YOLOX returned nothing".into()))?;
        let (n, per) = match t.shape.as_slice() {
            [1, n, per] => (*n, *per),
            other => return Err(ReframeError(format!("unexpected YOLOX output shape {other:?}"))),
        };
        let v = t.as_f32()?;
        // grids of strides 8, 16, 32 concatenated
        let mut cells: Vec<(f32, f32, f32)> = Vec::with_capacity(n);
        for stride in [8usize, 16, 32] {
            let g = s / stride;
            for gy in 0..g {
                for gx in 0..g {
                    cells.push((gx as f32, gy as f32, stride as f32));
                }
            }
        }
        if cells.len() != n || per < 6 {
            return Err(ReframeError(format!("YOLOX output has {n} cells, expected {} for a {s} px input", cells.len())));
        }
        let mut dets = Vec::new();
        for (i, (gx, gy, st)) in cells.iter().enumerate() {
            let r = v.get(i * per..(i + 1) * per).unwrap_or(&[]);
            let (Some(&x), Some(&y), Some(&w), Some(&h), Some(&obj), Some(&person)) = (r.first(), r.get(1), r.get(2), r.get(3), r.get(4), r.get(5)) else {
                continue;
            };
            let score = obj * person;
            if score < min_score {
                continue;
            }
            let (cx, cy) = ((x + gx) * st, (y + gy) * st);
            let (bw, bh) = (w.exp() * st, h.exp() * st);
            let k = 1.0 / prep.ratio;
            let clampx = |v: f32| v.clamp(0.0, img.width as f32);
            let clampy = |v: f32| v.clamp(0.0, img.height as f32);
            dets.push(Detection {
                x0: clampx((cx - bw / 2.0) * k),
                y0: clampy((cy - bh / 2.0) * k),
                x1: clampx((cx + bw / 2.0) * k),
                y1: clampy((cy + bh / 2.0) * k),
                score,
            });
        }
        Ok(nms(dets, 0.45))
    }
}

/// YuNet face detector (OpenCV Zoo, 2023 March, 640 × 640 input).
pub struct Yunet {
    model: Model,
}

impl Yunet {
    pub const SIZE: usize = 640;

    pub fn from_bytes(bytes: &[u8]) -> Result<Yunet> {
        Ok(Yunet { model: Model::parse(bytes)? })
    }

    pub fn detect(&self, img: &Image, min_score: f32) -> Result<Vec<Detection>> {
        let s = Self::SIZE;
        let prep = letterbox(img, s, 0.0)?;
        let input = Tensor::f32(vec![1, 3, s, s], prep.chw)?;
        let name = self.model.input_names().first().cloned().unwrap_or_else(|| "input".into());
        let outs = self.model.run(vec![(name.as_str(), input)])?;
        let get = |n: &str| -> Result<&Tensor> {
            self.model.output_names().iter().position(|x| x == n).and_then(|i| outs.get(i)).ok_or_else(|| ReframeError(format!("YuNet output {n} missing")))
        };
        let mut dets = Vec::new();
        for stride in [8usize, 16, 32] {
            let cols = s / stride;
            let cls = get(&format!("cls_{stride}"))?.as_f32()?;
            let obj = get(&format!("obj_{stride}"))?.as_f32()?;
            let bb = get(&format!("bbox_{stride}"))?.as_f32()?;
            if cls.len() != cols * cols || obj.len() != cols * cols || bb.len() != cols * cols * 4 {
                return Err(ReframeError("unexpected YuNet output sizes".into()));
            }
            for idx in 0..cols * cols {
                let (r, c) = (idx / cols, idx % cols);
                let score = (cls[idx].clamp(0.0, 1.0) * obj[idx].clamp(0.0, 1.0)).sqrt();
                if score < min_score {
                    continue;
                }
                let st = stride as f32;
                let (cx, cy) = ((c as f32 + bb[idx * 4]) * st, (r as f32 + bb[idx * 4 + 1]) * st);
                let (w, h) = (bb[idx * 4 + 2].exp() * st, bb[idx * 4 + 3].exp() * st);
                let k = 1.0 / prep.ratio;
                dets.push(Detection {
                    x0: ((cx - w / 2.0) * k).clamp(0.0, img.width as f32),
                    y0: ((cy - h / 2.0) * k).clamp(0.0, img.height as f32),
                    x1: ((cx + w / 2.0) * k).clamp(0.0, img.width as f32),
                    y1: ((cy + h / 2.0) * k).clamp(0.0, img.height as f32),
                    score,
                });
            }
        }
        Ok(nms(dets, 0.3))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(x0: f32, y0: f32, x1: f32, y1: f32, s: f32) -> Detection {
        Detection { x0, y0, x1, y1, score: s }
    }

    #[test]
    fn nms_keeps_the_best_of_overlapping_boxes() {
        let r = nms(vec![d(0.0, 0.0, 10.0, 10.0, 0.6), d(1.0, 1.0, 11.0, 11.0, 0.9), d(50.0, 50.0, 60.0, 60.0, 0.5)], 0.45);
        assert_eq!(r.len(), 2);
        assert!((r[0].score - 0.9).abs() < 1e-6);
    }

    #[test]
    fn letterbox_keeps_aspect_and_puts_bgr_in_order() {
        // 4 x 2 picture, left half red, right half blue
        let mut rgba = Vec::new();
        for _y in 0..2 {
            for x in 0..4 {
                rgba.extend_from_slice(if x < 2 { &[255, 0, 0, 255] } else { &[0, 0, 255, 255] });
            }
        }
        let img = Image { width: 4, height: 2, rgba: &rgba };
        let p = letterbox(&img, 8, 114.0).expect("letterbox");
        assert!((p.ratio - 2.0).abs() < 1e-6);
        let plane = 64;
        // pixel (0,0) is red: B = 0, G = 0, R = 255
        assert_eq!((p.chw[0], p.chw[plane], p.chw[2 * plane]), (0.0, 0.0, 255.0));
        // rows below the 4-row picture are padding
        assert_eq!(p.chw[7 * 8], 114.0);
    }
}
