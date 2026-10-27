//! Tests of [`crate::voice_clean`]: the model commands, the refusals, and (when the DeepFilterNet3
//! files are installed under `FILMCRAFT_MODELS_DIR`, e.g. `/Volumes/Madson/ArtCraft/models`) a real
//! clean-up of a clip: one undo step, the clip keeps its place and length, the pauses get quiet.

use std::sync::Arc;

use filmcraft_project::ClipId;
use serde_json::json;

use super::*;
use crate::voice_clean::floor_and_level;

const SR: u32 = 48_000;

fn models_dir() -> Option<String> {
    let d = std::env::var("FILMCRAFT_MODELS_DIR").ok()?;
    std::path::Path::new(&d).join("deepfilternet3/enc.onnx").exists().then_some(d)
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("fc-voice-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// 3 s: noise, noise + a voiced-like tone, noise. Returns the interleaved stereo samples.
fn noisy_clip() -> Vec<f32> {
    let mut s = 12_345u32;
    let mut rnd = move || {
        s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        ((s >> 8) as f32 / (1u32 << 24) as f32) * 2.0 - 1.0
    };
    let n = (3 * SR) as usize;
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        let t = i as f32 / SR as f32;
        let voice = if (1.0..2.0).contains(&t) {
            let f0 = 140.0 + 20.0 * (t * 3.0).sin();
            (1..8).map(|h| (2.0 * std::f32::consts::PI * f0 * h as f32 * t).sin() / h as f32).sum::<f32>() * 0.25 * (0.6 + 0.4 * (t * 9.0).sin())
        } else {
            0.0
        };
        let v = voice + rnd() * 0.05;
        out.push(v);
        out.push(v);
    }
    out
}

fn session(samples: &[f32]) -> (Session, ClipId) {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "voice", "audio": 2, "video": 1})).unwrap();
    let bytes: Arc<[u8]> = crate::previews::write_wav_f32(samples, SR).into();
    let item = crate::commands::import_bytes(&mut s, "/talk.wav", bytes, None).unwrap();
    let a1 = s.execute("timeline.place", json!({"item": item.0, "audioTrack": "A1", "seconds": 0.0})).unwrap();
    let clip = ClipId(a1["clips"][0].as_u64().unwrap());
    s.execute("timeline.select", json!({"clips": [clip.0]})).unwrap();
    (s, clip)
}

#[test]
fn models_are_listed_and_never_downloaded_without_acceptance() {
    let mut s = Session::default();
    let d = tmp("empty");
    let r = s.execute("models.list", json!({"modelsDir": d.to_string_lossy()})).unwrap();
    let ids: Vec<&str> = r["models"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["deepfilternet3", "yolox-nano", "yolox-tiny", "yolox-s", "yunet"]);
    let dfn = &r["models"][0];
    assert_eq!(dfn["installed"], false);
    assert!(dfn["license"].as_str().unwrap().contains("MIT"));
    assert_eq!(dfn["files"][0]["sha256"].as_str().unwrap().len(), 64);
    let e = s.execute("models.install", json!({"model": "deepfilternet3", "modelsDir": d.to_string_lossy()})).unwrap_err().to_string();
    assert!(e.contains("accept") && e.contains("MIT") && e.contains("github.com"), "{e}");
    assert!(!d.exists(), "nothing was created");
    assert!(s.execute("models.install", json!({"model": "nope"})).unwrap_err().to_string().contains("unknown model"));
    assert!(s.execute("models.install", json!({})).is_err());
}

#[test]
fn denoise_refuses_clearly_without_the_model_or_a_clip() {
    let (mut s, clip) = session(&noisy_clip());
    let d = tmp("none");
    let e = s.execute("audio.denoiseVoice", json!({"modelsDir": d.to_string_lossy()})).unwrap_err().to_string();
    assert!(e.contains("models.install") && e.contains("DeepFilterNet3"), "{e}");
    let _ = clip;
    s.execute("timeline.select", json!({"clips": []})).unwrap();
    let e = s.execute("audio.denoiseVoice", json!({"modelsDir": d.to_string_lossy()})).unwrap_err().to_string();
    assert!(e.contains("select"), "{e}");
    assert!(s.execute("audio.denoiseVoice", json!({"strength": "loud"})).unwrap_err().to_string().contains("strength"));
    assert!(s.execute("audio.denoiseVoice", json!({"amount": 140})).unwrap_err().to_string().contains("amount"));
}

#[test]
fn levels_report_a_floor_and_a_speech_level() {
    let x = noisy_clip();
    let mono: Vec<f32> = x.iter().step_by(2).copied().collect();
    let (floor, level) = floor_and_level(&mono, SR);
    assert!(level > floor + 6.0, "floor {floor}, level {level}");
    assert!((-40.0..-20.0).contains(&floor), "white noise at 0.05 peak is about -33 dBFS: {floor}");
}

#[test]
fn denoise_cleans_the_clip_in_one_undo_step() {
    let Some(dir) = models_dir() else {
        eprintln!("skipped: FILMCRAFT_MODELS_DIR has no DeepFilterNet3");
        return;
    };
    let (mut s, clip) = session(&noisy_clip());
    let out = tmp("out");
    let (item0, start0, dur0) = {
        let it = s.active_sequence().unwrap().find_item(clip).unwrap().1;
        (it.item, it.start, it.duration)
    };
    let undo0 = s.history.undo.len();
    let r = s.execute("audio.denoiseVoice", json!({"strength": "strong", "modelsDir": dir, "dir": out.to_string_lossy()})).unwrap();
    let c = &r["clips"][0];
    let (fb, fa) = (c["before"]["floorDb"].as_f64().unwrap(), c["after"]["floorDb"].as_f64().unwrap());
    assert!(fa < fb - 15.0, "the pauses got quieter: {fb} -> {fa} dBFS ({r})");
    assert!(r["summary"].as_str().unwrap().contains("DeepFilterNet3"));
    assert_eq!(s.history.undo.len(), undo0 + 1, "one undo step");
    let it = s.active_sequence().unwrap().find_item(clip).unwrap().1.clone();
    assert_ne!(it.item, item0, "the clip points at the cleaned file");
    assert_eq!((it.start, it.duration, it.source_in), (start0, dur0, Tick::ZERO), "place and length are kept");
    assert!(std::path::Path::new(c["path"].as_str().unwrap()).exists());
    s.undo();
    assert_eq!(s.active_sequence().unwrap().find_item(clip).unwrap().1.item, item0);
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn clips_that_are_retimed_are_refused() {
    let (mut s, clip) = session(&noisy_clip());
    s.execute("clip.setSpeed", json!({"clips": [clip.0], "speed": 200.0})).ok();
    let sp = s.active_sequence().unwrap().find_item(clip).unwrap().1.speed;
    if (sp - 1.0).abs() > 1e-9 {
        let e = s.execute("audio.denoiseVoice", json!({"modelsDir": tmp("x").to_string_lossy()})).unwrap_err().to_string();
        assert!(e.contains("retimed"), "{e}");
    }
}
