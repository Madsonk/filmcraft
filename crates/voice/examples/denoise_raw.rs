//! `denoise_raw MODEL_DIR IN.f32 OUT.f32 SAMPLE_RATE [AMOUNT]` — cleans a raw mono little-endian f32
//! file with the three DeepFilterNet3 graphs found in `MODEL_DIR` and prints the elapsed time.

use filmcraft_voice::{Options, VoiceModel};

fn main() -> Result<(), String> {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 5 {
        return Err("usage: denoise_raw MODEL_DIR IN.f32 OUT.f32 SAMPLE_RATE [AMOUNT]".into());
    }
    let rd = |n: &str| std::fs::read(std::path::Path::new(&a[1]).join(n)).map_err(|e| format!("{n}: {e}"));
    let m = VoiceModel::from_bytes(&rd("enc.onnx")?, &rd("erb_dec.onnx")?, &rd("df_dec.onnx")?).map_err(|e| e.to_string())?;
    let raw = std::fs::read(&a[2]).map_err(|e| e.to_string())?;
    let x: Vec<f32> = raw.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
    let rate: u32 = a[4].parse().map_err(|_| "bad sample rate")?;
    let amount: f32 = a.get(5).and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let t0 = std::time::Instant::now();
    let y = m.denoise(&x, rate, &Options { amount }, &mut |_| true).map_err(|e| e.to_string())?;
    eprintln!("{:.2} s of audio in {:.2} s", x.len() as f64 / f64::from(rate), t0.elapsed().as_secs_f64());
    std::fs::write(&a[3], y.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>()).map_err(|e| e.to_string())
}
