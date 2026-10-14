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
    if let Some(dir) = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR")
        && let Ok(img) = h.render()
    {
        let _ = img.save(Path::new(&dir).join(format!("assets-{name}.png")));
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
    // a .mogrt made in After Effects: refused ("preview only"), nothing changes
    let m = d.join("Lower third.mogrt");
    std::fs::write(&m, filmcraft_engine::mogrt_fixtures::after_effects().unwrap()).unwrap();
    let undo = h.state().session.history.undo.len();
    os_drop(&mut h, &m.to_string_lossy(), at);
    assert_eq!(h.state().session.history.undo.len(), undo);
    assert!(h.state().ui.status.contains("After Effects") && h.state().ui.status.contains("preview only"), "{}", h.state().ui.status);
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

/// A drop over a transition (the striped block on a cut) goes to the incoming clip, not into the
/// Project bin (seen in the running app: a sound dragged near a cut was only imported).
#[test]
fn a_drop_over_a_transition_lands_on_its_clip() {
    let mut h = harness();
    let d = tmp("transition");
    let lut = cube(&d, "Over the cut", true);
    let snd = wav(&d, "Whoosh on the cut.wav");
    // a transition on a cut (both clips): the drop goes to the incoming one
    let tr = h.state().session.active_sequence().unwrap().video_tracks[0]
        .transitions
        .iter()
        .find(|t| t.from.is_some() && t.to.is_some())
        .cloned()
        .expect("the demo has a transition on a V1 cut");
    let (from, clip) = (tr.from.unwrap(), tr.to.unwrap());
    let (x, y) = filmcraft_ui_egui::panels::timeline::locate(h.state(), from.0, Some("out")).expect("clip visible");
    let at = egui::pos2(x, y);
    let hit = filmcraft_ui_egui::panels::timeline::hit_json(h.state(), at, egui::Modifiers::NONE);
    assert_eq!(hit["hit"], "transition", "the point is over the transition: {hit}");
    let audio0 = audio_clip_count(&h);
    os_drop(&mut h, &lut, at);
    assert!(look_of(&h, clip).is_some(), "LUT on the incoming clip of {tr:?}: {:?}", h.state().assets.last_drop);
    os_drop(&mut h, &snd, at);
    assert_eq!(audio_clip_count(&h), audio0 + 1, "the sound is placed on the timeline: {}", h.state().ui.status);
}

/// titles-native: the Titles tab lists a Premiere title as editable and an After Effects one as
/// preview only; dragging the Premiere one onto a shot converts it and places an editable title
/// above; "Convert all shown" converts in the background into the user's graphics templates.
#[test]
fn a_premiere_title_from_the_titles_tab_becomes_an_editable_title_above_the_shot() {
    use filmcraft_engine::mogrt_fixtures::{Spec, after_effects, premiere};
    let mut h = harness();
    let root = tmp("titles-native");
    h.state_mut().session.prefs_path = Some(root.join("data/prefs.json"));
    let titles = root.join("TITLES");
    std::fs::create_dir_all(&titles).unwrap();
    let pr = root.join("Lower third PR.mogrt");
    std::fs::write(&pr, premiere(&Spec { text: "Madson Kurtis", font: "Inter-Bold", second_run_size: None, opacity_keys: true, extra_effect: false }).unwrap()).unwrap();
    let ae = root.join("Lower third AE.mogrt");
    std::fs::write(&ae, after_effects().unwrap()).unwrap();
    let preview = root.join("ae-preview.png");
    image::RgbaImage::from_fn(32, 18, |_, _| image::Rgba([200, 200, 200, 255])).save(&preview).unwrap();
    let idx = json!({"version": 1, "items": [
        {"source": pr.to_string_lossy(), "pack": "Pack", "name": "Premiere title", "app": "ppro", "template": "project.prgraphic", "duration": 5.0},
        {"source": ae.to_string_lossy(), "pack": "Pack", "name": "AE title", "app": "aefx", "template": "project.aegraphic", "thumb": preview.to_string_lossy()}
    ]});
    std::fs::write(titles.join("index.json"), idx.to_string()).unwrap();
    open_resources(&mut h, &root, &root, &titles);
    h.state_mut().assets.tab = Some(filmcraft_ui_egui::assets::index::Kind::Titles);
    h.state_mut().assets.filtered = None;
    wait_for(&mut h, "title rows", |h| {
        let els = &h.state().auto.elements;
        els.iter().any(|e| e.id == "resources.row.0" && e.label == "Premiere title") && els.iter().any(|e| e.id == "resources.row.1" && e.label == "AE title")
    });
    snapshot(&mut h, "05-titles-editable-vs-preview");
    let entries = h.state().assets.libs.get(&filmcraft_ui_egui::assets::index::Kind::Titles).unwrap().snapshot().entries;
    assert!(entries[0].ready && entries[0].path.ends_with(".mogrt") && !entries[1].ready, "{entries:?}");
    // drag the Premiere title onto a shot
    let clip = first_v1_clip(&h);
    let to = over_clip(&h, clip);
    let v1 = h.state().session.active_sequence().unwrap().video_tracks[0].items.len();
    let row = rect(&h, "resources.row.0").unwrap();
    drag_to(&mut h, row.center(), to);
    let q = h.state().session.active_sequence().unwrap();
    assert_eq!(q.video_tracks[0].items.len(), v1, "V1 untouched");
    let placed = q
        .video_tracks
        .iter()
        .skip(1)
        .flat_map(|t| &t.items)
        .find(|it| it.graphic.as_ref().and_then(|g| g.template.as_ref()).is_some_and(|l| l.name == "Test Lower Third"))
        .cloned()
        .unwrap_or_else(|| panic!("editable title above; status: {} drop: {:?}", h.state().ui.status, h.state().assets.last_drop));
    let link = placed.graphic.as_ref().unwrap().template.clone().unwrap();
    assert!(link.controls.iter().any(|c| c.param == "text"));
    // its text is editable (Essential Graphics ▸ Edit uses graphics.template.set)
    let ctl = link.controls.iter().find(|c| c.param == "text").unwrap().id.clone();
    let ctx = h.ctx.clone();
    filmcraft_ui_egui::menus::invoke(h.state_mut(), &ctx, "graphics.template.set", json!({"clip": placed.id.0, "control": ctl, "value": "Bonjour"})).unwrap();
    // the After Effects one: its preview, as a reference, with the reason said
    let row = rect(&h, "resources.row.1").unwrap();
    drag_to(&mut h, row.center(), to);
    assert!(h.state().ui.status.contains("After Effects") || h.state().ui.status.contains("placed"), "{}", h.state().ui.status);
    // convert all shown: one Premiere title → one .fcgt in the user's templates
    wait_for(&mut h, "convert button", |h| rect(h, "resources.convertAll").is_some());
    let b = rect(&h, "resources.convertAll").unwrap().center();
    h.input_mut().events.push(egui::Event::PointerMoved(b));
    step(&mut h);
    h.input_mut().events.push(egui::Event::PointerButton { pos: b, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    h.input_mut().events.push(egui::Event::PointerButton { pos: b, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    steps(&mut h, 2);
    wait_for(&mut h, "conversion done", |h| h.state().assets.convert_job.as_ref().is_some_and(|j| !j.lock().unwrap().running));
    let j = h.state().assets.convert_job.as_ref().unwrap().lock().unwrap().clone();
    assert_eq!((j.total, j.done, j.failed), (1, 1, 0), "{j:?}");
    let fcgt: Vec<_> = std::fs::read_dir(root.join("data/Graphics Templates")).unwrap().filter_map(|e| e.ok()).filter(|e| e.path().extension().is_some_and(|x| x == "fcgt")).collect();
    assert_eq!(fcgt.len(), 1);
    snapshot(&mut h, "06-titles-converted");
}
