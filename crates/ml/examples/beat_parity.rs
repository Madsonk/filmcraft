//! Parity of the candle Beat This! with the reference PyTorch model.
//! `beat_parity <models/beat-this dir> <mono 22050 Hz wav> <npy prefix written by ref_beat_this.py>`
use std::path::Path;

fn read_wav(p: &str) -> Vec<f32> {
    let b = std::fs::read(p).unwrap();
    assert_eq!(&b[0..4], b"RIFF");
    let (mut pos, mut fmt, mut data) = (12, (0u16, 0u16), None);
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let n = u32::from_le_bytes(b[pos + 4..pos + 8].try_into().unwrap()) as usize;
        if id == b"fmt " {
            fmt = (u16::from_le_bytes([b[pos + 8], b[pos + 9]]), u16::from_le_bytes([b[pos + 22], b[pos + 23]]));
        }
        if id == b"data" {
            data = Some(&b[pos + 8..(pos + 8 + n).min(b.len())]);
            break;
        }
        pos += 8 + n + (n & 1);
    }
    let d = data.unwrap();
    match fmt {
        (3, 32) => d.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect(),
        (1, 16) => d.as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes(*c) as f32 / 32768.0).collect(),
        f => panic!("unsupported wav {f:?}"),
    }
}

/// Values of a float32 .npy; a Fortran-ordered (T, cols) array is returned row-major.
fn read_npy(p: &str) -> Vec<f32> {
    let b = std::fs::read(p).unwrap();
    let hl = u16::from_le_bytes([b[8], b[9]]) as usize;
    let head = String::from_utf8_lossy(&b[10..10 + hl]).to_string();
    let v: Vec<f32> = b[10 + hl..].as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
    if head.contains("'fortran_order': True") && head.contains(", 128)") {
        let t = v.len() / 128;
        return (0..t * 128).map(|i| v[(i % 128) * t + i / 128]).collect();
    }
    v
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let model = filmcraft_ml::beat_this::BeatThis::load(Path::new(&a[1])).unwrap();
    println!("device {:?}", model.device());
    let wav = read_wav(&a[2]);
    let fb = std::fs::read(Path::new(&a[1]).join("mel-filterbank.bin")).unwrap();
    let fb: Vec<f32> = fb.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
    let (spect, frames) = filmcraft_ml::beat_this::log_mel(&wav, &fb).unwrap();
    let ref_spect = read_npy(&format!("{}_spect.npy", a[3]));
    let md = spect.iter().zip(&ref_spect).map(|(x, y)| (x - y).abs()).fold(0f32, f32::max);
    let (arg, _) = spect.iter().zip(&ref_spect).map(|(x, y)| (x - y).abs()).enumerate().fold((0, 0f32), |a, (i, v)| if v > a.1 { (i, v) } else { a });
    let mean = spect.iter().zip(&ref_spect).map(|(x, y)| (x - y).abs() as f64).sum::<f64>() / spect.len() as f64;
    println!("frames {frames} (ref {}), log-mel max |diff| = {md:.2e} at frame {} bin {}, mean {mean:.2e}", ref_spect.len() / 128, arg / 128, arg % 128);
    let t0 = std::time::Instant::now();
    let n = frames.min(1500);
    let (b, d) = model.logits(&spect[..n * 128], n, &mut |_, _| true).unwrap();
    println!("model {:.2}s", t0.elapsed().as_secs_f64());
    let (rb, rd) = (read_npy(&format!("{}_beat.npy", a[3])), read_npy(&format!("{}_down.npy", a[3])));
    let rms = |x: &[f32], y: &[f32]| (x.iter().zip(y).map(|(a, b)| ((a - b) as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt();
    println!(
        "logits rms diff: beat {:.3e} (ref rms {:.2}), downbeat {:.3e}",
        rms(&b[6..n - 6], &rb[6..n - 6]),
        rms(&rb[6..n - 6], &vec![0.0; n - 12]),
        rms(&d[6..n - 6], &rd[6..n - 6])
    );
    let mb = b.iter().zip(&rb).skip(6).take(n - 12).map(|(x, y)| (x - y).abs()).fold(0f32, f32::max);
    let mdn = d.iter().zip(&rd).skip(6).take(n - 12).map(|(x, y)| (x - y).abs()).fold(0f32, f32::max);
    println!("beat logits max |diff| = {mb:.2e}, downbeat = {mdn:.2e} (frames 6..{})", n - 6);
    let t0 = std::time::Instant::now();
    let tr = model.track(&wav, &mut |_, _| true).unwrap();
    println!(
        "track {:.2}s: {} beats, {} downbeats; first {:?}",
        t0.elapsed().as_secs_f64(),
        tr.beats.len(),
        tr.downbeats.len(),
        &tr.beats[..tr.beats.len().min(8)]
    );
}
