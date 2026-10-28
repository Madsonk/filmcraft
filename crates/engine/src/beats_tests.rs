//! Tests of [`crate::beats`]: `sequence.markBeats` and `sequence.cutOnBeats` on generated rhythmic
//! music (120 BPM, 16 bars, 32 s: the true beats sit every 0.5 s).

use std::sync::Arc;

use filmcraft_audio_dsp::remix as dsp;
use filmcraft_project::ClipId;
use serde_json::json;

use super::*;

const SR: u32 = 48_000;

/// A session with generated music on A1 (selected) and the same media on A2 at 0 s.
fn session() -> (Session, ClipId, ClipId) {
    let (m, _) = dsp::test_music(SR, 120.0, 16, 3);
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "beats", "audio": 2, "video": 1})).unwrap();
    let inter: Vec<f32> = (0..m[0].len()).flat_map(|i| [m[0][i], m[1][i]]).collect();
    let bytes: Arc<[u8]> = crate::previews::write_wav_f32(&inter, SR).into();
    let item = crate::commands::import_bytes(&mut s, "/music.wav", bytes, None).unwrap();
    let a1 = s.execute("timeline.place", json!({"item": item.0, "audioTrack": "A1", "seconds": 0.0})).unwrap();
    let a1 = ClipId(a1["clips"][0].as_u64().unwrap());
    let a2 = s.execute("timeline.place", json!({"item": item.0, "audioTrack": "A2", "seconds": 0.0})).unwrap();
    let a2 = ClipId(a2["clips"][0].as_u64().unwrap());
    s.execute("timeline.select", json!({"clips": [a1.0]})).unwrap();
    (s, a1, a2)
}

fn markers(s: &Session) -> Vec<(f64, String)> {
    s.active_sequence().unwrap().markers.iter().map(|m| (m.start.seconds(), m.name.clone())).collect()
}

fn clip_count(s: &Session, track: usize) -> usize {
    s.active_sequence().unwrap().audio_tracks[track].items.len()
}

#[test]
fn mark_every_beat_lands_on_the_true_beats_in_one_undo_step() {
    let (mut s, _, _) = session();
    let undo0 = s.history.undo.len();
    let r = s.execute("sequence.markBeats", json!({})).unwrap();
    assert!((r["bpm"].as_f64().unwrap() - 120.0).abs() < 2.0, "{r}");
    let m = markers(&s);
    assert_eq!(r["markers"].as_u64().unwrap() as usize, m.len());
    assert!(m.len() >= 60 && m.len() <= 64, "{} markers", m.len());
    // every marker is within 40 ms of a true beat (0.5 s grid; frame snapping adds up to half a frame)
    for (t, name) in &m {
        let off = (t / 0.5 - (t / 0.5).round()).abs() * 0.5;
        assert!(off < 0.04, "{name} at {t} s is {off} s off the beat grid");
    }
    assert_eq!(s.history.undo.len(), undo0 + 1, "one undo step");
    s.undo();
    assert!(markers(&s).is_empty());
}

#[test]
fn every_and_offset_thin_out_the_markers() {
    let (mut s, _, _) = session();
    let all = s.execute("sequence.markBeats", json!({"apply": false})).unwrap();
    assert!(markers(&s).is_empty(), "apply:false only reports");
    let n_all = all["beats"].as_u64().unwrap();
    let r = s.execute("sequence.markBeats", json!({"every": 4, "name": "Bar", "color": "Rose"})).unwrap();
    let n = r["markers"].as_u64().unwrap();
    assert_eq!(n, n_all.div_ceil(4), "{r}");
    let m = markers(&s);
    assert!(m.iter().all(|(_, name)| name.starts_with("Bar ")));
    // 4 beats of 0.5 s apart
    assert!((m[1].0 - m[0].0 - 2.0).abs() < 0.07, "{m:?}");
    let before = m.len();
    let again = s.execute("sequence.markBeats", json!({"every": 4, "name": "Bar"})).unwrap();
    assert_eq!(again["markers"], 0, "no duplicate marker at the same place");
    assert_eq!(markers(&s).len(), before);
    let off = s.execute("sequence.markBeats", json!({"every": 4, "offset": 2, "name": "Off"})).unwrap();
    assert!(off["markers"].as_u64().unwrap() > 0);
    assert!(s.execute("sequence.markBeats", json!({"every": 0})).is_err());
}

#[test]
fn cut_on_beats_cuts_the_given_clips_only_with_one_undo_step() {
    let (mut s, a1, a2) = session();
    let undo0 = s.history.undo.len();
    let r = s.execute("sequence.cutOnBeats", json!({"beatClip": a1.0, "clips": [a2.0], "every": 2})).unwrap();
    let cuts = r["cuts"].as_u64().unwrap() as usize;
    assert!(cuts >= 30, "{r}");
    assert_eq!(clip_count(&s, 1), cuts + 1, "A2 was cut");
    assert_eq!(clip_count(&s, 0), 1, "the beat clip itself was not cut");
    assert_eq!(s.history.undo.len(), undo0 + 1);
    s.undo();
    assert_eq!(clip_count(&s, 1), 1);
}

#[test]
fn cut_on_beats_uses_the_selection_and_cuts_the_music_when_alone() {
    let (mut s, a1, a2) = session();
    s.execute("timeline.select", json!({"clips": [a1.0, a2.0]})).unwrap();
    s.execute("sequence.cutOnBeats", json!({"every": 8})).unwrap();
    assert_eq!(clip_count(&s, 0), 1, "the beat clip (first selected audio clip) is not a target when others are selected");
    assert!(clip_count(&s, 1) > 3);
    s.undo();
    s.execute("timeline.select", json!({"clips": [a1.0]})).unwrap();
    s.execute("sequence.cutOnBeats", json!({"every": 8})).unwrap();
    assert!(clip_count(&s, 0) > 3, "alone, the music is cut");
}

#[test]
fn refusals_are_explained() {
    let (mut s, a1, _) = session();
    // no audio: disabled with a reason
    let mut empty = Session::default();
    empty.execute("file.newSequence", json!({"name": "e", "audio": 1, "video": 1})).unwrap();
    assert!(empty.execute("sequence.markBeats", json!({})).is_err());
    assert!(s.execute("sequence.markBeats", json!({"clip": 9999})).is_err());
    // a retimed clip is refused, not guessed at
    s.edit_sequence("speed", |q, _, _| {
        q.find_item_mut(a1).unwrap().1.speed = 2.0;
        Ok(())
    })
    .unwrap();
    let e = s.execute("sequence.markBeats", json!({"clip": a1.0})).unwrap_err().to_string();
    assert!(e.contains("100"), "{e}");
}

#[test]
fn subdivide_adds_the_beats_between_the_detected_ones() {
    let (mut s, _, _) = session();
    let one = s.execute("sequence.markBeats", json!({"apply": false})).unwrap()["beats"].as_u64().unwrap();
    let two = s.execute("sequence.markBeats", json!({"apply": false, "subdivide": 2})).unwrap();
    let n = two["beats"].as_u64().unwrap();
    assert!(n >= 2 * one - 1 && n <= 2 * one, "{one} detected, {n} with subdivide 2");
    let t: Vec<f64> = two["times"].as_array().unwrap().iter().map(|v| v["seconds"].as_f64().unwrap()).collect();
    assert!((t[1] - t[0] - 0.25).abs() < 0.05, "{t:?}");
    assert!(s.execute("sequence.markBeats", json!({"subdivide": 0})).is_err());
}

#[test]
fn every_command_says_what_it_did() {
    let (mut s, a1, a2) = session();
    let r = s.execute("sequence.markBeats", json!({})).unwrap();
    let line = r["summary"].as_str().unwrap();
    assert!(
        line.starts_with("63 marqueurs posés, 120 BPM") || line.starts_with("64 marqueurs posés, 120 BPM") || line.starts_with("62 marqueurs posés, 120 BPM"),
        "{line}"
    );
    let again = s.execute("sequence.markBeats", json!({})).unwrap();
    assert!(again["summary"].as_str().unwrap().starts_with("0 marqueur posé : les "), "{again}");
    let dry = s.execute("sequence.markBeats", json!({"apply": false, "every": 4})).unwrap();
    assert!(dry["summary"].as_str().unwrap().contains("rien posé"), "{dry}");
    let cuts = s.execute("sequence.cutOnBeats", json!({"beatClip": a1.0, "clips": [a2.0], "every": 8})).unwrap();
    let line = cuts["summary"].as_str().unwrap();
    assert!(line.contains(" coupes sur 1 clip, 120 BPM"), "{line}");
}

#[test]
fn a_clip_without_a_pulse_gets_a_clear_message() {
    // 20 s of a slow tone: no beat to find
    let sr = SR;
    let pad: Vec<f32> = (0..sr as usize * 20)
        .flat_map(|i| {
            let t = i as f64 / sr as f64;
            let v = (0.3
                * (0.5 - 0.5 * (std::f64::consts::TAU * t / 7.0).cos())
                * ((std::f64::consts::TAU * 220.0 * t).sin() + (std::f64::consts::TAU * 330.0 * t).sin())) as f32;
            [v, v]
        })
        .collect();
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "ambient", "audio": 1, "video": 1})).unwrap();
    let bytes: Arc<[u8]> = crate::previews::write_wav_f32(&pad, SR).into();
    let item = crate::commands::import_bytes(&mut s, "/ambient.wav", bytes, None).unwrap();
    s.execute("timeline.place", json!({"item": item.0, "audioTrack": "A1", "seconds": 0.0})).unwrap();
    for cmd in ["sequence.markBeats", "sequence.cutOnBeats"] {
        let e = s.execute(cmd, json!({})).unwrap_err().to_string();
        assert!(e.contains("0 beat détecté dans ce clip"), "{cmd}: {e}");
        assert!(e.contains("pulsation"), "{e}");
    }
    assert!(s.active_sequence().unwrap().markers.is_empty());
}

#[test]
fn a_tempo_hint_and_the_bar_start_are_honoured() {
    let (mut s, _, _) = session();
    // half tempo by hint: every other beat of the 120 BPM music
    let half = s.execute("sequence.markBeats", json!({"apply": false, "bpm": 60})).unwrap();
    assert!((half["bpm"].as_f64().unwrap() - 60.0).abs() < 1.0, "{half}");
    let n = half["beats"].as_u64().unwrap();
    assert!((30..=32).contains(&n), "{half}");
    assert!(s.execute("sequence.markBeats", json!({"bpm": 5})).is_err());
    // bars: one marker per bar, on the bar lines of the music (a bar is 2 s)
    let r = s.execute("sequence.markBeats", json!({"every": 4, "downbeat": true, "name": "Mesure"})).unwrap();
    let m = markers(&s);
    assert_eq!(m[0].1, "Mesure 1", "{m:?}");
    assert_eq!(m[1].1, "Mesure 2");
    assert!(r["barConfidence"].as_f64().unwrap() > 0.3, "the chords mark the bars: {r}");
    for (t, name) in &m {
        let off = (t / 2.0 - (t / 2.0).round()).abs() * 2.0;
        assert!(off < 0.05, "{name} at {t} s is not on a bar line");
    }
    assert!(r["summary"].as_str().unwrap().contains("mesure dès le beat"), "{r}");
}
