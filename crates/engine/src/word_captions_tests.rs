use std::sync::Arc;

use serde_json::json;

use crate::Session;
use filmcraft_project::graphic::{LayerContent, eval_layer};
use filmcraft_project::{ParamValue, Transcript, Word};
use filmcraft_speech::FixedTranscriber;
use filmcraft_time::Tick;

/// The demo project with the transcript "Hello um world." (0.2-1.4 s) + "Second speaker here." (2.6-4.0 s).
fn session() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    let a = &q.audio_tracks[0].items[0];
    let (item, sin) = (a.item, a.source_in);
    let sec = |x: f64| sin + Tick::from_seconds_f64(x);
    let mut t = Transcript { language: "en".into(), ..Default::default() };
    for (text, a, b, sp) in
        [("Hello", 0.2, 0.5, 0), ("um", 0.6, 0.9, 0), ("world.", 1.0, 1.4, 0), ("Second", 2.6, 3.0, 0), ("speaker", 3.0, 3.5, 0), ("here.", 3.5, 4.0, 0)]
    {
        let mut w = Word::new(text, sec(a), sec(b));
        w.speaker = Some(sp);
        t.words.push(w);
    }
    t.normalize();
    s.transcriber = Some(Arc::new(FixedTranscriber { transcript: t, id: "fixed".into() }));
    s.execute("transcript.generate", json!({"items": [item.0]})).unwrap();
    s
}

fn new_track(s: &Session) -> &filmcraft_project::Track {
    s.active_sequence().unwrap().video_tracks.last().unwrap()
}

#[test]
fn karaoke_groups_words_and_highlights_the_spoken_one() {
    let mut s = session();
    let tracks = s.active_sequence().unwrap().video_tracks.len();
    let r = s.execute("transcript.createWordCaptions", json!({"style": "karaoke", "maxChars": 24})).unwrap();
    assert_eq!(r["clips"], 2, "{r}");
    assert_eq!(s.active_sequence().unwrap().video_tracks.len(), tracks + 1);
    let t = new_track(&s);
    assert_eq!(t.name, "Sous-titres animés");
    assert_eq!(t.items.len(), 2);
    assert!(t.items[0].end() <= t.items[1].start, "clips of the track never overlap");
    let c = &t.items[0];
    assert_eq!(c.name, "Hello um world.");
    // base text + one overlay per word
    let layers: Vec<_> = c.effects.iter().filter(|e| filmcraft_project::graphic::is_layer(e)).collect();
    assert_eq!(layers.len(), 4);
    // overlay k is accent-coloured only while word k is spoken (hold keyframes)
    let fill_alpha = |k: usize, at: f64| -> f32 {
        let e = layers[1 + k];
        e.params["fill_color"].value_at(Tick::from_seconds_f64(at)).as_color().unwrap()[3]
    };
    let start_of = |sec: f64| Tick::from_seconds_f64(sec) - (c.start - s.active_sequence().unwrap().audio_tracks[0].items[0].start);
    let _ = start_of;
    let a0 = s.active_sequence().unwrap().audio_tracks[0].items[0].start;
    let rel = |sec: f64| (a0 + Tick::from_seconds_f64(sec) - c.start).seconds();
    assert_eq!(fill_alpha(0, rel(0.3)), 1.0);
    assert_eq!(fill_alpha(1, rel(0.3)), 0.0);
    assert_eq!(fill_alpha(1, rel(0.7)), 1.0);
    assert_eq!(fill_alpha(0, rel(0.7)), 0.0);
    assert_eq!(fill_alpha(2, rel(1.2)), 1.0);
    // the transparent-fill runs hide every other word of an overlay's text
    let spec = eval_layer(layers[2], Tick::from_seconds_f64(rel(0.7)), (1920, 1080)).unwrap();
    let LayerContent::Text(tp) = &spec.content else { panic!() };
    assert_eq!(tp.text, "Hello um world.");
    assert_eq!(tp.runs.len(), 2);
    // pop-in on every layer
    for e in &layers {
        assert!(e.params["scale"].is_animated() && e.params["opacity"].is_animated());
    }
    // one undo step removes the track again
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().video_tracks.len(), tracks);
}

#[test]
fn pop_style_makes_one_clip_per_word() {
    let mut s = session();
    let r = s.execute("transcript.createWordCaptions", json!({"style": "pop", "accent": "#00ff88", "caps": false, "size": 140})).unwrap();
    assert_eq!(r["clips"], 6, "{r}");
    let c = &new_track(&s).items[1];
    assert_eq!(c.name, "um");
    let e = c.effects.iter().find(|e| filmcraft_project::graphic::is_layer(e)).unwrap();
    assert!((e.params["fill_color"].value.as_color().unwrap()[1] - 1.0).abs() < 1e-6);
    assert_eq!(e.params["size"].value.as_f64(), Some(140.0));
    assert!(matches!(e.params["caps"].value, ParamValue::Choice(0)));
    let sc = |at: f64| e.params["scale"].value_at(Tick::from_seconds_f64(at)).as_f64().unwrap();
    assert!(sc(0.0) < 100.0 && sc(0.13) > 100.0 && (sc(0.4) - 100.0).abs() < 1e-9, "springs in: {} {} {}", sc(0.0), sc(0.13), sc(0.4));
}

#[test]
fn bad_arguments_are_refused() {
    let mut s = session();
    assert!(s.execute("transcript.createWordCaptions", json!({"style": "spin"})).is_err());
    assert!(s.execute("transcript.createWordCaptions", json!({"accent": "yellow"})).is_err());
    let tracks = s.active_sequence().unwrap().video_tracks.len();
    assert_eq!(s.active_sequence().unwrap().video_tracks.len(), tracks, "a refused call changes nothing");
    let mut empty = Session::default();
    empty.execute("file.openDemoProject", json!({})).unwrap();
    assert!(empty.execute("transcript.createWordCaptions", json!({})).is_err(), "needs a transcript");
}

#[test]
fn hex_colours() {
    assert_eq!(super::parse_hex("#ff0000"), Some([1.0, 0.0, 0.0, 1.0]));
    assert!(super::parse_hex("ff00").is_none());
}
