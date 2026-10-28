//! Tests of [`crate::dub`]: `sequence.placeDub` imports a dubbed voice, puts it on a new track and
//! writes the ducking of the original track, as one undo step, or leaves nothing behind.

use filmcraft_project::ClipId;
use serde_json::json;

use super::*;

const SR: u32 = 48_000;

fn write_tone(dir: &std::path::Path, name: &str, seconds: f64) -> String {
    let n = (seconds * SR as f64) as usize;
    let inter: Vec<f32> = (0..n)
        .flat_map(|i| {
            let v = 0.3 * (i as f32 * 440.0 * std::f32::consts::TAU / SR as f32).sin();
            [v, v]
        })
        .collect();
    let p = dir.join(name);
    std::fs::write(&p, crate::previews::write_wav_f32(&inter, SR)).unwrap();
    p.to_string_lossy().into_owned()
}

/// A session with a 6 s tone (the "original voice") on A1.
fn session(dir: &std::path::Path) -> (Session, ClipId) {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "dub", "audio": 1, "video": 1})).unwrap();
    let orig = write_tone(dir, "orig.wav", 6.0);
    let r = s.execute("file.import", json!({"paths": [orig]})).unwrap();
    let item = r["items"][0].as_u64().unwrap();
    let c = s.execute("timeline.place", json!({"item": item, "audioTrack": "A1", "seconds": 0.0})).unwrap();
    (s, ClipId(c["clips"][0].as_u64().unwrap()))
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("filmcraft-dub-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn lane_keys(s: &Session) -> usize {
    s.active_sequence().unwrap().audio_tracks[0].mixer.lanes.get("volume").map_or(0, |p| p.keyframes.len())
}

#[test]
fn place_dub_is_one_undo_step() {
    let dir = tmp("one-step");
    let (mut s, _) = session(&dir);
    let dubbed = write_tone(&dir, "dub.wav", 3.0);
    let (undo0, items0) = (s.history.undo.len(), s.project.items.len());
    let secs = |t: f64| (t * 254_016_000_000.0) as i64;
    let r = s
        .execute(
            "sequence.placeDub",
            json!({"path": dubbed, "seconds": 1.0, "name": "Doublage EN", "note": "2 phrases doublées",
                   "duck": {"strip": "A1", "points": [[secs(0.9), 0.0], [secs(1.0), -18.0], [secs(3.5), -18.0], [secs(3.75), 0.0]]}}),
        )
        .unwrap();
    assert_eq!(r["summary"], "2 phrases doublées", "{r}");
    let q = s.active_sequence().unwrap();
    assert_eq!(q.audio_tracks.len(), 2, "a new audio track");
    assert_eq!(q.audio_tracks[1].name, "Doublage EN");
    assert_eq!(q.audio_tracks[1].items.len(), 1);
    assert!((q.audio_tracks[1].items[0].start.seconds() - 1.0).abs() < 0.05);
    assert!(lane_keys(&s) >= 3, "{} keyframes", lane_keys(&s));
    assert_eq!(s.history.undo.len(), undo0 + 1, "one undo step");
    assert_eq!(s.undo().as_deref(), Some("Place Dubbed Audio"));
    assert_eq!(s.active_sequence().unwrap().audio_tracks.len(), 1, "track gone");
    assert_eq!(lane_keys(&s), 0, "ducking gone");
    assert_eq!(s.project.items.len(), items0, "imported item gone");
    s.redo().unwrap();
    assert_eq!(s.active_sequence().unwrap().audio_tracks.len(), 2);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn place_dub_without_ducking_leaves_the_original_track_alone() {
    let dir = tmp("no-duck");
    let (mut s, _) = session(&dir);
    let dubbed = write_tone(&dir, "dub.wav", 1.0);
    s.execute("sequence.placeDub", json!({"path": dubbed, "seconds": 0.5})).unwrap();
    assert_eq!(lane_keys(&s), 0);
    assert_eq!(s.active_sequence().unwrap().audio_tracks[1].name, "Doublage");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_failing_step_undoes_everything() {
    let dir = tmp("rollback");
    let (mut s, _) = session(&dir);
    let dubbed = write_tone(&dir, "dub.wav", 1.0);
    let (undo0, items0) = (s.history.undo.len(), s.project.items.len());
    // the strip does not exist: the import, the track and the clip are made first, then refused
    let e = s.execute("sequence.placeDub", json!({"path": dubbed, "seconds": 0.0, "duck": {"strip": "A9", "points": [[0, 0.0], [10, -9.0]]}}));
    assert!(e.is_err());
    assert_eq!(s.history.undo.len(), undo0, "no undo step left");
    assert_eq!(s.project.items.len(), items0, "nothing imported");
    assert_eq!(s.active_sequence().unwrap().audio_tracks.len(), 1, "no stray track");
    // a missing file is refused up front, with its name
    let e = s.execute("sequence.placeDub", json!({"path": "/nonexistent/doublage.wav"})).unwrap_err().to_string();
    assert!(e.contains("introuvable") && e.contains("doublage.wav"), "{e}");
    let _ = std::fs::remove_dir_all(dir);
}
