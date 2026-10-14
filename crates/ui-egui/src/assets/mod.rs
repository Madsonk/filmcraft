//! The user's own resources (assets-dnd): LUTs, sound effects, music and converted titles, dropped
//! on the timeline from the Finder or from Window ▸ Resources.
//!
//! - Where a drop lands is decided here (Timeline under the pointer: clip, track, time; anywhere
//!   else: the Project panel's bin), what it does is `assets.place` in the engine (LUT → Creative
//!   Look or LUT library, sound → a free audio track, picture/title → a free video track above).
//! - A `.mogrt` made in Premiere becomes an editable FilmCraft title (the engine converts it once,
//!   `graphics.template.importMogrt`); one made in After Effects is refused ("preview only"),
//!   unless the Titles index has a video rendered from it.
//! - Library folders and indexes live in `<data>/assets/` (`settings.json`, `index-<kind>.json`).
//!
//! macOS gives no pointer position during a drag from the Finder (winit 0.30 reports the files,
//! not where they are): the host's [`crate::HostHooks::pointer_in_window`] reads it from AppKit.

pub mod audition;
pub mod filters;
pub mod index;
pub mod library_state;
pub mod peaks;

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
    /// Folder of the waveform cache (ressources-v2): `/Volumes/Data/FilmCraft-cache/peaks` when
    /// that disk is there (the working disk), else `<data>/assets/peaks`.
    pub peaks_dir: String,
    /// Listen on hover: sound effects (from the start) and music (from 30 %).
    pub hover_sfx: bool,
    pub hover_music: bool,
    /// Titles shown as a grid of thumbnails (else a list).
    pub titles_grid: bool,
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
            peaks_dir: if Path::new("/Volumes/Data").is_dir() { "/Volumes/Data/FilmCraft-cache/peaks".into() } else { String::new() },
            hover_sfx: true,
            hover_music: true,
            titles_grid: true,
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
    /// The in-app audition (one voice).
    pub audition: audition::Audition,
    /// Waveform thumbnails (started with the panel).
    pub peaks: Option<peaks::PeakWorker>,
    /// Favourites and recents (`library-state.json`), loaded once.
    pub library: Option<library_state::LibraryState>,
    pub filters: filters::Filters,
    pub sort: filters::Sort,
    /// Selected row (path) of the current tab.
    pub selected: Option<String>,
    /// Row under the pointer: path, since when (seconds), last pointer position.
    pub hover: Option<(String, f64, Pos2)>,
    /// Bumped when favourites change (the filtered rows depend on them).
    pub library_rev: u64,
    /// Thumbnails of titles (path → texture), loaded in the background.
    pub thumbs: std::collections::HashMap<String, Option<egui::TextureHandle>>,
    pub thumbs_loading: std::sync::Arc<std::sync::Mutex<Vec<(String, Option<egui::ColorImage>)>>>,
    /// LUT row under the pointer (preview).
    pub hover_lut: Option<String>,
    pub preview: LookPreview,
    /// A drop made by the control channel (`ui.dropFiles`): paths and window point.
    pub synthetic_drop: Option<(Vec<String>, Pos2)>,
    /// Titles ▸ Convert all shown.
    pub convert_job: Option<std::sync::Arc<std::sync::Mutex<crate::panels::resources::ConvertJob>>>,
    /// Last drop's result, for the control channel and tests.
    pub last_drop: Option<Value>,
}

impl AssetsState {
    fn library_path(app_dir: Option<&Path>) -> Option<PathBuf> {
        app_dir.map(|d| d.join("assets").join("library-state.json"))
    }
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

/// Whether the Titles index says this `.mogrt` was made in After Effects (unknown = no).
fn title_is_after_effects(app: &FilmcraftApp, mogrt: &str) -> bool {
    app.assets
        .libs
        .get(&Kind::Titles)
        .is_some_and(|l| l.snapshot().entries.iter().any(|e| e.source.as_deref() == Some(mogrt) && e.family.as_deref() == Some("aefx")))
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
        (AssetKind::Mogrt, _, _) if title_is_after_effects(app, &paths[0]) => tl!("After Effects title: preview only, FilmCraft can't convert it").to_string(),
        (AssetKind::Mogrt, _, true) => tl!("Editable title on a free video track at this time").to_string(),
        (AssetKind::Mogrt, _, false) => tl!("Convert into an editable title (Graphics Templates)").to_string(),
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
        Err(e) if assets.iter().any(|p| AssetKind::of(p) == AssetKind::Mogrt) && e.to_string().contains("After Effects") => mogrt_message(),
        Err(e) if e.to_string().contains("not on a sound") => tl!("A LUT goes on a video clip, not on a sound").to_string(),
        Err(e) => e.to_string(),
    };
    if let Ok(v) = &r {
        note_used(app, v);
    }
    app.assets.last_drop = Some(match r {
        Ok(v) => json!({"params": q, "result": v}),
        Err(e) => json!({"params": q, "error": e.to_string()}),
    });
}

/// Favourites and recents, loaded on first use.
pub fn library_state(app: &mut FilmcraftApp) -> &mut library_state::LibraryState {
    if app.assets.library.is_none() {
        let st = AssetsState::library_path(data_dir(app).as_deref()).map(|p| library_state::LibraryState::load(&p)).unwrap_or_default();
        app.assets.library = Some(st);
    }
    app.assets.library.get_or_insert_with(Default::default)
}

pub fn save_library_state(app: &mut FilmcraftApp) {
    app.assets.library_rev += 1;
    if let (Some(p), Some(st)) = (AssetsState::library_path(data_dir(app).as_deref()), app.assets.library.as_ref()) {
        st.save(&p);
    }
}

/// The resources an `assets.place` answer placed become "recent".
pub fn note_used(app: &mut FilmcraftApp, answer: &Value) {
    let used: Vec<String> = answer["placed"]
        .as_array()
        .map(|a| a.iter().filter(|p| p["action"] != "imported").filter_map(|p| p["path"].as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    if used.is_empty() {
        return;
    }
    let st = library_state(app);
    for p in &used {
        st.used(p);
    }
    save_library_state(app);
}

/// The waveform store and its worker, started the first time a sound tab is shown.
pub fn peak_worker(app: &mut FilmcraftApp) -> Option<&peaks::PeakWorker> {
    if app.assets.peaks.is_none() {
        ensure_settings(app);
        let dir = if app.assets.settings.peaks_dir.is_empty() {
            data_dir(app).map(|d| d.join("assets").join("peaks")).unwrap_or_else(|| std::env::temp_dir().join("filmcraft-peaks"))
        } else {
            PathBuf::from(&app.assets.settings.peaks_dir)
        };
        let db = Some(PathBuf::from(&app.assets.settings.sound_db)).filter(|p| !p.as_os_str().is_empty());
        app.assets.peaks = Some(peaks::PeakWorker::start(dir, db, app.session.media.clone(), app.session.services.clone()));
    }
    app.assets.peaks.as_ref()
}

/// Whether the timeline or the Source monitor is playing (the audition and the waveform worker
/// give way to them).
pub fn other_playing(app: &FilmcraftApp) -> bool {
    app.playback.playing || app.source_playback.clock.playing
}

/// Audition `path` from `frac` of its length.
pub fn audition(app: &mut FilmcraftApp, path: &str, frac: f32) {
    if other_playing(app) {
        app.ui.status = tl!("Listening is paused while the timeline plays").to_string();
        return;
    }
    let (media, services) = (app.session.media.clone(), app.session.services.clone());
    let Some(out) = app.audio.as_deref_mut() else {
        app.ui.status = tl!("No audio output").to_string();
        return;
    };
    if let Err(e) = app.assets.audition.play(out, media, services, path, frac) {
        app.ui.status = tlf!("Could not play {path}", path = e);
    }
}

fn mogrt_message() -> String {
    tl!("This title was made in After Effects: FilmCraft can't convert it (preview only). Titles made in Premiere become editable titles.").to_string()
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
    let titles: Vec<&Value> = placed.iter().filter(|p| p["kind"] == "mogrt").collect();
    if titles.iter().any(|p| p["action"] == "converted") {
        parts.push(tlf!("{n} title(s) converted into Graphics Templates", n = titles.iter().filter(|p| p["action"] == "converted").count()));
    }
    let fonts: Vec<String> = titles
        .iter()
        .flat_map(|p| p["conversion"]["missingFonts"].as_array().cloned().unwrap_or_default())
        .filter_map(|f| f.as_str().map(str::to_string))
        .collect();
    if !fonts.is_empty() {
        parts.push(tlf!("missing fonts replaced by Inter: {fonts}", fonts = fonts.join(", ")));
    }
    if titles.iter().any(|p| p["conversion"]["lost"].as_array().is_some_and(|a| !a.is_empty())) {
        parts.push(tl!("some effects of the original title were left out (see the template's description)").to_string());
    }
    if refused.iter().any(|r| r["code"] == "afterEffects") {
        parts.push(mogrt_message());
    }
    let other = refused.iter().filter(|r| r["code"] != "afterEffects").count();
    if other > 0 {
        parts.push(tlf!(
            "{n} file(s) refused: {why}",
            n = other,
            why = refused.iter().find(|r| r["code"] != "afterEffects").and_then(|r| r["message"].as_str()).unwrap_or("")
        ));
    }
    parts.join(" · ")
}

/// Each frame, after the panels: the drag ghost of a resource and its drop, the hint of a drag
/// from the Finder, and the control channel's drop.
pub fn after_panels(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let busy = other_playing(app);
    app.assets.audition.tick(app.audio.as_mut(), busy);
    if let Some(w) = app.assets.peaks.as_ref() {
        w.set_paused(busy);
    }
    if app.assets.audition.state().playing {
        ctx.request_repaint_after(std::time::Duration::from_millis(30));
    }
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
                        .and_then(|t| filmcraft_color::Lut::parse(&t, filmcraft_color::LutFormat::from_path(&path)))
                        .map(|l| l.premiere_parity()); // the 64³ table the timeline renders
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
