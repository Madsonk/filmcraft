//! Perceptual hash for near-duplicates: two 64-bit difference hashes (dHash) of the photo's
//! luminance, one comparing horizontal neighbours on a 9 × 8 grid and one vertical neighbours on
//! an 8 × 9 grid. Shots of the same scene (a burst, an export at another size, a light edit)
//! land within a few bits of each other; different scenes sit near 64 of 128.

/// What the stored hashes mean (a change invalidates the file).
pub const TAG: &str = "dhash-9x8+8x9-v1";

/// Average of `gray` (`w × h`) over a `gw × gh` grid of boxes.
fn grid(gray: &[f32], w: usize, h: usize, gw: usize, gh: usize) -> Vec<f32> {
    let mut out = vec![0f32; gw * gh];
    for gy in 0..gh {
        let (y0, y1) = (gy * h / gh, ((gy + 1) * h / gh).max(gy * h / gh + 1).min(h));
        for gx in 0..gw {
            let (x0, x1) = (gx * w / gw, ((gx + 1) * w / gw).max(gx * w / gw + 1).min(w));
            let mut acc = 0f32;
            for y in y0..y1 {
                for x in x0..x1 {
                    acc += gray[y * w + x];
                }
            }
            out[gy * gw + gx] = acc / ((y1 - y0) * (x1 - x0)) as f32;
        }
    }
    out
}

/// Hash of an 8-bit RGB image; `None` when the buffer doesn't match its size.
pub fn dhash(rgb: &[u8], w: usize, h: usize) -> Option<[u64; 2]> {
    if w < 2 || h < 2 || rgb.len() != w * h * 3 {
        return None;
    }
    let gray: Vec<f32> = rgb.as_chunks::<3>().0.iter().map(|p| 0.299 * f32::from(p[0]) + 0.587 * f32::from(p[1]) + 0.114 * f32::from(p[2])).collect();
    let horizontal = grid(&gray, w, h, 9, 8);
    let vertical = grid(&gray, w, h, 8, 9);
    let (mut a, mut b) = (0u64, 0u64);
    for y in 0..8 {
        for x in 0..8 {
            a = (a << 1) | u64::from(horizontal[y * 9 + x] > horizontal[y * 9 + x + 1]);
            b = (b << 1) | u64::from(vertical[y * 8 + x] > vertical[(y + 1) * 8 + x]);
        }
    }
    Some([a, b])
}

/// Differing bits of two hashes (0..=128).
pub fn distance(a: [u64; 2], b: [u64; 2]) -> u32 {
    (a[0] ^ b[0]).count_ones() + (a[1] ^ b[1]).count_ones()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: usize, h: usize, flip: bool) -> Vec<u8> {
        let mut v = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let t = if flip { w - 1 - x } else { x } * 255 / w + y * 40 / h;
                v.extend_from_slice(&[t as u8, (t / 2) as u8, 30]);
            }
        }
        v
    }

    #[test]
    fn the_same_picture_at_another_size_is_close_and_a_mirrored_one_is_far() {
        let a = dhash(&gradient(400, 300, false), 400, 300).unwrap();
        let b = dhash(&gradient(200, 150, false), 200, 150).unwrap();
        let c = dhash(&gradient(400, 300, true), 400, 300).unwrap();
        assert!(distance(a, b) <= 4, "{}", distance(a, b));
        assert!(distance(a, c) > 30, "{}", distance(a, c));
        assert!(dhash(&[0; 5], 2, 2).is_none());
    }
}
