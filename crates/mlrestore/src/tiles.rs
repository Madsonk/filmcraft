//! Running a fully convolutional / windowed network over an image of any size in overlapping
//! tiles, so that memory stays bounded and the tiles leave no seams.
//!
//! The image is cut in tiles of at most `tile` px (a multiple of the network's block size) that
//! overlap their neighbours by `overlap` px. Each tile is padded to the network's block size by
//! replicating its border, run, cropped, and added to the output with a linear ramp over the
//! overlap (the outer `overlap` px of a tile are where the network sees the least context, so
//! they weigh least); the output is the weighted mean. Tiles at the image's own border are not
//! ramped on that side. Peak memory is the input, the output and a weight plane, plus one tile's
//! activations: independent of the image size apart from those three buffers.

use candle_core::{Device, Tensor};
use rayon::prelude::*;

use crate::{Error, Image, Result};

/// Tiling parameters, in input pixels.
#[derive(Clone, Copy, Debug)]
pub struct Tiling {
    /// Longest side of a tile.
    pub tile: usize,
    /// Overlap of two neighbouring tiles.
    pub overlap: usize,
    /// Tile sizes are padded to a multiple of this (64 for SCUNet, 1 for SPAN).
    pub multiple: usize,
}

/// Start offsets of the tiles along a side of `len` px.
fn starts(len: usize, tile: usize, overlap: usize) -> Vec<usize> {
    if len <= tile {
        return vec![0];
    }
    let stride = (tile - overlap).max(1);
    let mut v: Vec<usize> = (0..).map(|i| i * stride).take_while(|&s| s + tile < len).collect();
    v.push(len - tile);
    v
}

/// Weight of local coordinate `i` (of `n`) in a tile with neighbours on the given sides.
fn ramp(i: usize, n: usize, overlap: usize, before: bool, after: bool) -> f32 {
    let o = overlap.max(1) as f32;
    let a = if before { ((i as f32 + 0.5) / o).min(1.0) } else { 1.0 };
    let b = if after { (((n - i) as f32 - 0.5) / o).min(1.0) } else { 1.0 };
    a.min(b)
}

/// Run `net` over `img` in tiles; `net` maps `[1, 3, h, w]` (0..1) to `[1, 3, h * scale, w * scale]`.
/// `progress(done, total)` is called after each tile; returning `false` cancels.
pub fn run(
    img: &Image,
    scale: usize,
    t: Tiling,
    dev: &Device,
    net: &dyn Fn(&Tensor) -> Result<Tensor>,
    progress: &mut dyn FnMut(usize, usize) -> bool,
) -> Result<Image> {
    let (w, h) = (img.width, img.height);
    if w == 0 || h == 0 || img.data.len() != w * h * 3 {
        return Err(Error::Image(format!("{w} x {h} image with {} samples", img.data.len())));
    }
    let tile = t.tile.max(t.multiple);
    let overlap = t.overlap.min(tile / 2);
    let (xs, ys) = (starts(w, tile, overlap), starts(h, tile, overlap));
    let (ow, oh) = (w * scale, h * scale);
    let mut out = vec![0f32; ow * oh * 3];
    let mut wsum = vec![0f32; ow * oh];
    let total = xs.len() * ys.len();
    let mut done = 0;
    for &y0 in &ys {
        for &x0 in &xs {
            let (tw, th) = ((w - x0).min(tile), (h - y0).min(tile));
            let (pw, ph) = (tw.div_ceil(t.multiple) * t.multiple, th.div_ceil(t.multiple) * t.multiple);
            // planar, border-replicated to the padded size
            let mut inp = vec![0f32; 3 * pw * ph];
            inp.par_chunks_mut(pw * ph).enumerate().for_each(|(c, plane)| {
                for y in 0..ph {
                    let sy = y0 + y.min(th - 1);
                    for x in 0..pw {
                        plane[y * pw + x] = img.data[(sy * w + x0 + x.min(tw - 1)) * 3 + c];
                    }
                }
            });
            let x = Tensor::from_vec(inp, (1, 3, ph, pw), dev)?;
            let y = net(&x)?;
            let (sw, sh) = (pw * scale, ph * scale);
            let y = y.reshape((3, sh, sw))?.flatten_all()?.to_vec1::<f32>()?;
            let (cw, ch) = (tw * scale, th * scale);
            let (before_x, after_x, before_y, after_y) = (x0 > 0, x0 + tw < w, y0 > 0, y0 + th < h);
            let wx: Vec<f32> = (0..cw).map(|i| ramp(i, cw, overlap * scale, before_x, after_x)).collect();
            let wy: Vec<f32> = (0..ch).map(|i| ramp(i, ch, overlap * scale, before_y, after_y)).collect();
            // rows of the tile are disjoint rows of the output: accumulate them in parallel
            let (oy0, ox0) = (y0 * scale, x0 * scale);
            let rows = &mut out[oy0 * ow * 3..(oy0 + ch) * ow * 3];
            let wrows = &mut wsum[oy0 * ow..(oy0 + ch) * ow];
            rows.par_chunks_mut(ow * 3).zip(wrows.par_chunks_mut(ow)).enumerate().for_each(|(j, (orow, wrow))| {
                for i in 0..cw {
                    let wt = wx[i] * wy[j];
                    for c in 0..3 {
                        orow[(ox0 + i) * 3 + c] += wt * y[c * sh * sw + j * sw + i];
                    }
                    wrow[ox0 + i] += wt;
                }
            });
            done += 1;
            if !progress(done, total) {
                return Err(Error::Cancelled);
            }
        }
    }
    out.par_chunks_mut(ow * 3).zip(wsum.par_chunks(ow)).for_each(|(orow, wrow)| {
        for (i, &wt) in wrow.iter().enumerate() {
            let k = 1.0 / wt.max(1e-12);
            for c in 0..3 {
                orow[i * 3 + c] *= k;
            }
        }
    });
    Ok(Image { width: ow, height: oh, data: out })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_cover_the_side_with_the_last_one_flush() {
        assert_eq!(starts(100, 128, 16), vec![0]);
        assert_eq!(starts(128, 128, 16), vec![0]);
        assert_eq!(starts(129, 128, 16), vec![0, 1]);
        assert_eq!(starts(300, 128, 16), vec![0, 112, 172]);
        for len in [129usize, 255, 256, 257, 1000, 6000] {
            let s = starts(len, 128, 16);
            assert_eq!(s.last().copied(), Some(len - 128));
            assert!(s.windows(2).all(|p| p[1] > p[0] && p[1] - p[0] <= 112), "{len}: {s:?}");
        }
    }

    /// An identity network over odd sizes returns the input (padding, ramps and the weighted mean
    /// don't change values), and a 2x nearest upscaler returns the input doubled.
    #[test]
    fn identity_and_2x_nearest_are_exact_for_any_size() {
        let dev = Device::Cpu;
        for (w, h) in [(37usize, 29usize), (130, 70), (300, 20), (5, 400)] {
            let data: Vec<f32> = (0..w * h * 3).map(|i| ((i * 7919) % 255) as f32 / 255.0).collect();
            let img = Image { width: w, height: h, data };
            let t = Tiling { tile: 128, overlap: 16, multiple: 8 };
            let id = run(&img, 1, t, &dev, &|x| Ok(x.clone()), &mut |_, _| true).unwrap();
            assert_eq!((id.width, id.height), (w, h));
            let err = id.data.iter().zip(&img.data).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
            assert!(err < 1e-6, "{w}x{h}: {err}");
            let up = run(&img, 2, t, &dev, &|x| Ok(x.upsample_nearest2d(x.dim(2)? * 2, x.dim(3)? * 2)?), &mut |_, _| true).unwrap();
            assert_eq!((up.width, up.height), (2 * w, 2 * h));
            let err = (0..2 * h)
                .flat_map(|y| (0..2 * w).map(move |x| (x, y)))
                .map(|(x, y)| (0..3).map(|c| (up.data[(y * 2 * w + x) * 3 + c] - img.data[((y / 2) * w + x / 2) * 3 + c]).abs()).fold(0f32, f32::max))
                .fold(0f32, f32::max);
            assert!(err < 1e-6, "{w}x{h} 2x: {err}");
        }
    }

    #[test]
    fn cancelling_stops_the_run() {
        let img = Image { width: 300, height: 300, data: vec![0.5; 300 * 300 * 3] };
        let t = Tiling { tile: 128, overlap: 16, multiple: 8 };
        let r = run(&img, 1, t, &Device::Cpu, &|x| Ok(x.clone()), &mut |d, _| d < 2);
        assert!(matches!(r, Err(Error::Cancelled)));
    }
}
