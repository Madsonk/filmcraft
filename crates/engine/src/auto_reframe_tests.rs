//! Tests of [`crate::auto_reframe`]: formats, refusals, and (when the YOLOX files are installed under
//! `FILMCRAFT_MODELS_DIR` and `FILMCRAFT_TEST_PERSON_IMAGE` names a photo with a person) a re-frame of
//! a still: the crop stays inside the picture, one undo step.

use std::sync::Arc;

use filmcraft_project::ClipId;
use serde_json::json;

use crate::auto_reframe::{output_size, ratio_p};

use super::*;

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x % 256) as u8, (y % 256) as u8, 90, 255]));
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
    out
}

fn session_with_still(bytes: Vec<u8>, two_clips: bool) -> (Session, ClipId) {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "reframe", "audio": 1, "video": 2, "width": 1920, "height": 1080, "fps": 25})).unwrap();
    let item = crate::commands::import_bytes(&mut s, "/shot.png", Arc::from(bytes), None).unwrap();
    let v = s.execute("timeline.place", json!({"item": item.0, "videoTrack": "V1", "seconds": 0.0})).unwrap();
    let clip = ClipId(v["clips"][0].as_u64().unwrap());
    if two_clips {
        s.execute("timeline.place", json!({"item": item.0, "videoTrack": "V1", "seconds": 30.0})).unwrap();
    }
    let n: usize = s.active_sequence().unwrap().video_tracks.iter().map(|t| t.items.len()).sum();
    assert_eq!(n, if two_clips { 2 } else { 1 });
    s.execute("timeline.select", json!({"clips": [clip.0]})).unwrap();
    (s, clip)
}

#[test]
fn ratios_and_output_sizes() {
    let r = |v: serde_json::Value| ratio_p(&json!({"ratio": v}), "t");
    assert_eq!(r(json!("9:16")).unwrap(), (9.0, 16.0));
    assert_eq!(r(json!("1080x1920")).unwrap(), (1080.0, 1920.0));
    assert_eq!(r(json!(1.0)).unwrap(), (1.0, 1.0));
    assert_eq!(ratio_p(&json!({}), "t").unwrap(), (9.0, 16.0));
    assert!(r(json!("nine:sixteen")).is_err() && r(json!("0:5")).is_err() && r(json!("100:1")).is_err() && r(json!(true)).is_err());
    assert_eq!(output_size((9.0, 16.0), (1920, 1080)), (1080, 1920));
    assert_eq!(output_size((1.0, 1.0), (1920, 1080)), (1080, 1080));
    assert_eq!(output_size((4.0, 5.0), (1920, 1080)), (1080, 1350));
    assert_eq!(output_size((16.0, 9.0), (1280, 720)), (1280, 720), "already that ratio: the sequence's own size");
    assert_eq!(output_size((9.0, 16.0), (1080, 1920)), (1080, 1920));
}

#[test]
fn refuses_clearly_without_models_and_when_other_clips_would_change() {
    let (mut s, clip) = session_with_still(png(640, 360), false);
    let none = std::env::temp_dir().join(format!("fc-reframe-none-{}", std::process::id()));
    let p = json!({"modelsDir": none.to_string_lossy()});
    let e = s.execute("sequence.autoReframe", p.clone()).unwrap_err().to_string();
    assert!(e.contains("models.install") && e.contains("YOLOX"), "{e}");
    assert_eq!(s.active_sequence().unwrap().settings.width, 1920, "nothing changed");
    let _ = clip;
    // a second clip on another track would be re-framed too
    let (mut s2, c2) = session_with_still(png(640, 360), true);
    let e = s2.execute("sequence.autoReframe", json!({"modelsDir": none.to_string_lossy(), "clips": [c2.0]})).unwrap_err().to_string();
    assert!(e.contains("autre") && e.contains("resizeSequence"), "{e}");
    // bad parameters
    assert!(s.execute("sequence.autoReframe", json!({"ratio": "x"})).unwrap_err().to_string().contains("ratio"));
    s.execute("timeline.select", json!({"clips": []})).unwrap();
    assert!(s.execute("sequence.autoReframe", p).unwrap_err().to_string().contains("select"));
}

#[test]
fn reframes_a_still_with_a_person_in_one_undo_step() {
    let (Ok(dir), Ok(photo)) = (std::env::var("FILMCRAFT_MODELS_DIR"), std::env::var("FILMCRAFT_TEST_PERSON_IMAGE")) else {
        eprintln!("skipped: FILMCRAFT_MODELS_DIR / FILMCRAFT_TEST_PERSON_IMAGE not set");
        return;
    };
    if !std::path::Path::new(&dir).join("yolox-nano/yolox_nano.onnx").exists() {
        eprintln!("skipped: YOLOX-Nano is not installed");
        return;
    }
    let (mut s, clip) = session_with_still(std::fs::read(&photo).unwrap(), false);
    let undo0 = s.history.undo.len();
    let r = s.execute("sequence.autoReframe", json!({"ratio": "9:16", "modelsDir": dir, "faces": false})).unwrap();
    assert_eq!(s.history.undo.len(), undo0 + 1, "one undo step");
    let q = s.active_sequence().unwrap();
    assert_eq!((q.settings.width, q.settings.height), (1080, 1920));
    let it = q.find_item(clip).unwrap().1.clone();
    let src = s.project.source_size(it.item).unwrap();
    // the output frame, mapped back to the picture, stays inside it
    let m = filmcraft_render::motion_matrix(q, &it, src, None, it.effect_time_at(it.start));
    let inv = m.inverse().unwrap();
    for (x, y) in [(0.0, 0.0), (1080.0, 1920.0)] {
        let p = inv.apply(filmcraft_geom::Vec2::new(x, y));
        assert!(p.x >= -1.0 && p.x <= f64::from(src.0) + 1.0 && p.y >= -1.0 && p.y <= f64::from(src.1) + 1.0, "{p:?} outside {src:?} ({r})");
    }
    s.undo();
    assert_eq!(s.active_sequence().unwrap().settings.width, 1920);
}
