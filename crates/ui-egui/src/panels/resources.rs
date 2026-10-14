//! Window ▸ Resources (assets-dnd): the user's own LUTs, sound effects, music and converted titles,
//! searched by name and dragged onto the timeline.
//!
//! - **LUTs**: hovering a LUT previews it on the current shot (selected video clip, else the clip
//!   under the playhead): before / after, computed on a small frame, nothing written to the
//!   project. Double-click = Creative Look of that shot. Drag = drop on any shot.
//! - **Sound effects / Music**: ▶ auditions the file (host player), double-click places it at the
//!   playhead on a free audio track, drag = place where dropped.
//! - **Titles**: the `.mogrt` converted by `mogrt_to_alpha` into videos with transparency; a title
//!   not converted yet shows its preview (a reference only) and says so.
//!
//! Indexes are built in the background ([`crate::assets::index`]); the panel never lists a folder
//! itself. Automation ids: `resources.tab.<luts|sfx|music|titles>`, `resources.search`,
//! `resources.folder`, `resources.refresh`, `resources.row.<n>` (n = row in the filtered list),
//! `resources.play.<n>`, `resources.preview`.

use egui::{Align2, Rect, Sense, pos2, vec2};
use serde_json::json;

use crate::FilmcraftApp;
use crate::assets::index::{self, Kind};
use crate::assets::{self as assets_mod, AssetDrag};
use crate::icons::{self, Icon};
use crate::theme::Tokens;

const ROW_H: f32 = 22.0;

fn tab_label(k: Kind) -> String {
    match k {
        Kind::Luts => tl!("LUTs").to_string(),
        Kind::Sfx => tl!("Sound Effects").to_string(),
        Kind::Music => tl!("Music").to_string(),
        Kind::Titles => tl!("Titles").to_string(),
    }
}

/// Shown name: a sound file's tags (`name [tag · tag · 1.4s].wav`) go to the tooltip.
fn display_name(name: &str) -> (&str, Option<&str>) {
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
    match stem.find(" [") {
        Some(i) => (&stem[..i], Some(&stem[i + 2..stem.len().saturating_sub(stem.ends_with(']') as usize)])),
        None => (stem, None),
    }
}

fn duration_text(d: f32) -> String {
    if d >= 60.0 { format!("{}:{:02}", (d / 60.0) as u32, (d % 60.0) as u32) } else { format!("{d:.1} s") }
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let ctx = ui.ctx().clone();
    assets_mod::ensure_settings(app);
    let tab = app.assets.tab.unwrap_or(Kind::Luts);
    // ---- tabs
    let mut x = rect.min.x + 6.0;
    let y = rect.min.y + 4.0;
    for k in Kind::ALL {
        let label = tab_label(k);
        let w = label.chars().count() as f32 * 6.6 + 18.0;
        let r = Rect::from_min_size(pos2(x, y), vec2(w, 22.0));
        let resp = ui.interact(r, egui::Id::new(("res-tab", k.id())), Sense::click());
        app.auto.add(&format!("resources.tab.{}", k.id()), r, &label);
        let active = k == tab;
        ui.painter().rect_filled(
            r,
            4.0,
            if active {
                t.row_selected
            } else if resp.hovered() {
                t.hover
            } else {
                t.tab_bg
            },
        );
        ui.painter().text(r.center(), Align2::CENTER_CENTER, &label, Tokens::ui(11.5), if active { t.tab_text_active } else { t.tab_text });
        if resp.clicked() {
            app.assets.tab = Some(k);
            app.assets.filtered = None;
        }
        x += w + 4.0;
    }
    // folder + refresh at the right
    let fr = Rect::from_min_size(pos2(rect.max.x - 56.0, y), vec2(24.0, 22.0));
    let rr = Rect::from_min_size(pos2(rect.max.x - 28.0, y), vec2(24.0, 22.0));
    let root = app.assets.settings.root(tab).to_string();
    let folder_tip = if root.is_empty() { tl!("Choose the folder…").to_string() } else { tlf!("Folder: {path} (click to change)", path = root) };
    let f = ui.interact(fr, egui::Id::new("res-folder"), Sense::click()).on_hover_text(&folder_tip);
    app.auto.add("resources.folder", fr, &folder_tip);
    icons::paint(ui.painter(), fr.shrink(5.0), Icon::Folder, if f.hovered() { t.icon_active } else { t.icon });
    if f.clicked()
        && let Some(pick) = app.hooks.pick_folder.as_mut()
        && let Some(dir) = pick()
    {
        app.assets.settings.set_root(tab, dir);
        assets_mod::save_settings(app);
        app.assets.filtered = None;
    }
    let refresh_tip = tl!("Update the index (only changed folders are read again)");
    let rf = ui.interact(rr, egui::Id::new("res-refresh"), Sense::click()).on_hover_text(refresh_tip);
    app.auto.add("resources.refresh", rr, refresh_tip);
    icons::paint(ui.painter(), rr.shrink(5.0), Icon::Reset, if rf.hovered() { t.icon_active } else { t.icon });
    if rf.clicked() {
        assets_mod::refresh(app, tab);
    }
    // ---- search
    let sr = Rect::from_min_size(pos2(rect.min.x + 6.0, y + 28.0), vec2((rect.width() - 12.0).max(40.0), 22.0));
    let mut q = app.assets.query.clone();
    let resp = ui.put(
        sr,
        egui::TextEdit::singleline(&mut q).id(egui::Id::new("res-search")).hint_text(tl!("Search by name or folder")).font(egui::FontId::proportional(11.5)),
    );
    app.auto.add("resources.search", sr, "Search");
    if resp.changed() {
        app.assets.query = q;
    }
    // ---- index state
    let snap = assets_mod::library(app, tab).map(|l| l.snapshot());
    let mut top = sr.max.y + 4.0;
    let status_r = Rect::from_min_size(pos2(rect.min.x + 8.0, top), vec2(rect.width() - 16.0, 16.0));
    top += 18.0;
    let Some(snap) = snap else {
        let msg = tl!("No folder chosen: click the folder icon.");
        ui.painter().text(status_r.left_center(), Align2::LEFT_CENTER, msg, Tokens::ui(11.0), t.text_dim);
        return;
    };
    let key = (tab, snap.rev, app.assets.query.clone());
    if app.assets.filtered.as_ref().is_none_or(|(k, _)| *k != key) {
        let rows = index::search(&snap.entries, &app.assets.query);
        app.assets.filtered = Some((key, rows));
    }
    let rows = app.assets.filtered.as_ref().map(|(_, r)| r.clone()).unwrap_or_default();
    let state = if let Some(e) = &snap.error {
        tlf!("Index error: {error}", error = e)
    } else if snap.running {
        tlf!("{n} of {total} · indexing in the background ({dirs} folders read)…", n = rows.len(), total = snap.entries.len(), dirs = snap.dirs_seen)
    } else {
        tlf!("{n} of {total}", n = rows.len(), total = snap.entries.len())
    };
    ui.painter().text(status_r.left_center(), Align2::LEFT_CENTER, &state, Tokens::ui(10.5), t.text_dim);
    if snap.running {
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
    // ---- LUT preview strip
    if tab == Kind::Luts {
        let pr = Rect::from_min_size(pos2(rect.min.x + 6.0, top), vec2(rect.width() - 12.0, 96.0));
        top = pr.max.y + 4.0;
        let hovered = app.assets.hover_lut.clone();
        assets_mod::update_preview(app, &ctx, hovered.as_deref());
        app.auto.add("resources.preview", pr, "LUT preview");
        ui.painter().rect_filled(pr, 4.0, t.field_bg);
        match (&app.assets.preview.before, &app.assets.preview.base) {
            (Some(before), Some(base)) => {
                let aspect = base.width as f32 / base.height.max(1) as f32;
                let h = pr.height() - 16.0;
                let w = (h * aspect).min(pr.width() / 2.0 - 6.0);
                let a = Rect::from_min_size(pr.min + vec2(2.0, 2.0), vec2(w, w / aspect));
                let b = Rect::from_min_size(a.right_top() + vec2(6.0, 0.0), a.size());
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                ui.painter().image(before.id(), a, uv, egui::Color32::WHITE);
                let lbl_y = a.max.y + 7.0;
                ui.painter().text(pos2(a.center().x, lbl_y), Align2::CENTER_CENTER, tl!("Before"), Tokens::ui(10.0), t.text_dim);
                let after = app.assets.preview.after.as_ref().filter(|_| hovered.is_some() && app.assets.preview.after_of == hovered);
                if let Some(after) = after {
                    ui.painter().image(after.id(), b, uv, egui::Color32::WHITE);
                    let name = hovered
                        .as_deref()
                        .map(|p| display_name(std::path::Path::new(p).file_name().and_then(|s| s.to_str()).unwrap_or(p)).0.to_string())
                        .unwrap_or_default();
                    ui.painter().text(pos2(b.center().x, lbl_y), Align2::CENTER_CENTER, name, Tokens::ui(10.0), t.text);
                } else {
                    ui.painter().rect_stroke(b, 2.0, egui::Stroke::new(1.0, t.separator), egui::StrokeKind::Inside);
                    let msg = if hovered.is_some() { tl!("Reading the LUT…") } else { tl!("Hover a LUT to preview it") };
                    // wrapped inside the empty frame (the French line is wider than the frame)
                    let galley = ui.painter().layout(msg.to_string(), Tokens::ui(10.5), t.text_dim, (b.width() - 12.0).max(20.0));
                    let g = galley.size();
                    ui.painter().galley(pos2(b.center().x - g.x / 2.0, b.center().y - g.y / 2.0), galley, t.text_dim);
                }
            }
            _ => {
                let msg = tl!("Select a video clip (or put the playhead on one) to preview LUTs on it.");
                ui.painter().text(pr.center(), Align2::CENTER_CENTER, msg, Tokens::ui(11.0), t.text_dim);
            }
        }
    }
    if tab == Kind::Titles {
        let note =
            tl!("Titles = .mogrt converted to video with transparency (mogrt_to_alpha). “preview only” = not converted yet: a reference, not the title.");
        let nr = Rect::from_min_size(pos2(rect.min.x + 8.0, top), vec2(rect.width() - 16.0, 28.0));
        let galley = ui.painter().layout(note.to_string(), Tokens::ui(10.0), t.text_dim, nr.width());
        top += galley.size().y + 4.0;
        ui.painter().galley(nr.min, galley, t.text_dim);
    }
    // ---- rows
    let list = Rect::from_min_max(pos2(rect.min.x, top), rect.max);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list).id_salt(("res-list", tab.id())));
    let mut hover_lut = None;
    let mut action: Option<(usize, &'static str)> = None;
    egui::ScrollArea::vertical().id_salt(("res-scroll", tab.id())).auto_shrink([false, false]).show_rows(&mut child, ROW_H, rows.len(), |ui, range| {
        for n in range {
            let e = &snap.entries[rows[n]];
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
            let resp = ui.interact(r, egui::Id::new(("res-row", tab.id(), n)), Sense::click_and_drag());
            app.auto.add(&format!("resources.row.{n}"), r, &e.name);
            if n % 2 == 1 {
                ui.painter().rect_filled(r, 0.0, t.row_alt);
            }
            if resp.hovered() {
                ui.painter().rect_filled(r, 0.0, t.hover);
                if tab == Kind::Luts {
                    hover_lut = Some(e.path.clone());
                }
            }
            let mut x = r.min.x + 6.0;
            if matches!(tab, Kind::Sfx | Kind::Music) {
                let pr = Rect::from_min_size(pos2(x, r.min.y + 3.0), vec2(16.0, 16.0));
                let play = ui.interact(pr, egui::Id::new(("res-play", tab.id(), n)), Sense::click());
                app.auto.add(&format!("resources.play.{n}"), pr, "Play");
                let on = app.assets.playing.as_deref() == Some(e.path.as_str());
                icons::paint(ui.painter(), pr.shrink(2.0), if on { Icon::Pause } else { Icon::Play }, if play.hovered() || on { t.accent } else { t.icon });
                if play.clicked() {
                    action = Some((n, "play"));
                }
                x += 22.0;
            }
            let (name, tags) = display_name(&e.name);
            let right = match (tab, e.dur) {
                (Kind::Titles, _) if !e.ready => tl!("preview only").to_string(),
                (_, Some(d)) => duration_text(d),
                _ => String::new(),
            };
            let rw = right.chars().count() as f32 * 6.0 + 8.0;
            let name_w = (r.max.x - x - rw - 6.0).max(20.0);
            let shown = if tab == Kind::Titles && !e.dir.is_empty() { format!("{name} — {}", e.dir) } else { name.to_string() };
            // long names (the library prefixes them with their folders) keep their end: the
            // part that tells the files apart (".. LogC to ACEScct")
            let shown = elide_middle(ui, &shown, name_w);
            let galley = ui.painter().layout_no_wrap(shown, Tokens::ui(11.5), t.text);
            let clip = Rect::from_min_size(pos2(x, r.min.y), vec2(name_w, ROW_H));
            ui.painter().with_clip_rect(clip).galley(pos2(x, r.center().y - galley.size().y / 2.0), galley, t.text);
            ui.painter().text(pos2(r.max.x - 6.0, r.center().y), Align2::RIGHT_CENTER, &right, Tokens::ui(10.5), t.text_dim);
            // the full name first (the row may be elided), then the folder and the tags
            let head = if e.dir.is_empty() { name.to_string() } else { format!("{name}\n{}", e.dir) };
            let resp = match (tags, &e.family) {
                (Some(tg), _) => resp.on_hover_text(format!("{head}\n{tg}")),
                (None, Some(f)) => resp.on_hover_text(format!("{head}\n{f}")),
                _ => resp.on_hover_text(head),
            };
            if resp.double_clicked() {
                action = Some((n, "apply"));
            } else if resp.drag_started() {
                action = Some((n, "drag"));
            }
        }
    });
    if hover_lut.is_some() || !ctx.input(|i| i.pointer.any_down()) {
        app.assets.hover_lut = hover_lut;
    }
    if let Some((n, what)) = action {
        let e = snap.entries[rows[n]].clone();
        match what {
            "play" => toggle_play(app, &e.path),
            "drag" => {
                if tab == Kind::Titles && !e.ready {
                    app.ui.status = tl!("Not converted yet: the preview is dropped as a reference, not the title itself").to_string();
                }
                app.assets.drag = Some(AssetDrag { paths: vec![e.path.clone()], label: e.name.clone() });
            }
            _ => apply(app, tab, &e.path),
        }
    }
}

/// Double-click: a LUT → Creative Look of the preview shot; a sound or a title → at the playhead.
fn apply(app: &mut FilmcraftApp, tab: Kind, path: &str) {
    let mut q = json!({"paths": [path]});
    if tab == Kind::Luts {
        match filmcraft_engine::assets::preview_clip(&app.session) {
            Some(c) => q["clip"] = json!(c.0),
            None => {
                app.ui.status = tl!("Select a video clip first: the LUT becomes its Creative Look.").to_string();
                return;
            }
        }
    } else if app.session.active_sequence().is_some() {
        q["time"] = json!(app.session.playhead().0);
    }
    let r = app.session.execute("assets.place", q.clone());
    app.ui.status = match &r {
        Ok(v) => v["summary"].as_str().unwrap_or_default().to_string(),
        Err(e) => e.to_string(),
    };
    app.assets.last_drop = Some(match r {
        Ok(v) => json!({"params": q, "result": v}),
        Err(e) => json!({"params": q, "error": e.to_string()}),
    });
}

/// ▶: audition through the host (stops the previous sound); again = stop.
pub fn toggle_play(app: &mut FilmcraftApp, path: &str) {
    let stop = app.assets.playing.as_deref() == Some(path);
    let Some(play) = app.hooks.preview_audio.as_mut() else {
        app.ui.status = tl!("Listening is not available in this build").to_string();
        return;
    };
    if stop {
        play(None);
        app.assets.playing = None;
    } else if play(Some(path)) {
        app.assets.playing = Some(path.to_string());
    } else {
        app.ui.status = tlf!("Could not play {path}", path = path);
        app.assets.playing = None;
    }
}

/// `text` shortened in the middle ("start … end") to fit `width` at the row font.
fn elide_middle(ui: &egui::Ui, text: &str, width: f32) -> String {
    let w = |s: &str| ui.painter().layout_no_wrap(s.to_string(), Tokens::ui(11.5), egui::Color32::WHITE).size().x;
    let full = w(text);
    if full <= width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let cut = |keep: usize| -> String {
        let head = keep * 2 / 5;
        let tail = keep - head;
        chars[..head].iter().chain(['…'].iter()).chain(chars[chars.len() - tail..].iter()).collect()
    };
    // start from the proportional guess, then trim a few characters until it fits
    let mut keep = ((chars.len() as f32 * width / full) as usize).min(chars.len().saturating_sub(1));
    while keep > 4 {
        let s = cut(keep);
        if w(&s) <= width {
            return s;
        }
        keep -= 2;
    }
    cut(keep.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sound_tags_go_to_the_tooltip() {
        let (n, t) = display_name("SFX · Hits · boom [impact · court · 1.4s].wav");
        assert_eq!(n, "SFX · Hits · boom");
        assert_eq!(t, Some("impact · court · 1.4s"));
        assert_eq!(display_name("AG X AL_Drive.cube"), ("AG X AL_Drive", None));
        assert_eq!(duration_text(83.0), "1:23");
        assert_eq!(duration_text(1.44), "1.4 s");
    }
}
