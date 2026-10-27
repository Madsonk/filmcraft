//! `demucs_check <htdemucs dir> <stereo 44.1 kHz wav (f32 or 16-bit)> <out prefix>`: separates and writes
//! `<prefix>_<source>.npy` (2 x N float32) for comparison with the PyTorch reference.
use std::io::Write;
use std::path::Path;

fn read_wav(p: &str) -> [Vec<f32>; 2] {
    let b = std::fs::read(p).unwrap();
    let (mut pos, mut fmt, mut ch, mut data) = (12, 0u16, 2u16, None);
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let n = u32::from_le_bytes(b[pos + 4..pos + 8].try_into().unwrap()) as usize;
        if id == b"fmt " {
            fmt = u16::from_le_bytes([b[pos + 8], b[pos + 9]]) * 1000 + u16::from_le_bytes([b[pos + 22], b[pos + 23]]);
            ch = u16::from_le_bytes([b[pos + 10], b[pos + 11]]);
        }
        if id == b"data" {
            data = Some(&b[pos + 8..(pos + 8 + n).min(b.len())]);
            break;
        }
        pos += 8 + n + (n & 1);
    }
    let d = data.unwrap();
    let v: Vec<f32> = match fmt {
        3032 => d.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect(),
        1016 => d.as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes(*c) as f32 / 32768.0).collect(),
        f => panic!("unsupported wav {f}"),
    };
    assert_eq!(ch, 2);
    [v.iter().step_by(2).copied().collect(), v.iter().skip(1).step_by(2).copied().collect()]
}

fn write_npy(p: &str, ch: &[Vec<f32>; 2]) {
    let n = ch[0].len();
    let header = format!("{{'descr': '<f4', 'fortran_order': False, 'shape': (2, {n}), }}");
    let mut h = header.into_bytes();
    while (10 + h.len() + 1) % 64 != 0 {
        h.push(b' ');
    }
    h.push(b'\n');
    let mut f = std::fs::File::create(p).unwrap();
    f.write_all(b"\x93NUMPY\x01\x00").unwrap();
    f.write_all(&(h.len() as u16).to_le_bytes()).unwrap();
    f.write_all(&h).unwrap();
    for c in ch {
        for v in c {
            f.write_all(&v.to_le_bytes()).unwrap();
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let t0 = std::time::Instant::now();
    let m = filmcraft_ml::htdemucs::Htdemucs::load(Path::new(&a[1])).unwrap();
    println!("loaded in {:.1}s on {:?}", t0.elapsed().as_secs_f64(), m.device());
    let wav = read_wav(&a[2]);
    println!("{} samples ({:.1}s)", wav[0].len(), wav[0].len() as f64 / 44100.0);
    let t0 = std::time::Instant::now();
    let out = m
        .separate(&wav, &mut |d, t| {
            println!("segment {d}/{t} at {:.1}s", t0.elapsed().as_secs_f64());
            true
        })
        .unwrap();
    let dt = t0.elapsed().as_secs_f64();
    println!("separated in {dt:.1}s = {:.2}x real time", wav[0].len() as f64 / 44100.0 / dt);
    for (s, name) in filmcraft_ml::htdemucs::SOURCES.iter().enumerate() {
        write_npy(&format!("{}_{name}.npy", a[3]), &out[s]);
    }
}
