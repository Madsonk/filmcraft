//! `color.matchShots`: a warm, darker target clip is matched to a reference clip of the demo
//! project; the LUT lands in the library and the target's Creative Look, as one undo step.

use serde_json::json;

use crate::Session;
use filmcraft_project::{ClipId, ParamValue};

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

/// Two different V1 clips: (reference, target).
fn two_clips(s: &Session) -> (ClipId, ClipId) {
    let q = s.active_sequence().unwrap();
    let items = &q.video_tracks[0].items;
    assert!(items.len() >= 2);
    (items[0].id, items[items.len() - 1].id)
}

fn look_of(s: &Session, clip: ClipId) -> Option<String> {
    let q = s.active_sequence().unwrap();
    let e = q.find_item(clip).unwrap().1.effects.iter().find(|e| e.effect == "lumetri")?;
    match e.param("look_lut").map(|v| &v.value) {
        Some(ParamValue::Text(t)) if !t.is_empty() => Some(t.clone()),
        _ => None,
    }
}

/// The target as a second camera with its own cast: Lumetri temperature + exposure.
fn cast_target(s: &mut Session, target: ClipId) {
    s.execute("effects.apply", json!({"clips": [target.0], "effect": "lumetri"})).unwrap();
    for (param, value) in [("temperature", json!(70.0)), ("tint", json!(-30.0)), ("exposure", json!(-0.5))] {
        s.execute("effects.setParam", json!({"clip": target.0, "effect": "lumetri", "param": param, "value": value})).unwrap();
    }
}

#[test]
fn matching_brings_the_target_towards_the_reference_in_one_undo_step() {
    let mut s = demo();
    let (reference, target) = two_clips(&s);
    cast_target(&mut s, target);
    let luts0 = s.project.luts.len();
    let undo0 = s.history.undo.len();
    for method in ["reinhard", "mkl", "histogram"] {
        let r = s.execute("color.matchShots", json!({"referenceClip": reference.0, "targetClip": target.0, "method": method})).unwrap();
        let d = &r["meanDistance"];
        let (before, after) = (d["before"].as_f64().unwrap(), d["measuredAfter"].as_f64().unwrap());
        assert!(before > 3.0, "{method}: the cast is visible {r}");
        assert!(after < before * 0.5, "{method}: {r}");
        eprintln!("{method}: mean distance {before:.2} → predicted {:.2}, measured {after:.2}", d["predictedAfter"].as_f64().unwrap());
        assert!(d["predictedAfter"].as_f64().unwrap() < before * 0.5, "{method}: {r}");
        let lut = r["lut"].as_str().unwrap().to_string();
        assert_eq!(look_of(&s, target).as_deref(), Some(lut.as_str()));
        assert_eq!(s.project.luts.len(), luts0 + 1);
        let text = &s.project.luts.last().unwrap().text;
        assert!(text.contains("LUT_3D_SIZE 33"));
        assert_eq!(s.history.undo.len(), undo0 + 1, "one undo step");
        // undo removes the look and the library entry together
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(look_of(&s, target), None);
        assert_eq!(s.project.luts.len(), luts0);
        assert_eq!(s.history.undo.len(), undo0);
    }
}

#[test]
fn without_apply_only_the_library_changes_and_strength_zero_changes_nothing() {
    let mut s = demo();
    let (reference, target) = two_clips(&s);
    cast_target(&mut s, target);
    let dir = std::env::temp_dir().join(format!("fc-shotmatch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("m.cube");
    let r =
        s.execute("color.matchShots", json!({"referenceClip": reference.0, "targetClip": target.0, "apply": false, "path": path.to_string_lossy()})).unwrap();
    assert_eq!(r["applied"], false);
    assert!(r["meanDistance"]["measuredAfter"].is_null());
    assert_eq!(look_of(&s, target), None);
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("LUT_3D_SIZE 33"));
    filmcraft_color::Lut::parse_cube(&written).unwrap();
    // strength 0: the look is the identity, the picture does not move
    let r = s.execute("color.matchShots", json!({"referenceClip": reference.0, "targetClip": target.0, "strength": 0})).unwrap();
    let d = &r["meanDistance"];
    assert!((d["measuredAfter"].as_f64().unwrap() - d["before"].as_f64().unwrap()).abs() < 0.3, "{r}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hostile_parameters_are_refused() {
    let mut s = demo();
    let (reference, target) = two_clips(&s);
    let undo0 = s.history.undo.len();
    for p in [
        json!({}),
        json!({"referenceClip": reference.0}),
        json!({"referenceClip": reference.0, "targetClip": reference.0}),
        json!({"referenceClip": reference.0, "targetClip": 999_999}),
        json!({"referenceClip": reference.0, "targetClip": target.0, "method": "magic"}),
        json!({"referenceClip": reference.0, "targetClip": target.0, "strength": 150}),
        json!({"referenceClip": reference.0, "targetClip": target.0, "strength": -1}),
        json!({"referenceClip": reference.0, "targetClip": target.0, "frames": 0}),
        json!({"referenceClip": reference.0, "targetClip": target.0, "frames": 1000}),
        json!({"referenceClip": reference.0, "targetClip": target.0, "apply": false, "path": "/nonexistent-dir/x.cube"}),
    ] {
        assert!(s.execute("color.matchShots", p.clone()).is_err(), "{p}");
    }
    assert_eq!(s.history.undo.len(), undo0, "nothing recorded");
    assert_eq!(s.project.luts.len(), 0);
}
