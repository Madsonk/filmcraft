//! The user's own resources (assets-dnd): LUTs, sound effects, music and converted titles, dropped
//! on the timeline from the Finder or from Window ▸ Resources.
//!
//! - Where a drop lands is decided here (Timeline under the pointer: clip, track, time; anywhere
//!   else: the Project panel's bin), what it does is `assets.place` in the engine (LUT → Creative
//!   Look or LUT library, sound → a free audio track, picture/title → a free video track above).
//! - A `.mogrt` is swapped for its converted video when `mogrt_to_alpha` made one (Titles index),
//!   else the engine refuses it with the way to convert it.
//! - Library folders and indexes live in `<data>/assets/` (`settings.json`, `index-<kind>.json`).
//!
//! macOS gives no pointer position during a drag from the Finder (winit 0.30 reports the files,
//! not where they are): the host's [`crate::HostHooks::pointer_in_window`] reads it from AppKit.

pub mod index;

use std::path::{Path, PathBuf};

use egui::{Color32, Pos2, Rect, vec2};
use filmcraft_engine::assets::AssetKind;
use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::dock::PanelKind;
use crate::theme::Tokens;
use index::{Kind, Library};

/// Madson's resource library (used for the default folders when it is there).
pub const DEFAULT_LIBRARY: &str = "/Volumes/Qnap bureau/6. Garden/Ressources";
/// Where `mogrt_to_alpha.py` writes converted titles by default.
pub const DEFAULT_TITLES: &str = "/Volumes/Studio Mad/mogrt-alpha";

/// Window ▸ Resources folders (`<data>/assets/settings.json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AssetSettings {
    pub luts: String,
    pub sfx: String,
    pub music: String,
    pub titles: String,
    /// SQLite sound index of the `/sfx` tool, used to seed the sound indexes.
    pub sound_db: String,
}

impl Default for AssetSettings {
    fn default() -> Self {
        let lib = Path::new(DEFAULT_LIBRARY);
        let here = |sub: &str| {
            let p = lib.join(sub);
            if p.is_dir() { p.to_string_lossy().into_owned() } else { String::new() }
        };
        let db = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("audio-tools/audio_index.db")).filter(|p| p.exists());
        AssetSettings {
            luts: here("03 LUTS"),
            sfx: here("02 SFX"),
            music: here("01 MUSIC"),
            titles: if Path::new(DEFAULT_TITLES).parent().is_some_and(Path::is_dir) { DEFAULT_TITLES.into() } else { String::new() },
            sound_db: db.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        }
    }
}

impl AssetSettings {
    pub fn root(&self, k: Kind) -> &str {
        match k {
            Kind::Luts => &self.luts,
            Kind::Sfx => &self.sfx,
            Kind::Music => &self.music,
            Kind::Titles => &self.titles,
        }
    }
    pub fn set_root(&mut self, k: Kind, v: String) {
        match k {
            Kind::Luts => self.luts = v,
            Kind::Sfx => self.sfx = v,
            Kind::Music => self.music = v,
            Kind::Titles => self.titles = v,
        }
    }
}

/// A resource being dragged out of Window ▸ Resources.
#[derive(Clone, Debug, PartialEq)]
pub struct AssetDrag {
    pub paths: Vec<String>,
    pub label: String,
}

/// LUT preview of the Resources panel: the shot as its look slot sees it, with and without the
/// hovered LUT.
#[derive(Default)]
pub struct LookPreview {
    /// (clip, playhead, history length) the base frame was rendered for.
    pub key: Option<(u64, i64, usize)>,
    pub base: Option<filmcraft_engine::assets::SignalFrame>,
    pub before: Option<egui::TextureHandle>,
    /// LUT path shown in `after`.
    pub after_of: Option<String>,
    pub after: Option<egui::TextureHandle>,
    /// Parsed LUTs read in the background: path → LUT (or the error).
    pub luts: std::collections::HashMap<String, Result<std::sync::Arc<filmcraft_color::Lut>, String>>,
    pub loading: std::sync::Arc<std::sync::Mutex<Vec<(String, Result<filmcraft_color::Lut, String>)>>>,
    pub asked: std::collections::HashSet<String>,
}

/// State of the Resources panel and of drops.
#[derive(Default)]
pub struct AssetsState {
    pub settings: AssetSettings,
    pub loaded_settings: bool,
    pub tab: Option<Kind>,
    pub query: String,
    pub libs: std::collections::HashMap<Kind, Library>,
    /// Filtered rows: (kind, snapshot rev, query) → indices.
    pub filtered: Option<((Kind, u64, String), Vec<usize>)>,
    pub drag: Option<AssetDrag>,
    /// Sound being auditioned (path).
    pub playing: Option<String>,
    /// LUT row under the pointer (preview).
    pub hover_lut: Option<String>,
    pub preview: LookPreview,
    /// A drop made by the control channel (`ui.dropFiles`): paths and window point.
    pub synthetic_drop: Option<(Vec<String>, Pos2)>,
    /// Last drop's result, for the control channel and tests.
    pub last_drop: Option<Value>,
}

impl AssetsState {
    fn settings_path(app_dir: Option<&Path>) -> Option<PathBuf> {
        app_dir.map(|d| d.join("assets").join("settings.json"))
    }
}

fn data_dir(app: &FilmcraftApp) -> Option<PathBuf> {
    app.session.prefs_path.as_ref().and_then(|p| p.parent()).map(Path::to_path_buf)
}

/// Load `settings.json` once.
pub fn ensure_settings(app: &mut FilmcraftApp) {
    if app.assets.loaded_settings {
        return;
    }
    app.assets.loaded_settings = true;
    if let Some(p) = AssetsState::settings_path(data_dir(app).as_deref())
        && let Ok(b) = std::fs::read(&p)
        && let Ok(s) = serde_json::from_slice::<AssetSettings>(&b)
    {
        app.assets.settings = s;
    }
}

pub fn save_settings(app: &FilmcraftApp) {
    if let Some(p) = AssetsState::settings_path(data_dir(app).as_deref()) {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(b) = serde_json::to_vec_pretty(&app.assets.settings) {
            let _ = std::fs::write(p, b);
        }
    }
}

/// The library of `k`, started (in the background) the first time it is asked for.
pub fn library(app: &mut FilmcraftApp, k: Kind) -> Option<&Library> {
    ensure_settings(app);
    let root = app.assets.settings.root(k).to_string();
    if root.is_empty() {
        app.assets.libs.remove(&k);
        return None;
    }
    if app.assets.libs.get(&k).is_none_or(|l| l.root != root) {
        let cache = data_dir(app).map(|d| d.join("assets").join(format!("index-{}.json", k.id())));
        let db = matches!(k, Kind::Sfx | Kind::Music).then(|| PathBuf::from(&app.assets.settings.sound_db)).filter(|p| !p.as_os_str().is_empty());
        app.assets.libs.insert(k, Library::start(k, &root, cache, db));
    }
    app.assets.libs.get(&k)
}

/// Re-walk a library (Refresh button): the cache keeps what did not change.
pub fn refresh(app: &mut FilmcraftApp, k: Kind) {
    app.assets.libs.remove(&k);
    app.assets.filtered = None;
    let _ = library(app, k);
}

/// The converted video of a `.mogrt`, from the Titles index.
fn converted_title(app: &mut FilmcraftApp, mogrt: &str) -> Option<String> {
    let lib = library(app, Kind::Titles)?;
    let s = lib.snapshot();
    s.entries.iter().find(|e| e.ready && e.source.as_deref() == Some(mogrt)).map(|e| e.path.clone())
}

/// Where a drop at `pos` lands: the `assets.place` parameters (without `paths`) and a short
/// description of the target.
pub fn target_at(app: &FilmcraftApp, pos: Pos2) -> (Value, String) {
    let over = |p: PanelKind| {
        app.auto
            .find(&format!("panel.{}", p.id()))
            .is_some_and(|e| Rect::from_min_size(egui::pos2(e.rect[0], e.rect[1]), vec2(e.rect[2], e.rect[3])).contains(pos))
    };
    if over(PanelKind::Timeline) && app.session.active_sequence().is_some() {
        let h = crate::panels::timeline::hit_json(app, pos, egui::Modifiers::NONE);
        let rate = app.session.sequence_rate();
        let t = |v: &Value| v.as_i64().map(|t| rate.snap_nearest(Tick(t.max(0))).0);
        match h["hit"].as_str() {
            Some("clip") => {
                let clip = h["clip"].as_u64().unwrap_or(0);
                let name =
                    app.session.active_sequence().and_then(|q| q.find_item(filmcraft_project::ClipId(clip))).map(|(_, it)| it.name.clone()).unwrap_or_default();
                return (json!({"clip": clip, "time": t(&h["time"])}), name);
            }
            Some("empty") => {
                let track = h["track"].as_u64();
                let name = app
                    .session
                    .active_sequence()
                    .and_then(|q| track.and_then(|id| q.track(filmcraft_project::TrackId(id))))
                    .map(|t| t.name.clone())
                    .unwrap_or_default();
                return (json!({"track": track, "time": t(&h["time"])}), name);
            }
            // a transition sits on its clips' edges: the drop goes to the incoming clip (the
            // outgoing one for a fade to nothing) at the transition's middle, not into the bin
            Some("transition") => {
                let found = app.session.active_sequence().and_then(|q| {
                    let tr = q.track(filmcraft_project::TrackId(h["track"].as_u64()?))?;
                    let x = tr.transitions.iter().find(|x| Some(x.id.0) == h["transition"].as_u64())?;
                    let clip = x.to.or(x.from)?;
                    let name = q.find_item(clip).map(|(_, it)| it.name.clone()).unwrap_or_default();
                    Some((clip.0, x.start.0 + x.duration.0 / 2, name))
                });
                if let Some((clip, mid, name)) = found {
                    return (json!({"clip": clip, "time": rate.snap_nearest(Tick(mid.max(0))).0}), name);
                }
            }
            Some("ruler") => return (json!({"time": t(&h["time"])}), String::new()),
            _ => {}
        }
    }
    (json!({"bin": app.import_bin().0}), crate::i18n::t("Project").to_string())
}

/// What dropping `paths` at `pos` would do, for the hint shown near the pointer.
pub fn describe(app: &FilmcraftApp, paths: &[String], pos: Pos2) -> String {
    let (q, name) = target_at(app, pos);
    let kind = paths.first().map(|p| AssetKind::of(p)).unwrap_or(AssetKind::Other);
    let on_clip = q.get("clip").is_some();
    let on_timeline = q.get("time").is_some();
    match (kind, on_clip, on_timeline) {
        (AssetKind::Lut, true, _) => tlf!("Creative Look on {clip}", clip = name),
        (AssetKind::Lut, false, _) => tl!("Add to the LUT library").to_string(),
        (AssetKind::Mogrt, _, _) => tl!("Adobe template: needs converting (see Resources ▸ Titles)").to_string(),
        (AssetKind::Sound, _, true) => tl!("On a free audio track at this time").to_string(),
        (AssetKind::Picture, _, true) => tl!("On a free video track at this time").to_string(),
        _ => tlf!("Import into {bin}", bin = name),
    }
}

/// Drop `paths` at `pos` (OS drop, control-channel drop, or a row of the Resources panel).
/// LUTs, sounds, pictures and `.mogrt` go through `assets.place`; other files (projects, XML,
/// captions) are imported as before.
pub fn drop_files(app: &mut FilmcraftApp, paths: Vec<String>, pos: Option<Pos2>) {
    let (assets, other): (Vec<String>, Vec<String>) = paths.into_iter().partition(|p| AssetKind::of(p) != AssetKind::Other);
    if !other.is_empty() {
        let _ = app.session.execute("file.import", json!({"paths": other, "bin": app.import_bin().0}));
    }
    if assets.is_empty() {
        return;
    }
    let assets: Vec<String> =
        assets.into_iter().map(|p| if AssetKind::of(&p) == AssetKind::Mogrt { converted_title(app, &p).unwrap_or(p) } else { p }).collect();
    let mut q = match pos {
        Some(p) => target_at(app, p).0,
        None => json!({"bin": app.import_bin().0}),
    };
    q["paths"] = json!(assets);
    let r = app.session.execute("assets.place", q.clone());
    app.ui.status = match &r {
        Ok(v) => status_of(v),
        Err(e) if assets.iter().any(|p| AssetKind::of(p) == AssetKind::Mogrt) && e.to_string().contains("mogrt_to_alpha") => mogrt_message(),
        Err(e) if e.to_string().contains("not on a sound") => tl!("A LUT goes on a video clip, not on a sound").to_string(),
        Err(e) => e.to_string(),
    };
    app.assets.last_drop = Some(match r {
        Ok(v) => json!({"params": q, "result": v}),
        Err(e) => json!({"params": q, "error": e.to_string()}),
    });
}

fn mogrt_message() -> String {
    tl!("FilmCraft can't read Adobe .mogrt templates: convert it into a video with transparency (mogrt_to_alpha, see Window ▸ Resources ▸ Titles), then drop the video").to_string()
}

fn status_of(v: &Value) -> String {
    let placed = v["placed"].as_array().cloned().unwrap_or_default();
    let refused = v["refused"].as_array().cloned().unwrap_or_default();
    let count = |a: &str| placed.iter().filter(|p| p["action"] == a).count();
    let mut parts = Vec::new();
    if count("look") > 0 {
        parts.push(tlf!("Creative Look applied ({n})", n = count("look")));
    }
    if count("library") > 0 {
        parts.push(tlf!("{n} LUT(s) added to the LUT library", n = count("library")));
    }
    if count("placed") > 0 {
        parts.push(tlf!("{n} resource(s) placed on the timeline", n = count("placed")));
    }
    if count("imported") > 0 {
        parts.push(tlf!("{n} file(s) imported", n = count("imported")));
    }
    if refused.iter().any(|r| r["code"] == "mogrt") {
        parts.push(mogrt_message());
    }
    let other = refused.iter().filter(|r| r["code"] != "mogrt").count();
    if other > 0 {
        parts.push(tlf!(
            "{n} file(s) refused: {why}",
            n = other,
            why = refused.iter().find(|r| r["code"] != "mogrt").and_then(|r| r["message"].as_str()).unwrap_or("")
        ));
    }
    parts.join(" · ")
}

/// Each frame, after the panels: the drag ghost of a resource and its drop, the hint of a drag
/// from the Finder, and the control channel's drop.
pub fn after_panels(app: &mut FilmcraftApp, ctx: &egui::Context) {
    if let Some((paths, pos)) = app.assets.synthetic_drop.take() {
        drop_files(app, paths, Some(pos));
    }
    let hovered_os: Vec<String> = ctx.input(|i| i.raw.hovered_files.iter().filter_map(|f| f.path.as_ref().map(|p| p.to_string_lossy().into_owned())).collect());
    let pointer = ctx.pointer_hover_pos().or_else(|| ctx.input(|i| i.pointer.latest_pos()));
    if !hovered_os.is_empty() {
        let pos = os_pointer(app, ctx).or(pointer);
        if let Some(p) = pos {
            hint(ctx, &app.tokens, p, &hovered_os[0], &describe(app, &hovered_os, p));
        }
        ctx.request_repaint();
    }
    let Some(drag) = app.assets.drag.clone() else { return };
    let released = ctx.input(|i| i.pointer.any_released());
    if let Some(p) = pointer {
        hint(ctx, &app.tokens, p, &drag.label, &describe(app, &drag.paths, p));
    }
    if released {
        app.assets.drag = None;
        drop_files(app, drag.paths, pointer);
    } else if !ctx.input(|i| i.pointer.any_down()) {
        app.assets.drag = None;
    }
}

/// Pointer position from the host during a drag from the Finder (logical points of the window).
pub fn os_pointer(app: &mut FilmcraftApp, ctx: &egui::Context) -> Option<Pos2> {
    let f = app.hooks.pointer_in_window.as_mut()?;
    let (x, y) = f()?;
    let z = ctx.zoom_factor().max(0.1);
    Some(egui::pos2(x / z, y / z))
}

fn hint(ctx: &egui::Context, t: &Tokens, p: Pos2, label: &str, what: &str) {
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("asset-drag-hint")));
    let name = Path::new(label).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| label.to_string());
    let text = format!("{name}\n{what}");
    let galley = painter.layout(text, Tokens::ui(11.5), Color32::WHITE, 420.0);
    let r = Rect::from_min_size(p + vec2(14.0, 10.0), galley.size() + vec2(14.0, 8.0));
    painter.rect_filled(r, 4.0, Color32::from_black_alpha(215));
    painter.rect_stroke(r, 4.0, egui::Stroke::new(1.0, t.accent), egui::StrokeKind::Inside);
    painter.galley(r.min + vec2(7.0, 4.0), galley, Color32::WHITE);
}

/// The preview's base frame for the current clip (re-rendered when the clip, the playhead or the
/// project changes) and the "after" picture of `lut`.
pub fn update_preview(app: &mut FilmcraftApp, ctx: &egui::Context, lut: Option<&str>) {
    let Some(clip) = filmcraft_engine::assets::preview_clip(&app.session) else {
        app.assets.preview.key = None;
        app.assets.preview.base = None;
        return;
    };
    let key = (clip.0, app.session.playhead().0, app.session.history.undo.len());
    if app.assets.preview.key != Some(key) {
        app.assets.preview.key = Some(key);
        app.assets.preview.base = filmcraft_engine::assets::look_base_frame(&app.session, clip, app.session.playhead(), 320);
        app.assets.preview.after_of = None;
        app.assets.preview.before = app.assets.preview.base.as_ref().map(|f| {
            let px = filmcraft_engine::assets::apply_look_rgba8(f, None);
            ctx.load_texture("assets-look-before", egui::ColorImage::from_rgba_unmultiplied([f.width, f.height], &px), egui::TextureOptions::LINEAR)
        });
    }
    // LUTs read in the background (the library can be on a slow disk)
    let done: Vec<(String, Result<filmcraft_color::Lut, String>)> = std::mem::take(&mut *app.assets.preview.loading.lock().unwrap_or_else(|e| e.into_inner()));
    for (p, r) in done {
        app.assets.preview.luts.insert(p, r.map(std::sync::Arc::new));
    }
    let Some(lut) = lut else { return };
    if app.assets.preview.after_of.as_deref() == Some(lut) {
        return;
    }
    match app.assets.preview.luts.get(lut).cloned() {
        Some(Ok(l)) => {
            if let Some(f) = &app.assets.preview.base {
                let px = filmcraft_engine::assets::apply_look_rgba8(f, Some(&l));
                app.assets.preview.after = Some(ctx.load_texture(
                    "assets-look-after",
                    egui::ColorImage::from_rgba_unmultiplied([f.width, f.height], &px),
                    egui::TextureOptions::LINEAR,
                ));
                app.assets.preview.after_of = Some(lut.to_string());
            }
        }
        Some(Err(_)) => {}
        None => {
            if app.assets.preview.asked.insert(lut.to_string()) {
                let (path, sink, ctx2) = (lut.to_string(), app.assets.preview.loading.clone(), ctx.clone());
                let _ = std::thread::Builder::new().name("assets-lut-read".into()).spawn(move || {
                    let r = std::fs::read_to_string(&path)
                        .map_err(|e| e.to_string())
                        .and_then(|t| filmcraft_color::Lut::parse(&t, filmcraft_color::LutFormat::from_path(&path)));
                    sink.lock().unwrap_or_else(|e| e.into_inner()).push((path, r));
                    ctx2.request_repaint();
                });
            }
        }
    }
}

/// Wait (tests): the LUT read in the background is parsed and its preview made.
pub fn preview_ready(app: &FilmcraftApp, lut: &str) -> bool {
    app.assets.preview.after_of.as_deref() == Some(lut)
}
