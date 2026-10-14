//! `assets.place`: the user's LUTs, sounds and title pictures dropped on the timeline (demo project).

use serde_json::{Value, json};

use crate::Session;
use crate::media_test_util::tmp_dir;
use filmcraft_project::{ClipId, ParamValue, TrackKind};
use filmcraft_time::Tick;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn first_video_clip(s: &Session) -> (ClipId, Tick) {
    let q = s.active_sequence().unwrap();
    let it = &q.video_tracks[0].items[0];
    (it.id, it.start + Tick(it.duration.0 / 2))
}

fn look_of(s: &Session, clip: ClipId) -> Option<String> {
    let q = s.active_sequence().unwrap();
    let e = q.find_item(clip).unwrap().1.effects.iter().find(|e| e.effect == "lumetri")?;
    match e.param("look_lut").map(|v| &v.value) {
        Some(ParamValue::Text(t)) if !t.is_empty() => Some(t.clone()),
        _ => None,
    }
}

/// A warm 2³ `.cube` (red up, blue down).
fn warm_cube(dir: &std::path::Path) -> String {
    let mut t = String::from("TITLE \"Warm test\"\nLUT_3D_SIZE 2\n");
    for b in 0..2 {
        for g in 0..2 {
            for r in 0..2 {
                t.push_str(&format!("{} {} {}\n", (r as f32 * 0.8 + 0.2).min(1.0), g as f32, b as f32 * 0.7));
            }
        }
    }
    let p = dir.join("Warm test.cube");
    std::fs::write(&p, t).unwrap();
    p.to_string_lossy().into_owned()
}

/// A one-second 48 kHz mono 16-bit WAV (440 Hz).
fn wav(dir: &std::path::Path, name: &str) -> String {
    let n = 48_000u32;
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + n * 2).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&48_000u32.to_le_bytes());
    b.extend_from_slice(&96_000u32.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(n * 2).to_le_bytes());
    for i in 0..n {
        let v = ((i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 8000.0) as i16;
        b.extend_from_slice(&v.to_le_bytes());
    }
    let p = dir.join(name);
    std::fs::write(&p, b).unwrap();
    p.to_string_lossy().into_owned()
}

/// A 64×36 PNG with a transparent half (a title with alpha).
fn png(dir: &std::path::Path) -> String {
    let img = image::RgbaImage::from_fn(64, 36, |x, _| if x < 32 { image::Rgba([255, 255, 255, 255]) } else { image::Rgba([0, 0, 0, 0]) });
    let p = dir.join("title.png");
    img.save(&p).unwrap();
    p.to_string_lossy().into_owned()
}

fn clip_count(s: &Session, kind: TrackKind, index: usize) -> usize {
    let q = s.active_sequence().unwrap();
    let list = if kind == TrackKind::Video { &q.video_tracks } else { &q.audio_tracks };
    list.get(index).map(|t| t.items.len()).unwrap_or(0)
}

fn track_index(s: &Session, id: u64) -> (TrackKind, usize) {
    let q = s.active_sequence().unwrap();
    if let Some(i) = q.video_tracks.iter().position(|t| t.id.0 == id) {
        return (TrackKind::Video, i);
    }
    (TrackKind::Audio, q.audio_tracks.iter().position(|t| t.id.0 == id).expect("track"))
}

#[test]
fn a_lut_dropped_on_a_shot_becomes_its_creative_look_in_one_undo_step() {
    let mut s = demo();
    let d = tmp_dir("assets-lut");
    let cube = warm_cube(&d);
    let (clip, _) = first_video_clip(&s);
    let (luts0, undo0) = (s.project.luts.len(), s.history.undo.len());
    let r = s.execute("assets.place", json!({"paths": [cube], "clip": clip.0})).unwrap();
    assert_eq!(r["placed"][0]["action"], "look", "{r}");
    let look = look_of(&s, clip).expect("look set");
    assert!(look.starts_with("lib:"), "{look}");
    assert_eq!(s.project.luts.len(), luts0 + 1);
    assert_eq!(s.project.luts.last().unwrap().name, "Warm test");
    assert_eq!(s.history.undo.len(), undo0 + 1, "one undo step");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(look_of(&s, clip), None);
    assert_eq!(s.project.luts.len(), luts0);
}

#[test]
fn a_lut_dropped_elsewhere_only_joins_the_library_and_never_goes_on_a_sound() {
    let mut s = demo();
    let d = tmp_dir("assets-lut-lib");
    let cube = warm_cube(&d);
    let luts0 = s.project.luts.len();
    let r = s.execute("assets.place", json!({"paths": [cube.clone()]})).unwrap();
    assert_eq!(r["placed"][0]["action"], "library", "{r}");
    assert_eq!(s.project.luts.len(), luts0 + 1);
    let q = s.active_sequence().unwrap();
    assert!(q.video_tracks.iter().flat_map(|t| &t.items).all(|it| look_of(&s, it.id).is_none()));
    // on an audio clip: refused, nothing changes
    let a = s.active_sequence().unwrap().audio_tracks.iter().find_map(|t| t.items.first()).map(|it| it.id).expect("demo audio clip");
    let undo0 = s.history.undo.len();
    let e = s.execute("assets.place", json!({"paths": [cube], "clip": a.0})).unwrap_err().to_string();
    assert!(e.contains("video clip"), "{e}");
    assert_eq!(s.history.undo.len(), undo0);
}

#[test]
fn a_sound_dropped_on_a_shot_goes_on_a_free_audio_track_without_overwriting() {
    let mut s = demo();
    let d = tmp_dir("assets-sfx");
    let w = wav(&d, "whoosh.wav");
    let (clip, t) = first_video_clip(&s);
    let a_counts: Vec<usize> = (0..s.active_sequence().unwrap().audio_tracks.len()).map(|i| clip_count(&s, TrackKind::Audio, i)).collect();
    let v1 = clip_count(&s, TrackKind::Video, 0);
    let undo0 = s.history.undo.len();
    let tracks0 = s.active_sequence().unwrap().audio_tracks.len();
    let r = s.execute("assets.place", json!({"paths": [w], "clip": clip.0, "time": t.0})).unwrap();
    let placed = &r["placed"][0];
    assert_eq!(placed["action"], "placed", "{r}");
    assert_eq!(placed["time"], json!(t.0));
    let (kind, idx) = track_index(&s, placed["track"].as_u64().unwrap());
    assert_eq!(kind, TrackKind::Audio);
    // the chosen track had nothing at t; every clip already there is still there
    assert_eq!(clip_count(&s, TrackKind::Video, 0), v1);
    for (i, n) in a_counts.iter().enumerate() {
        let now = clip_count(&s, TrackKind::Audio, i);
        assert_eq!(now, if i == idx { n + 1 } else { *n }, "audio track {i}");
    }
    let q = s.active_sequence().unwrap();
    let c = ClipId(placed["clips"][0].as_u64().unwrap());
    let (_, it) = q.find_item(c).unwrap();
    assert_eq!(it.start, t);
    assert_eq!(s.history.undo.len(), undo0 + 1, "one undo step");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().audio_tracks.len(), tracks0);
    assert!(s.active_sequence().unwrap().find_item(c).is_none());
}

#[test]
fn a_sound_on_an_empty_audio_track_lands_there_and_a_busy_track_passes_it_down() {
    let mut s = demo();
    let d = tmp_dir("assets-sfx2");
    let w = wav(&d, "hit.wav");
    s.execute("sequence.addTracks", json!({"video": 0, "audio": 2})).unwrap();
    let q = s.active_sequence().unwrap();
    let last = q.audio_tracks.len() - 2;
    let empty = q.audio_tracks[last].id.0;
    let t = Tick(0);
    let r = s.execute("assets.place", json!({"paths": [w.clone()], "track": empty, "time": t.0})).unwrap();
    assert_eq!(r["placed"][0]["track"], json!(empty), "{r}");
    // the same sound at the same time on the now busy track: the next one down
    let r = s.execute("assets.place", json!({"paths": [w], "track": empty, "time": t.0})).unwrap();
    let (_, idx) = track_index(&s, r["placed"][0]["track"].as_u64().unwrap());
    assert_eq!(idx, last + 1, "{r}");
}

#[test]
fn a_title_dropped_on_a_shot_goes_above_it() {
    let mut s = demo();
    let d = tmp_dir("assets-title");
    let p = png(&d);
    let (clip, t) = first_video_clip(&s);
    let v1 = clip_count(&s, TrackKind::Video, 0);
    let r = s.execute("assets.place", json!({"paths": [p], "clip": clip.0, "time": t.0})).unwrap();
    let (kind, idx) = track_index(&s, r["placed"][0]["track"].as_u64().unwrap());
    assert_eq!((kind, idx > 0), (TrackKind::Video, true), "{r}");
    assert_eq!(clip_count(&s, TrackKind::Video, 0), v1, "V1 untouched");
}

#[test]
fn an_after_effects_mogrt_is_refused_as_preview_only_and_a_premiere_one_is_placed_as_a_title() {
    use crate::mogrt_fixtures::{Spec, after_effects, premiere};
    let mut s = demo();
    let d = tmp_dir("assets-mogrt");
    s.prefs_path = Some(d.join("prefs.json"));
    let ae = d.join("Lower third AE.mogrt");
    std::fs::write(&ae, after_effects().unwrap()).unwrap();
    let undo0 = s.history.undo.len();
    let e = s.execute("assets.place", json!({"paths": [ae.to_string_lossy()], "time": 0})).unwrap_err().to_string();
    assert!(e.contains("After Effects") && e.contains("preview only"), "{e}");
    assert_eq!(s.history.undo.len(), undo0);
    // mixed with a usable file: the usable one goes in, the After Effects title is listed as refused
    let w = wav(&d, "ok.wav");
    let r = s.execute("assets.place", json!({"paths": [ae.to_string_lossy(), w]})).unwrap();
    assert_eq!(r["refused"][0]["code"], "afterEffects", "{r}");
    assert_eq!(r["placed"][0]["action"], "imported", "{r}");
    let e = s.execute("graphics.template.install", json!({"path": ae.to_string_lossy()})).unwrap_err().to_string();
    assert!(e.contains("After Effects"), "{e}");
    // a Premiere title dropped on a shot: converted, placed above it, one undo step
    let pr = d.join("Lower third PR.mogrt");
    std::fs::write(&pr, premiere(&Spec { text: "Hello Title", font: "Inter-Bold", second_run_size: None, opacity_keys: false, extra_effect: false }).unwrap())
        .unwrap();
    let original = std::fs::read(&pr).unwrap();
    let (clip, t) = first_video_clip(&s);
    let v1 = clip_count(&s, TrackKind::Video, 0);
    let undo0 = s.history.undo.len();
    let r = s.execute("assets.place", json!({"paths": [pr.to_string_lossy()], "clip": clip.0, "time": t.0})).unwrap();
    let placed = &r["placed"][0];
    assert_eq!(placed["action"], "placed", "{r}");
    let (kind, idx) = track_index(&s, placed["track"].as_u64().unwrap());
    assert_eq!((kind, idx > 0), (TrackKind::Video, true), "{r}");
    assert_eq!(clip_count(&s, TrackKind::Video, 0), v1, "V1 untouched");
    assert_eq!(s.history.undo.len(), undo0 + 1, "one undo step");
    let c = ClipId(placed["clip"].as_u64().unwrap());
    let it = s.active_sequence().unwrap().find_item(c).unwrap().1.clone();
    let link = it.graphic.as_ref().and_then(|g| g.template.clone()).expect("template link");
    assert!(link.controls.iter().any(|c| c.param == "text"), "{link:?}");
    // the source was only read
    assert_eq!(std::fs::read(&pr).unwrap(), original);
}

#[test]
fn without_a_time_a_sound_is_only_imported() {
    let mut s = demo();
    let d = tmp_dir("assets-import");
    let w = wav(&d, "music.wav");
    let items0 = s.project.items.len();
    let r: Value = s.execute("assets.place", json!({"paths": [w]})).unwrap();
    assert_eq!(r["placed"][0]["action"], "imported", "{r}");
    assert_eq!(s.project.items.len(), items0 + 1);
}

#[test]
fn the_look_preview_frame_is_the_shot_and_an_identity_lut_keeps_it() {
    let s = demo();
    let (clip, t) = first_video_clip(&s);
    let f = crate::assets::look_base_frame(&s, clip, t, 160).expect("frame");
    assert!(f.width <= 160 && f.width > 0 && f.height > 0);
    assert_eq!(f.px.len(), f.width * f.height);
    let plain = crate::assets::apply_look_rgba8(&f, None);
    let ident =
        filmcraft_color::Lut::parse("LUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n", Some(filmcraft_color::LutFormat::Cube)).unwrap();
    let same = crate::assets::apply_look_rgba8(&f, Some(&ident));
    let maxd = plain.iter().zip(&same).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
    assert!(maxd <= 1, "identity LUT changes the preview by {maxd}");
    let d = tmp_dir("assets-prev");
    let warm = filmcraft_color::Lut::parse(&std::fs::read_to_string(warm_cube(&d)).unwrap(), None).unwrap();
    let w = crate::assets::apply_look_rgba8(&f, Some(&warm));
    let mean = |v: &[u8], c: usize| v.chunks(4).map(|p| p[c] as f64).sum::<f64>() / (v.len() / 4) as f64;
    assert!(mean(&w, 2) < mean(&plain, 2), "the warm LUT lowers blue");
    // the preview clip is the selected video clip, else the one under the playhead
    assert!(crate::assets::preview_clip(&s).is_some());
}
