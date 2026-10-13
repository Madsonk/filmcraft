//! assets-dnd: the user's LUTs, sounds and titles dropped from "the Finder" (egui dropped files at
//! a pointer position, and the control channel's `ui.dropFiles`) and dragged out of Window ▸
//! Resources, on the demo project.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render offscreen with wgpu and write
//! `assets-*.png`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::{ClipId, ParamValue};
use filmcraft_ui_egui::FilmcraftApp;
use serde_json::json;

type H = Harness<'static, FilmcraftApp>;

fn step(h: &mut H) {
    let ctx = h.ctx.clone();
    let mut raw = std::mem::take(h.input_mut());
    eframe::App::raw_input_hook(h.state_mut(), &ctx, &mut raw);
    *h.input_mut() = raw;
    ctx.request_repaint();
    h.step();
}

fn steps(h: &mut H, n: usize) {
    for _ in 0..n {
        step(h);
    }
}

fn tmp(name: &str) -> PathBuf {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let d = std::env::temp_dir().join(format!("fc-assets-ui-{name}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn harness() -> H {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
    if std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").is_some() {
        b = b.wgpu();
    }
    let mut h = b.build_eframe(move |_cc| FilmcraftApp::new(s));
    steps(&mut h, 6);
    h
}

fn snapshot(h: &mut H, name: &str) {
    if let Some(dir) = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR") {
        if let Ok(img) = h.render() {
            let _ = img.save(Path::new(&dir).join(format!("assets-{name}.png")));
        }
    }
}

fn rect(h: &H, id: &str) -> Option<egui::Rect> {
    let e = h.state().auto.elements.iter().find(|e| e.id == id).or_else(|| h.state().auto.find(id))?;
    let [x, y, w, hh] = e.rect;
    Some(egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, hh)))
}

fn wait_for(h: &mut H, what: &str, mut ok: impl FnMut(&H) -> bool) {
    for _ in 0..400 {
        if ok(h) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        step(h);
    }
    panic!("timed out waiting for {what}");
}

fn first_v1_clip(h: &H) -> ClipId {
    h.state().session.active_sequence().unwrap().video_tracks[0].items[0].id
}

/// Window point over the middle of a clip.
fn over_clip(h: &H, clip: ClipId) -> egui::Pos2 {
    let (x, y) = filmcraft_ui_egui::panels::timeline::locate(h.state(), clip.0, None).expect("clip visible");
    egui::pos2(x, y)
}

fn look_of(h: &H, clip: ClipId) -> Option<String> {
    let q = h.state().session.active_sequence().unwrap();
    let e = q.find_item(clip).unwrap().1.effects.iter().find(|e| e.effect == "lumetri")?;
    match e.param("look_lut").map(|v| &v.value) {
        Some(ParamValue::Text(t)) if !t.is_empty() => Some(t.clone()),
        _ => None,
    }
}

fn cube(dir: &Path, name: &str, warm: bool) -> String {
    let mut t = format!("TITLE \"{name}\"\nLUT_3D_SIZE 2\n");
    for b in 0..2 {
        for g in 0..2 {
            for r in 0..2 {
                let (r, g, b) = (r as f32, g as f32, b as f32);
                let (r, b) = if warm { ((r * 0.8 + 0.2).min(1.0), b * 0.7) } else { (r, b) };
                t.push_str(&format!("{r} {g} {b}\n"));
            }
        }
    }
    let p = dir.join(format!("{name}.cube"));
    std::fs::write(&p, t).unwrap();
    p.to_string_lossy().into_owned()
}

fn wav(dir: &Path, name: &str) -> String {
    let n = 24_000u32;
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
        b.extend_from_slice(&(((i as f32 * 0.05).sin() * 6000.0) as i16).to_le_bytes());
    }
    let p = dir.join(name);
    std::fs::write(&p, b).unwrap();
    p.to_string_lossy().into_owned()
}

/// A file dropped by "the Finder" (egui's native handle is the integration's).
#[derive(Debug)]
struct TestFile(PathBuf);

impl egui::DroppedFile for TestFile {
    fn path(&self) -> &Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

/// A drop from the Finder: the pointer moves there, then egui reports the dropped file.
fn os_drop(h: &mut H, path: &str, at: egui::Pos2) {
    h.input_mut().events.push(egui::Event::PointerMoved(at));
    step(h);
    h.input_mut().dropped_files.push(Arc::new(TestFile(PathBuf::from(path))));
    steps(h, 3);
}

fn audio_clip_count(h: &H) -> usize {
    h.state().session.active_sequence().unwrap().audio_tracks.iter().map(|t| t.items.len()).sum()
}

#[test]
fn a_lut_from_the_finder_on_a_shot_becomes_its_look_and_elsewhere_joins_the_library() {
    let mut h = harness();
    let d = tmp("lut");
    let lut = cube(&d, "Drop warm", true);
    let clip = first_v1_clip(&h);
    let at = over_clip(&h, clip);
    os_drop(&mut h, &lut, at);
    let look = look_of(&h, clip).unwrap_or_else(|| panic!("look set; status: {}", h.state().ui.status));
    assert!(look.starts_with("lib:"), "{look}");
    assert!(h.state().ui.status.contains("Creative Look"), "{}", h.state().ui.status);
    snapshot(&mut h, "01-lut-dropped-on-shot");
    // dropped on the Project panel: only the LUT library grows
    let luts = h.state().session.project.luts.len();
    let other = cube(&d, "Drop neutral", false);
    let proj = rect(&h, "panel.Project").expect("project panel").center();
    os_drop(&mut h, &other, proj);
    assert_eq!(h.state().session.project.luts.len(), luts + 1, "{}", h.state().ui.status);
    assert_eq!(look_of(&h, clip), Some(look), "the shot keeps its look");
}

#[test]
fn a_sound_from_the_finder_on_a_shot_lands_on_a_free_audio_track_and_a_mogrt_is_explained() {
    let mut h = harness();
    let d = tmp("sfx");
    let w = wav(&d, "whoosh.wav");
    let clip = first_v1_clip(&h);
    let start = h.state().session.active_sequence().unwrap().find_item(clip).unwrap().1.start;
    let n0 = audio_clip_count(&h);
    let at = over_clip(&h, clip);
    os_drop(&mut h, &w, at);
    assert_eq!(audio_clip_count(&h), n0 + 1, "{}", h.state().ui.status);
    let last = h.state().assets.last_drop.clone().expect("drop recorded");
    let placed = &last["result"]["placed"][0];
    assert_eq!(placed["action"], "placed", "{last}");
    let t = placed["time"].as_i64().unwrap();
    assert!(t >= start.0, "placed at the drop time, not at 0: {last}");
    // a .mogrt that was never converted: refused with the way to convert it, nothing changes
    let m = d.join("Lower third.mogrt");
    std::fs::write(&m, b"PK\x03\x04").unwrap();
    let undo = h.state().session.history.undo.len();
    os_drop(&mut h, &m.to_string_lossy(), at);
    assert_eq!(h.state().session.history.undo.len(), undo);
    assert!(h.state().ui.status.contains("mogrt_to_alpha"), "{}", h.state().ui.status);
}

#[test]
fn the_control_channel_drops_files_where_it_is_told() {
    use filmcraft_ui_egui::control::ControlRequest;
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let mut h = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| FilmcraftApp::new(s).with_control(rx));
    steps(&mut h, 6);
    let d = tmp("ctl");
    let lut = cube(&d, "Control warm", true);
    let clip = first_v1_clip(&h);
    let at = over_clip(&h, clip);
    let call = |h: &mut H, m: &str, p: serde_json::Value| {
        let (req, reply) = ControlRequest::new(m, p);
        tx.send(req).unwrap();
        for _ in 0..200 {
            step(h);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {m}");
    };
    let v = call(&mut h, "ui.dropFiles", json!({"paths": [lut], "x": at.x, "y": at.y}));
    assert_eq!(v["ok"], json!(true), "{v}");
    steps(&mut h, 3);
    assert!(look_of(&h, clip).is_some(), "{}", h.state().ui.status);
    let v = call(&mut h, "ui.assets", json!({}));
    assert_eq!(v["result"]["lastDrop"]["result"]["placed"][0]["action"], "look", "{v}");
}

fn open_resources(h: &mut H, luts: &Path, sfx: &Path, titles: &Path) {
    {
        let app = h.state_mut();
        app.assets.loaded_settings = true;
        app.assets.settings.luts = luts.to_string_lossy().into_owned();
        app.assets.settings.sfx = sfx.to_string_lossy().into_owned();
        app.assets.settings.music = String::new();
        app.assets.settings.titles = titles.to_string_lossy().into_owned();
        app.assets.settings.sound_db = String::new();
    }
    let ctx = h.ctx.clone();
    filmcraft_ui_egui::menus::invoke(h.state_mut(), &ctx, "window.panel.Resources", json!({})).expect("Window ▸ Resources");
    steps(h, 4);
}

fn drag_to(h: &mut H, from: egui::Pos2, to: egui::Pos2) {
    h.input_mut().events.push(egui::Event::PointerMoved(from));
    step(h);
    h.input_mut().events.push(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    step(h);
    for i in 1..=8 {
        let p = from + (to - from) * (i as f32 / 8.0);
        h.input_mut().events.push(egui::Event::PointerMoved(p));
        step(h);
    }
    h.input_mut().events.push(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    steps(h, 3);
}

#[test]
fn the_resources_panel_lists_previews_and_drags_luts_sounds_and_titles() {
    let mut h = harness();
    let root = tmp("panel");
    let (luts, sfx, titles) = (root.join("LUTS"), root.join("SFX/01 HITS"), root.join("TITLES"));
    for d in [&luts, &sfx, &titles] {
        std::fs::create_dir_all(d).unwrap();
    }
    let warm = cube(&luts, "AG Warm", true);
    cube(&luts, "Neutral", false);
    let w = wav(&sfx, "SFX · Hits · boom [impact · court · 0.5s].wav");
    // a converted title (any picture stands in for the ProRes 4444 + alpha video)
    let title = titles.join("Pack/Lower third.png");
    std::fs::create_dir_all(title.parent().unwrap()).unwrap();
    image::RgbaImage::from_fn(64, 36, |x, _| if x < 32 { image::Rgba([255, 255, 255, 255]) } else { image::Rgba([0, 0, 0, 0]) }).save(&title).unwrap();
    let idx = json!({"version": 1, "items": [{"source": "/lib/Pack/Lower third.mogrt", "pack": "Pack", "name": "Lower third", "video": title.to_string_lossy(), "duration": 2.0, "app": "aefx"}]});
    std::fs::write(titles.join("index.json"), idx.to_string()).unwrap();
    open_resources(&mut h, &luts, root.join("SFX").as_path(), &titles);

    // --- LUTs: listed from the background index, hover = preview on the clip under the playhead
    let clip = first_v1_clip(&h);
    let ctx = h.ctx.clone();
    filmcraft_ui_egui::menus::invoke(h.state_mut(), &ctx, "timeline.select", json!({"clips": [clip.0]})).unwrap();
    wait_for(&mut h, "LUT rows", |h| rect(h, "resources.row.1").is_some());
    let row0 = rect(&h, "resources.row.0").unwrap();
    h.input_mut().events.push(egui::Event::PointerMoved(row0.center()));
    wait_for(&mut h, "LUT preview", |h| h.state().assets.preview.after_of.as_deref() == Some(warm.as_str()));
    assert!(h.state().assets.preview.before.is_some() && h.state().assets.preview.after.is_some());
    snapshot(&mut h, "02-resources-lut-preview");
    assert!(look_of(&h, clip).is_none(), "hovering writes nothing to the project");
    // drag the LUT row onto the shot
    let to = over_clip(&h, clip);
    drag_to(&mut h, row0.center(), to);
    assert!(look_of(&h, clip).is_some(), "look from a dragged row; status: {}", h.state().ui.status);

    // --- Sound effects: search, ▶ through the host, drag onto the shot = free audio track
    let played: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
    let rec = played.clone();
    h.state_mut().hooks.preview_audio = Some(Box::new(move |p: Option<&str>| {
        rec.lock().unwrap().push(p.map(str::to_string));
        true
    }));
    let tab = rect(&h, "resources.tab.sfx").unwrap().center();
    h.input_mut().events.push(egui::Event::PointerMoved(tab));
    step(&mut h);
    h.input_mut().events.push(egui::Event::PointerButton { pos: tab, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    h.input_mut().events.push(egui::Event::PointerButton { pos: tab, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    steps(&mut h, 2);
    h.state_mut().assets.query = "hits boom".into();
    wait_for(&mut h, "sound row", |h| rect(h, "resources.play.0").is_some());
    let play = rect(&h, "resources.play.0").unwrap().center();
    h.input_mut().events.push(egui::Event::PointerMoved(play));
    step(&mut h);
    h.input_mut().events.push(egui::Event::PointerButton { pos: play, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    h.input_mut().events.push(egui::Event::PointerButton { pos: play, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    steps(&mut h, 2);
    assert_eq!(played.lock().unwrap().last().cloned().flatten().as_deref(), Some(w.as_str()));
    assert_eq!(h.state().assets.playing.as_deref(), Some(w.as_str()));
    snapshot(&mut h, "03-resources-sfx");
    let n0 = audio_clip_count(&h);
    let row = rect(&h, "resources.row.0").unwrap();
    drag_to(&mut h, row.center() + egui::vec2(40.0, 0.0), to);
    assert_eq!(audio_clip_count(&h), n0 + 1, "sound placed; status: {}", h.state().ui.status);

    // --- Titles: the converted title goes on a video track above the shot
    let tab = rect(&h, "resources.tab.titles").unwrap().center();
    h.input_mut().events.push(egui::Event::PointerMoved(tab));
    step(&mut h);
    h.input_mut().events.push(egui::Event::PointerButton { pos: tab, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    h.input_mut().events.push(egui::Event::PointerButton { pos: tab, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    steps(&mut h, 2);
    h.state_mut().assets.query.clear();
    assert_eq!(h.state().assets.tab, Some(filmcraft_ui_egui::assets::index::Kind::Titles));
    wait_for(&mut h, "title row", |h| {
        h.state().assets.libs.get(&filmcraft_ui_egui::assets::index::Kind::Titles).is_some_and(|l| l.snapshot().entries.len() == 1)
            && h.state().auto.elements.iter().any(|e| e.id == "resources.row.0" && e.label == "Lower third")
    });
    snapshot(&mut h, "04-resources-titles");
    let v1 = h.state().session.active_sequence().unwrap().video_tracks[0].items.len();
    let row = rect(&h, "resources.row.0").unwrap();
    drag_to(&mut h, row.center(), to);
    let q = h.state().session.active_sequence().unwrap();
    assert_eq!(q.video_tracks[0].items.len(), v1, "V1 untouched");
    assert!(
        q.video_tracks.iter().skip(1).any(|t| t.items.iter().any(|it| it.name.contains("Lower third"))),
        "title above; status: {} drop: {:?} tracks: {:?}",
        h.state().ui.status,
        h.state().assets.last_drop,
        q.video_tracks.iter().map(|t| t.items.iter().map(|i| i.name.clone()).collect::<Vec<_>>()).collect::<Vec<_>>()
    );
}
