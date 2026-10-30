//! Window ▸ Resources (assets-dnd, ressources-v2): the user's own LUTs, sound effects, music and
//! titles, searched, filtered, auditioned and dragged onto the timeline.
//!
//! - **LUTs**: hovering a LUT previews it on the current shot (selected video clip, else the clip
//!   under the playhead): before / after, computed on a small frame, nothing written to the
//!   project. Double-click = Creative Look of that shot. Drag = drop on any shot.
//! - **Sound effects / Music** (ressources-v2): every row shows the sound's **waveform**
//!   (computed once, kept on disk: [`crate::assets::peaks`]); **listening on hover** (after
//!   150 ms still; sound effects from the start, music from 30 %; the speaker button turns it off)
//!   plays **inside the app** ([`crate::assets::audition`]) with a play head on the waveform;
//!   moving along the waveform scrubs, a click there plays from that point; ▶ / Space play and
//!   pause. Search by relevance, filters from the sound index (family, duration, tempo, key,
//!   loops), favourites (★, `F`) and recently used, sort. Double-click / Enter = at the playhead on
//!   a free audio track, drag = where dropped.
//! - **Titles**: list or grid of thumbnails. A title made in Premiere becomes an editable FilmCraft
//!   title when dropped; "Convert all shown" converts the listed ones in the background. One made
//!   in After Effects is "preview only".
//!
//! Keyboard (panel focused): ↑ ↓ select (and listen when listening on hover is on), Space play /
//! pause, Enter apply, Esc stop / clear the search, `F` favourite, `/` search.
//!
//! Indexes and waveforms are built in the background; the panel never reads a file itself.
//! Automation ids: `resources.tab.<luts|sfx|music|titles>`, `resources.search`,
//! `resources.folder`, `resources.refresh`, `resources.favorites`, `resources.recents`,
//! `resources.hoverPlay`, `resources.view`, `resources.filter.<family|duration|bpm|key|loops|sort>`,
//! `resources.row.<n>` (n = row in the filtered list), `resources.play.<n>`, `resources.star.<n>`,
//! `resources.wave.<n>`, `resources.preview`.

use egui::{Align2, Color32, Rect, Sense, Stroke, pos2, vec2};
use serde_json::json;

use crate::FilmcraftApp;
use crate::assets::filters::{self, DURATIONS, Sort};
use crate::assets::index::{self, Kind};
use crate::assets::{self as assets_mod, AssetDrag};
use crate::icons::{self, Icon};
use crate::theme::Tokens;

const ROW_H: f32 = 22.0;
/// Sound rows are taller: the waveform needs room.
const SOUND_ROW_H: f32 = 30.0;
/// Hover this long, still, before listening.
const HOVER_DELAY: f64 = 0.15;
const TILE_W: f32 = 150.0;
const TILE_H: f32 = 112.0;

fn tab_label(k: Kind) -> String {
    match k {
        Kind::Luts => tl!("LUTs").to_string(),
        Kind::Sfx => tl!("Sound Effects").to_string(),
        Kind::Music => tl!("Music").to_string(),
        Kind::Titles => tl!("Titles").to_string(),
    }
}

fn sort_label(s: Sort) -> String {
    match s {
        Sort::Relevance => tl!("Relevance").to_string(),
        Sort::Name => tl!("Name").to_string(),
        Sort::Duration => tl!("Duration").to_string(),
        Sort::Tempo => tl!("Tempo").to_string(),
    }
}

fn duration_label(i: usize) -> String {
    match i {
        0 => tl!("< 0.5 s").to_string(),
        1 => tl!("0.5–2 s").to_string(),
        2 => tl!("2–10 s").to_string(),
        _ => tl!("> 10 s").to_string(),
    }
}

/// Families as the sound index writes them (French), shown in the interface language.
fn family_label(f: &str) -> String {
    match f {
        "one-shot / impact" => tl!("one-shot / impact").to_string(),
        "ambiance / texture" => tl!("ambience / texture").to_string(),
        "ambiance dense (evenements aigus)" => tl!("dense ambience").to_string(),
        "nappe / bruit continu" => tl!("pad / continuous noise").to_string(),
        "tonal non rythmique (nappe, drone)" => tl!("tonal, no rhythm (pad, drone)").to_string(),
        "musique" => tl!("music").to_string(),
        o => o.to_string(),
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

/// Where listening starts: sound effects at the start, music at 30 % (intros are often empty).
fn start_frac(tab: Kind) -> f32 {
    if tab == Kind::Music { 0.3 } else { 0.0 }
}

fn hover_on(app: &FilmcraftApp, tab: Kind) -> bool {
    match tab {
        Kind::Sfx => app.assets.settings.hover_sfx,
        Kind::Music => app.assets.settings.hover_music,
        _ => false,
    }
}

/// Draw a waveform thumbnail (min/max bars at the real pixel width); the played part in full
/// accent; a 2 px play head; a flat line while the peaks are unknown.
pub fn draw_wave(p: &egui::Painter, r: Rect, peaks: Option<&crate::assets::peaks::Peaks>, progress: Option<f32>, t: &Tokens) {
    let mid = r.center().y;
    let half = r.height() / 2.0 - 1.0;
    let dim = t.accent.gamma_multiply(0.7);
    match peaks {
        Some(pk) if !pk.cols.is_empty() => {
            let w = r.width().max(1.0) as usize;
            for x in 0..w {
                let (a, b) = (x as f32 / w as f32, (x + 1) as f32 / w as f32);
                let (lo, hi) = pk.range(a, b);
                let played = progress.is_some_and(|pr| a < pr);
                let c = if played { t.accent } else { dim };
                let x0 = r.min.x + x as f32 + 0.5;
                let (y0, y1) = (mid - hi.max(0.02) * half, mid - lo.min(-0.02) * half);
                p.line_segment([pos2(x0, y0), pos2(x0, y1)], Stroke::new(1.0, c));
            }
        }
        _ => {
            p.line_segment([pos2(r.min.x, mid), pos2(r.max.x, mid)], Stroke::new(1.0, t.text_faint));
        }
    }
    if let Some(pr) = progress {
        let x = r.min.x + pr.clamp(0.0, 1.0) * r.width();
        p.line_segment([pos2(x, r.min.y), pos2(x, r.max.y)], Stroke::new(2.0, Color32::WHITE));
    }
}

fn toggle_button(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: Rect, id: &str, label: &str, tip: &str, on: bool) -> bool {
    let t = app.tokens;
    let resp = ui.interact(r, egui::Id::unique(("res-btn", id)), Sense::click()).on_hover_text(tip);
    app.auto.add(&format!("resources.{id}"), r, tip);
    ui.painter().rect_filled(
        r,
        4.0,
        if on {
            t.accent
        } else if resp.hovered() {
            t.hover
        } else {
            t.tab_bg
        },
    );
    ui.painter().text(r.center(), Align2::CENTER_CENTER, label, Tokens::ui(12.0), if on { Color32::WHITE } else { t.tab_text });
    resp.clicked()
}

#[allow(clippy::cognitive_complexity)] // one panel's immediate-mode layout: list, tabs, filters, drag, audition
pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let ctx = ui.ctx().clone();
    assets_mod::ensure_settings(app);
    let tab = app.assets.tab.unwrap_or(Kind::Luts);
    let sound = matches!(tab, Kind::Sfx | Kind::Music);
    // ---- tabs
    let mut x = rect.min.x + 6.0;
    let y = rect.min.y + 4.0;
    for k in Kind::ALL {
        let label = tab_label(k);
        let w = label.chars().count() as f32 * 6.6 + 18.0;
        let r = Rect::from_min_size(pos2(x, y), vec2(w, 22.0));
        let resp = ui.interact(r, egui::Id::unique(("res-tab", k.id())), Sense::click());
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
            app.assets.selected = None;
        }
        x += w + 4.0;
    }
    // folder + refresh at the right
    let fr = Rect::from_min_size(pos2(rect.max.x - 56.0, y), vec2(24.0, 22.0));
    let rr = Rect::from_min_size(pos2(rect.max.x - 28.0, y), vec2(24.0, 22.0));
    let root = app.assets.settings.root(tab).to_string();
    let folder_tip = if root.is_empty() { tl!("Choose the folder…").to_string() } else { tlf!("Folder: {path} (click to change)", path = root) };
    let f = ui.interact(fr, egui::Id::unique("res-folder"), Sense::click()).on_hover_text(&folder_tip);
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
    let rf = ui.interact(rr, egui::Id::unique("res-refresh"), Sense::click()).on_hover_text(refresh_tip);
    app.auto.add("resources.refresh", rr, refresh_tip);
    icons::paint(ui.painter(), rr.shrink(5.0), Icon::Reset, if rf.hovered() { t.icon_active } else { t.icon });
    if rf.clicked() {
        assets_mod::refresh(app, tab);
    }
    // ---- search + toggles
    let buttons = if sound || tab == Kind::Titles { 3 } else { 2 };
    let bw = 26.0;
    let sw = (rect.width() - 12.0 - buttons as f32 * (bw + 4.0)).max(40.0);
    let sr = Rect::from_min_size(pos2(rect.min.x + 6.0, y + 28.0), vec2(sw, 22.0));
    let mut q = app.assets.query.clone();
    let resp = ui.put(
        sr,
        egui::TextEdit::singleline(&mut q).id(egui::Id::unique("res-search")).hint_text(tl!("Search by name or folder")).font(egui::FontId::proportional(11.5)),
    );
    app.auto.add("resources.search", sr, "Search");
    if resp.changed() {
        app.assets.query = q;
    }
    let mut bx = sr.max.x + 4.0;
    let mut next = || {
        let r = Rect::from_min_size(pos2(bx, sr.min.y), vec2(bw, 22.0));
        bx += bw + 4.0;
        r
    };
    let (r1, r2) = (next(), next());
    if toggle_button(app, ui, r1, "favorites", "★", tl!("Favourites only"), app.assets.filters.favorites_only) {
        app.assets.filters.favorites_only = !app.assets.filters.favorites_only;
    }
    if toggle_button(app, ui, r2, "recents", "↺", tl!("Recently used"), app.assets.filters.recents_only) {
        app.assets.filters.recents_only = !app.assets.filters.recents_only;
    }
    if sound {
        let r3 = next();
        let on = hover_on(app, tab);
        if toggle_button(app, ui, r3, "hoverPlay", "♪", tl!("Listen on hover"), on) {
            match tab {
                Kind::Music => app.assets.settings.hover_music = !on,
                _ => app.assets.settings.hover_sfx = !on,
            }
            assets_mod::save_settings(app);
            app.assets.audition.stop();
        }
    } else if tab == Kind::Titles {
        let r3 = next();
        let grid = app.assets.settings.titles_grid;
        if toggle_button(app, ui, r3, "view", if grid { "▦" } else { "☰" }, tl!("Grid / list"), false) {
            app.assets.settings.titles_grid = !grid;
            assets_mod::save_settings(app);
        }
    }
    let mut top = sr.max.y + 4.0;
    // ---- index state
    let snap = assets_mod::library(app, tab).map(|l| l.snapshot());
    let Some(snap) = snap else {
        let status_r = Rect::from_min_size(pos2(rect.min.x + 8.0, top), vec2(rect.width() - 16.0, 16.0));
        let msg = tl!("No folder chosen: click the folder icon.");
        ui.painter().text(status_r.left_center(), Align2::LEFT_CENTER, msg, Tokens::ui(11.0), t.text_dim);
        return;
    };
    // ---- filters (sounds)
    if sound {
        top = filter_bar(app, ui, &snap, tab, rect, top);
    }
    let status_r = Rect::from_min_size(pos2(rect.min.x + 8.0, top), vec2(rect.width() - 16.0, 16.0));
    top += 18.0;
    let lib_rev = app.assets.library_rev;
    let fkey = format!("{}|{:?}|{:?}|{lib_rev}", app.assets.query, app.assets.filters, app.assets.sort);
    let key = (tab, snap.rev, fkey);
    if app.assets.filtered.as_ref().is_none_or(|(k, _)| *k != key) {
        let st = assets_mod::library_state(app).clone();
        let rows = filters::select(&snap.entries, &app.assets.query, &app.assets.filters, app.assets.sort, &st.favorites, &st.recents);
        // a new search or filter shows its first rows (not the old scroll position); a new
        // index publication (same query) keeps the position
        let same_query = app.assets.filtered.as_ref().is_some_and(|(k, _)| k.0 == key.0 && k.2 == key.2);
        if !same_query {
            ctx.data_mut(|d| d.insert_temp(egui::Id::unique("res-scroll-top"), true));
        }
        app.assets.filtered = Some((key, rows));
    }
    let rows = app.assets.filtered.as_ref().map(|(_, r)| r.clone()).unwrap_or_default();
    let mut state = if let Some(e) = &snap.error {
        tlf!("Index error: {error}", error = e)
    } else if snap.running {
        tlf!("{n} of {total} · indexing in the background ({dirs} folders read)…", n = rows.len(), total = snap.entries.len(), dirs = snap.dirs_seen)
    } else {
        tlf!("{n} of {total}", n = rows.len(), total = snap.entries.len())
    };
    let mut previews = None;
    if sound && let Some(w) = assets_mod::peak_worker(app) {
        w.set_repaint(&ctx);
        let st = w.stats();
        // light previews of this tab: made / sounds in the library
        if let Some(store) = w.previews() {
            let kind = if tab == Kind::Music { assets_mod::peaks::PreviewKind::Music } else { assets_mod::peaks::PreviewKind::Sfx };
            let (made, bytes) = store.stats(kind);
            state.push_str(&format!(" · {}", tlf!("previews: {made}/{total}", made = made, total = snap.entries.len())));
            previews = Some((store, bytes));
        }
        if !st.loaded {
            state.push_str(&format!(" · {}", tl!("loading waveforms…")));
        } else if st.queued > 0 {
            state.push_str(&format!(" · {}", tlf!("waveforms: {known} known, {queued} to read", known = st.known, queued = st.queued)));
        } else {
            state.push_str(&format!(" · {}", tlf!("waveforms: {known} known", known = st.known)));
        }
        // the background sweep: every file of the library, once per session
        if snap.complete {
            w.want_background(snap.entries.iter().map(|e| e.path.clone()));
        }
    }
    ui.painter().with_clip_rect(status_r).text(status_r.left_center(), Align2::LEFT_CENTER, &state, Tokens::ui(10.5), t.text_dim);
    if let Some((store, bytes)) = previews {
        // right-click on the status line: empty the preview cache (the waveforms are kept)
        let r = ui
            .interact(status_r, egui::Id::unique("res-status"), Sense::click())
            .on_hover_text(format!("{state}\n{}", tl!("Right-click: empty the preview cache")));
        r.context_menu(|ui| {
            let label = tlf!("Empty the preview cache ({mb} MB)", mb = format!("{:.1}", bytes as f64 / 1e6));
            if ui.button(label).clicked() {
                clear_previews(app, store.clone());
                ui.close();
            }
        });
    }
    if snap.running {
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
    // ---- LUT preview strip
    if tab == Kind::Luts {
        top = lut_preview(app, ui, &ctx, rect, top);
    }
    if tab == Kind::Titles {
        let note = tl!(
            "Titles made in Premiere become editable titles (text, colours, fonts) when dropped. After Effects titles are “preview only”: a reference, not the title."
        );
        // wrapped to what is visible of the panel (a narrow panel shows every word, nothing cut)
        let vis = rect.intersect(ui.clip_rect());
        let nr = Rect::from_min_size(pos2(vis.min.x + 8.0, top), vec2((vis.width() - 16.0).max(60.0), 28.0));
        let galley = ui.painter().layout(note.to_string(), Tokens::ui(10.5), t.text_dim, nr.width());
        app.auto.add("resources.titlesNote", Rect::from_min_size(nr.min, galley.size()), note);
        top += galley.size().y + 4.0;
        ui.painter().galley(nr.min, galley, t.text_dim);
        top = convert_all_row(app, ui, &snap, &rows, rect, top);
    }
    let list = Rect::from_min_max(pos2(rect.min.x, top), rect.max);
    if tab == Kind::Titles && app.assets.settings.titles_grid {
        titles_grid(app, ui, &ctx, &snap, &rows, list);
    } else {
        rows_list(app, ui, &ctx, &snap, &rows, list, tab);
    }
}

/// Family, duration, tempo, key, loops and sort (sound tabs). Returns the new top.
fn filter_bar(app: &mut FilmcraftApp, ui: &mut egui::Ui, snap: &index::Snapshot, tab: Kind, rect: Rect, top: f32) -> f32 {
    let r = Rect::from_min_size(pos2(rect.min.x + 6.0, top), vec2(rect.width() - 12.0, 22.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(egui::Layout::left_to_right(egui::Align::Center)));
    child.spacing_mut().item_spacing.x = 4.0;
    let f = &mut app.assets.filters;
    let before = f.clone();
    let any = tl!("All");
    let fam_text = f.family.as_deref().map(family_label).unwrap_or_else(|| tlf!("Family: {v}", v = any));
    let fam = craft_ui::ComboBox::from_id_salt(("res-fam", tab.id())).selected_text(fam_text).width(110.0).show_ui(&mut child, |ui| {
        ui.selectable_value(&mut f.family, None, any);
        for fam in filters::families(&snap.entries) {
            let l = family_label(&fam);
            ui.selectable_value(&mut f.family, Some(fam), l);
        }
    });
    let dur_text = f.duration.map(duration_label).unwrap_or_else(|| tlf!("Length: {v}", v = any));
    let dur = craft_ui::ComboBox::from_id_salt(("res-dur", tab.id())).selected_text(dur_text).width(80.0).show_ui(&mut child, |ui| {
        ui.selectable_value(&mut f.duration, None, any);
        for i in 0..DURATIONS.len() {
            ui.selectable_value(&mut f.duration, Some(i), duration_label(i));
        }
    });
    let mut rects = vec![("family", fam.response.rect), ("duration", dur.response.rect)];
    if tab == Kind::Music {
        let mut lo = f.bpm_min.unwrap_or(0.0);
        let mut hi = f.bpm_max.unwrap_or(0.0);
        child.label(egui::RichText::new(tl!("BPM")).size(10.5));
        let a = child
            .add(egui::DragValue::new(&mut lo).range(0.0..=300.0).speed(1.0).custom_formatter(|v, _| if v <= 0.0 { "–".into() } else { format!("{v:.0}") }));
        let b = child
            .add(egui::DragValue::new(&mut hi).range(0.0..=300.0).speed(1.0).custom_formatter(|v, _| if v <= 0.0 { "–".into() } else { format!("{v:.0}") }));
        f.bpm_min = (lo > 0.0).then_some(lo);
        f.bpm_max = (hi > 0.0).then_some(hi);
        rects.push(("bpm", a.rect.union(b.rect)));
        let key_text = f.key.clone().unwrap_or_else(|| tlf!("Key: {v}", v = any));
        let key = craft_ui::ComboBox::from_id_salt(("res-key", tab.id())).selected_text(key_text).width(90.0).show_ui(&mut child, |ui| {
            ui.selectable_value(&mut f.key, None, any);
            for k in filters::keys(&snap.entries).into_iter().take(24) {
                let l = k.clone();
                ui.selectable_value(&mut f.key, Some(k), l);
            }
        });
        rects.push(("key", key.response.on_hover_text(tl!("Also finds the relative major / minor")).rect));
    }
    let lp = child.checkbox(&mut f.loops_only, tl!("Loops"));
    rects.push(("loops", lp.rect));
    let sort = &mut app.assets.sort;
    let st = craft_ui::ComboBox::from_id_salt(("res-sort", tab.id())).selected_text(tlf!("Sort: {v}", v = sort_label(*sort))).width(100.0).show_ui(
        &mut child,
        |ui| {
            for s in Sort::ALL {
                ui.selectable_value(sort, s, sort_label(s));
            }
        },
    );
    rects.push(("sort", st.response.rect));
    for (id, r) in rects {
        app.auto.add(&format!("resources.filter.{id}"), r, id);
    }
    if app.assets.filters != before {
        app.assets.filtered = None;
    }
    top + 26.0
}

/// List rows (all tabs; sound rows carry a waveform). Hover listening, scrub, keyboard selection.
#[allow(clippy::too_many_arguments)]
fn rows_list(app: &mut FilmcraftApp, ui: &mut egui::Ui, ctx: &egui::Context, snap: &index::Snapshot, rows: &[usize], list: Rect, tab: Kind) {
    let t = app.tokens;
    let sound = matches!(tab, Kind::Sfx | Kind::Music);
    let row_h = if sound { SOUND_ROW_H } else { ROW_H };
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list).id_salt(("res-list", tab.id())));
    let mut hover_lut = None;
    let mut hovered_row: Option<String> = None;
    let mut action: Option<(usize, &'static str, f32)> = None;
    let now = ctx.input(|i| i.time);
    let pointer = ctx.pointer_hover_pos();
    let audition = app.assets.audition.state();
    let favorites = assets_mod::library_state(app).favorites.clone();
    let mut visible_paths = Vec::new();
    let mut scroll = egui::ScrollArea::vertical().id_salt(("res-scroll", tab.id())).auto_shrink([false, false]);
    if let Some(sel) = app.assets.selected.clone()
        && ctx.data(|d| d.get_temp::<bool>(egui::Id::unique("res-scroll-to"))).unwrap_or(false)
        && let Some(n) = rows.iter().position(|&i| snap.entries[i].path == sel)
    {
        ctx.data_mut(|d| d.insert_temp(egui::Id::unique("res-scroll-to"), false));
        let off = (n as f32 * row_h - list.height() / 2.0).max(0.0);
        scroll = scroll.vertical_scroll_offset(off);
    }
    if ctx.data(|d| d.get_temp::<bool>(egui::Id::unique("res-scroll-top"))).unwrap_or(false) {
        ctx.data_mut(|d| d.insert_temp(egui::Id::unique("res-scroll-top"), false));
        scroll = scroll.vertical_scroll_offset(0.0);
    }
    let selected = app.assets.selected.clone();
    scroll.show_rows(&mut child, row_h, rows.len(), |ui, range| {
        for n in range {
            let e = &snap.entries[rows[n]];
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
            let resp = ui.interact(r, egui::Id::unique(("res-row", tab.id(), n)), Sense::click_and_drag());
            app.auto.add(&format!("resources.row.{n}"), r, &e.name);
            if n % 2 == 1 {
                ui.painter().rect_filled(r, 0.0, t.row_alt);
            }
            let is_sel = selected.as_deref() == Some(e.path.as_str());
            if is_sel {
                ui.painter().rect_filled(r, 0.0, t.row_selected);
            }
            if resp.hovered() {
                ui.painter().rect_filled(r, 0.0, t.hover);
                if tab == Kind::Luts {
                    hover_lut = Some(e.path.clone());
                }
                hovered_row = Some(e.path.clone());
            }
            let mut x = r.min.x + 4.0;
            // ★
            let star_r = Rect::from_min_size(pos2(x, r.center().y - 8.0), vec2(16.0, 16.0));
            let fav = favorites.contains(&e.path);
            let star = ui.interact(star_r, egui::Id::unique(("res-star", tab.id(), n)), Sense::click());
            app.auto.add(&format!("resources.star.{n}"), star_r, "Favourite");
            ui.painter().text(star_r.center(), Align2::CENTER_CENTER, if fav { "★" } else { "☆" }, Tokens::ui(12.0), if fav { t.accent } else { t.text_faint });
            if star.clicked() {
                action = Some((n, "star", 0.0));
            }
            x += 18.0;
            if sound {
                let pr = Rect::from_min_size(pos2(x, r.center().y - 8.0), vec2(16.0, 16.0));
                let play = ui.interact(pr, egui::Id::unique(("res-play", tab.id(), n)), Sense::click());
                app.auto.add(&format!("resources.play.{n}"), pr, "Play");
                let on = audition.path.as_deref() == Some(e.path.as_str()) && audition.playing;
                icons::paint(ui.painter(), pr.shrink(2.0), if on { Icon::Pause } else { Icon::Play }, if play.hovered() || on { t.accent } else { t.icon });
                if play.clicked() {
                    action = Some((n, "play", 0.0));
                }
                x += 20.0;
            }
            let (name, tags) = display_name(&e.name);
            let right = match (tab, e.dur) {
                (Kind::Titles, _) if !e.ready => tl!("preview only").to_string(),
                (Kind::Titles, _) if e.path.to_ascii_lowercase().ends_with(".mogrt") => tl!("editable").to_string(),
                (_, Some(d)) => duration_text(d),
                _ => String::new(),
            };
            let wide = r.width() > 440.0;
            let extra_w = if sound && wide { 96.0 } else { 0.0 };
            let wave_w = if sound { (r.width() * 0.28).clamp(80.0, 180.0) } else { 0.0 };
            let rw = right.chars().count() as f32 * 6.0 + 8.0;
            let name_w = (r.max.x - x - rw - wave_w - extra_w - 12.0).max(20.0);
            let shown = if tab == Kind::Titles && !e.dir.is_empty() { format!("{name} — {}", e.dir) } else { name.to_string() };
            // long names (the library prefixes them with their folders) keep their end: the
            // part that tells the files apart (".. LogC to ACEScct")
            let shown = elide_middle(ui, &shown, name_w);
            let galley = ui.painter().layout_no_wrap(shown, Tokens::ui(11.5), t.text);
            let clip = Rect::from_min_size(pos2(x, r.min.y), vec2(name_w, row_h));
            ui.painter().with_clip_rect(clip).galley(pos2(x, r.center().y - galley.size().y / 2.0), galley, t.text);
            let mut rx = r.max.x - 6.0;
            ui.painter().text(pos2(rx, r.center().y), Align2::RIGHT_CENTER, &right, Tokens::ui(10.5), t.text_dim);
            rx -= rw;
            if sound {
                if wide {
                    // family (sound effects) or tempo + key (music)
                    let txt = if tab == Kind::Music {
                        let b = e.meta.bpm.map(|b| format!("{b:.0} BPM")).unwrap_or_default();
                        let k = e.meta.key.clone().unwrap_or_default();
                        format!("{b} {k}").trim().to_string()
                    } else {
                        e.family.as_deref().map(family_label).unwrap_or_default()
                    };
                    let weak = tab == Kind::Music && e.meta.bpm_conf.is_some_and(|c| c < 0.5);
                    let er = Rect::from_min_max(pos2(rx - extra_w, r.min.y), pos2(rx, r.max.y));
                    let txt = elide_middle(ui, &txt, extra_w - 4.0);
                    ui.painter().with_clip_rect(er).text(
                        pos2(er.max.x - 2.0, r.center().y),
                        Align2::RIGHT_CENTER,
                        txt,
                        Tokens::ui(10.0),
                        if weak { t.text_faint } else { t.text_dim },
                    );
                    rx -= extra_w;
                }
                let wr = Rect::from_min_max(pos2(rx - wave_w, r.min.y + 4.0), pos2(rx - 4.0, r.max.y - 4.0));
                visible_paths.push(e.path.clone());
                let peaks = app.assets.peaks.as_ref().and_then(|w| w.get(&e.path));
                let unreadable = peaks.is_none() && app.assets.peaks.as_ref().is_some_and(|w| w.failed(&e.path));
                let playing_this = audition.path.as_deref() == Some(e.path.as_str());
                if unreadable {
                    ui.painter().text(wr.center(), Align2::CENTER_CENTER, tl!("not a readable sound"), Tokens::ui(9.5), t.text_faint);
                } else {
                    draw_wave(ui.painter(), wr, peaks.as_deref(), if playing_this { audition.progress } else { None }, &t);
                }
                app.auto.add(&format!("resources.wave.{n}"), wr, &e.name);
                if let Some(p) = pointer
                    && wr.contains(p)
                {
                    let frac = (p.x - wr.min.x) / wr.width().max(1.0);
                    if resp.clicked() {
                        action = Some((n, "playAt", frac));
                    } else if playing_this && audition.playing && ctx.input(|i| i.pointer.delta().x.abs() > 0.0) {
                        action = Some((n, "scrub", frac));
                    }
                }
            }
            // the full name first (the row may be elided), then the folder and the tags
            let head = if e.dir.is_empty() { name.to_string() } else { format!("{name}\n{}", e.dir) };
            let resp = match (tags, &e.family) {
                (Some(tg), _) => resp.on_hover_text(format!("{head}\n{tg}")),
                (None, Some(f)) => resp.on_hover_text(format!("{head}\n{f}")),
                _ => resp.on_hover_text(head),
            };
            if resp.double_clicked() {
                action = Some((n, "apply", 0.0));
            } else if resp.drag_started() {
                action = Some((n, "drag", 0.0));
            } else if resp.clicked() && action.is_none() {
                action = Some((n, "select", 0.0));
            }
        }
    });
    if let Some(w) = app.assets.peaks.as_ref() {
        w.want_visible(visible_paths);
    }
    if hover_lut.is_some() || !ctx.input(|i| i.pointer.any_down()) {
        app.assets.hover_lut = hover_lut;
    }
    // ---- listening on hover (sound tabs)
    if sound {
        match (&hovered_row, pointer) {
            (Some(path), Some(p)) => {
                let moved = app.assets.hover.as_ref().is_none_or(|(hp, _, last)| hp != path || (p - *last).length() > 2.0);
                if moved && app.assets.hover.as_ref().is_none_or(|(hp, _, _)| hp != path) {
                    // a new row: the sound of the previous one (if hover started it) stops
                    if app.assets.audition.path.is_some() && app.assets.audition.path.as_ref() != Some(path) && hover_on(app, tab) {
                        app.assets.audition.stop();
                    }
                    app.assets.hover = Some((path.clone(), now, p));
                } else if moved && let Some(h) = app.assets.hover.as_mut() {
                    h.2 = p;
                    if app.assets.audition.path.as_ref() != Some(path) {
                        h.1 = now;
                    }
                }
                let still_for = app.assets.hover.as_ref().map(|h| now - h.1).unwrap_or(0.0);
                if hover_on(app, tab) && still_for >= HOVER_DELAY && app.assets.audition.path.as_ref() != Some(path) && !assets_mod::other_playing(app) {
                    let path = path.clone();
                    assets_mod::audition(app, &path, start_frac(tab));
                }
                if still_for < HOVER_DELAY {
                    ctx.request_repaint_after(std::time::Duration::from_millis(40));
                }
            }
            _ => {
                if app.assets.hover.take().is_some() && hover_on(app, tab) && app.assets.audition.path.is_some() {
                    app.assets.audition.stop();
                }
            }
        }
    }
    if let Some((n, what, frac)) = action {
        let e = snap.entries[rows[n]].clone();
        match what {
            "play" => toggle_play(app, &e.path, start_frac(tab)),
            "playAt" => {
                app.assets.selected = Some(e.path.clone());
                assets_mod::audition(app, &e.path, frac);
            }
            "scrub" => app.assets.audition.seek_frac(frac),
            "star" => {
                assets_mod::library_state(app).toggle_favorite(&e.path);
                assets_mod::save_library_state(app);
            }
            "select" => app.assets.selected = Some(e.path.clone()),
            "drag" => {
                if tab == Kind::Titles && !e.ready {
                    app.ui.status = tl!("After Effects title: only its preview is dropped, as a reference (FilmCraft can't convert it)").to_string();
                }
                app.assets.drag = Some(AssetDrag { paths: vec![e.path.clone()], label: e.name.clone() });
            }
            _ => apply(app, tab, &e.path),
        }
    }
}

/// Titles as a grid of thumbnails (`thumb.png`, read in the background, 240 px).
fn titles_grid(app: &mut FilmcraftApp, ui: &mut egui::Ui, ctx: &egui::Context, snap: &index::Snapshot, rows: &[usize], list: Rect) {
    let t = app.tokens;
    // thumbnails read in the background arrive here
    let done: Vec<(String, Option<egui::ColorImage>)> = std::mem::take(&mut *app.assets.thumbs_loading.lock().unwrap_or_else(|e| e.into_inner()));
    for (p, img) in done {
        let tex = img.map(|im| ctx.load_texture(format!("res-thumb-{p}"), im, egui::TextureOptions::LINEAR));
        app.assets.thumbs.insert(p, tex);
    }
    if app.assets.thumbs.len() > 600 {
        app.assets.thumbs.clear();
    }
    let cols = ((list.width() - 8.0) / (TILE_W + 6.0)).floor().max(1.0) as usize;
    let lines = rows.len().div_ceil(cols);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list).id_salt("res-grid"));
    let mut action: Option<(usize, &'static str)> = None;
    let selected = app.assets.selected.clone();
    let mut want: Vec<String> = Vec::new();
    egui::ScrollArea::vertical().id_salt("res-grid-scroll").auto_shrink([false, false]).show_rows(&mut child, TILE_H + 6.0, lines, |ui, range| {
        for line in range {
            let (lr, _) = ui.allocate_exact_size(vec2(ui.available_width(), TILE_H + 6.0), Sense::hover());
            for c in 0..cols {
                let n = line * cols + c;
                let Some(&i) = rows.get(n) else { break };
                let e = &snap.entries[i];
                let r = Rect::from_min_size(pos2(lr.min.x + 4.0 + c as f32 * (TILE_W + 6.0), lr.min.y + 3.0), vec2(TILE_W, TILE_H));
                let resp = ui.interact(r, egui::Id::unique(("res-tile", n)), Sense::click_and_drag());
                app.auto.add(&format!("resources.row.{n}"), r, &e.name);
                let sel = selected.as_deref() == Some(e.path.as_str());
                ui.painter().rect_filled(
                    r,
                    4.0,
                    if sel {
                        t.row_selected
                    } else if resp.hovered() {
                        t.hover
                    } else {
                        t.field_bg
                    },
                );
                let img_r = Rect::from_min_size(r.min + vec2(4.0, 4.0), vec2(TILE_W - 8.0, TILE_H - 30.0));
                let key = e.thumb.clone().unwrap_or_default();
                // a neutral checkerboard behind the picture: a title on a transparent background
                // (white or black text) stays visible
                checker(ui.painter(), img_r);
                match app.assets.thumbs.get(&key) {
                    Some(Some(tex)) => {
                        let sz = tex.size_vec2();
                        let s = (img_r.width() / sz.x).min(img_r.height() / sz.y);
                        let ir = Rect::from_center_size(img_r.center(), sz * s);
                        ui.painter().image(tex.id(), ir, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                    }
                    // no picture, or an empty one (some packs ship an all-black preview): the
                    // title's name, large, instead of a black square
                    Some(None) | None if key.is_empty() || app.assets.thumbs.contains_key(&key) => {
                        let g = ui.painter().layout(display_name(&e.name).0.to_string(), Tokens::ui(13.0), Color32::from_gray(230), img_r.width() - 12.0);
                        let at = img_r.center() - g.size() / 2.0;
                        ui.painter().rect_filled(Rect::from_min_size(at - vec2(4.0, 2.0), g.size() + vec2(8.0, 4.0)), 3.0, Color32::from_black_alpha(140));
                        ui.painter().with_clip_rect(img_r).galley(at, g, Color32::from_gray(230));
                        ui.painter().text(
                            img_r.left_bottom() + vec2(4.0, -4.0),
                            Align2::LEFT_BOTTOM,
                            tl!("no preview"),
                            Tokens::ui(9.0),
                            Color32::from_gray(200),
                        );
                    }
                    _ => want.push(key),
                }
                let badge = if e.ready { tl!("editable") } else { tl!("preview only") };
                // the name on two lines (cut in the middle beyond), readable
                let nm = display_name(&e.name).0.to_string();
                let g = ui.painter().layout(nm, Tokens::ui(11.0), t.text, TILE_W - 8.0);
                let name_r = Rect::from_min_max(pos2(r.min.x + 4.0, img_r.max.y + 2.0), pos2(r.max.x - 4.0, r.max.y - 1.0));
                ui.painter().with_clip_rect(name_r).galley(name_r.min, g, t.text);
                let bg = ui.painter().layout_no_wrap(badge.to_string(), Tokens::ui(9.5), if e.ready { t.accent } else { t.text_faint });
                let br = Rect::from_min_size(pos2(r.max.x - 8.0 - bg.size().x, r.min.y + 6.0), bg.size() + vec2(4.0, 2.0));
                ui.painter().rect_filled(br, 3.0, Color32::from_black_alpha(170));
                ui.painter().galley(br.min + vec2(2.0, 1.0), bg, t.accent);
                let resp = resp.on_hover_text(format!("{}\n{}", e.name, e.dir));
                if resp.double_clicked() {
                    action = Some((n, "apply"));
                } else if resp.drag_started() {
                    action = Some((n, "drag"));
                } else if resp.clicked() {
                    action = Some((n, "select"));
                }
            }
        }
    });
    // read the missing thumbnails in the background (a few per frame, never on this thread)
    for p in want.into_iter().take(8) {
        app.assets.thumbs.insert(p.clone(), None);
        let sink = app.assets.thumbs_loading.clone();
        let ctx2 = ctx.clone();
        let _ = std::thread::Builder::new().name("res-thumb".into()).spawn(move || {
            let img = std::fs::read(&p)
                .ok()
                .and_then(|b| image::load_from_memory(&b).ok())
                .map(|im| im.thumbnail(240, 240).to_rgba8())
                .filter(|im| !is_blank(im))
                .map(|im| egui::ColorImage::from_rgba_unmultiplied([im.width() as usize, im.height() as usize], im.as_raw()));
            sink.lock().unwrap_or_else(|e| e.into_inner()).push((p, img));
            ctx2.request_repaint();
        });
    }
    if let Some((n, what)) = action {
        let e = snap.entries[rows[n]].clone();
        match what {
            "select" => app.assets.selected = Some(e.path.clone()),
            "drag" => {
                if !e.ready {
                    app.ui.status = tl!("After Effects title: only its preview is dropped, as a reference (FilmCraft can't convert it)").to_string();
                }
                app.assets.drag = Some(AssetDrag { paths: vec![e.path.clone()], label: e.name.clone() });
            }
            _ => apply(app, Kind::Titles, &e.path),
        }
    }
}

/// A preview that shows nothing: every pixel black (or nearly), or fully transparent.
pub fn is_blank(im: &image::RgbaImage) -> bool {
    im.pixels().all(|p| p[3] < 8 || (p[0] < 12 && p[1] < 12 && p[2] < 12))
}

/// A neutral grey checkerboard (behind pictures that may be transparent).
fn checker(p: &egui::Painter, r: Rect) {
    p.rect_filled(r, 2.0, Color32::from_gray(58));
    let s = 8.0;
    let (nx, ny) = ((r.width() / s).ceil() as i32, (r.height() / s).ceil() as i32);
    let p = p.with_clip_rect(r);
    for j in 0..ny {
        for i in 0..nx {
            if (i + j) % 2 == 0 {
                p.rect_filled(Rect::from_min_size(pos2(r.min.x + i as f32 * s, r.min.y + j as f32 * s), vec2(s, s)), 0.0, Color32::from_gray(74));
            }
        }
    }
}

/// The LUT preview strip (before / after on the current shot). Returns the new top.
fn lut_preview(app: &mut FilmcraftApp, ui: &mut egui::Ui, ctx: &egui::Context, rect: Rect, top: f32) -> f32 {
    let t = app.tokens;
    let pr = Rect::from_min_size(pos2(rect.min.x + 6.0, top), vec2(rect.width() - 12.0, 96.0));
    let hovered = app.assets.hover_lut.clone();
    assets_mod::update_preview(app, ctx, hovered.as_deref());
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
            ui.painter().image(before.id(), a, uv, Color32::WHITE);
            let lbl_y = a.max.y + 7.0;
            ui.painter().text(pos2(a.center().x, lbl_y), Align2::CENTER_CENTER, tl!("Before"), Tokens::ui(10.0), t.text_dim);
            let after = app.assets.preview.after.as_ref().filter(|_| hovered.is_some() && app.assets.preview.after_of == hovered);
            if let Some(after) = after {
                ui.painter().image(after.id(), b, uv, Color32::WHITE);
                let name = hovered
                    .as_deref()
                    .map(|p| display_name(std::path::Path::new(p).file_name().and_then(|s| s.to_str()).unwrap_or(p)).0.to_string())
                    .unwrap_or_default();
                // a long LUT name would run over the "Before" label: keep it under its picture
                let name = elide_middle(ui, &name, b.width());
                ui.painter().text(pos2(b.center().x, lbl_y), Align2::CENTER_CENTER, name, Tokens::ui(10.0), t.text);
            } else {
                ui.painter().rect_stroke(b, 2.0, Stroke::new(1.0, t.separator), egui::StrokeKind::Inside);
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
    pr.max.y + 4.0
}

/// Keyboard of the focused panel (called before the application shortcuts, which would take
/// Space and the arrows for the timeline).
pub fn keyboard(app: &mut FilmcraftApp, ctx: &egui::Context) {
    use egui::{Key, Modifiers};
    let tab = app.assets.tab.unwrap_or(Kind::Luts);
    let Some((_, rows)) = app.assets.filtered.clone() else { return };
    let Some(snap) = app.assets.libs.get(&tab).map(|l| l.snapshot()) else { return };
    let paths: Vec<String> = rows.iter().filter_map(|&i| snap.entries.get(i).map(|e| e.path.clone())).collect();
    let cur = app.assets.selected.as_ref().and_then(|s| paths.iter().position(|p| p == s));
    let (down, up, space, enter, esc, fav, slash) = ctx.input_mut(|i| {
        (
            i.consume_key(Modifiers::NONE, Key::ArrowDown),
            i.consume_key(Modifiers::NONE, Key::ArrowUp),
            i.consume_key(Modifiers::NONE, Key::Space),
            i.consume_key(Modifiers::NONE, Key::Enter),
            i.consume_key(Modifiers::NONE, Key::Escape),
            i.consume_key(Modifiers::NONE, Key::F),
            i.consume_key(Modifiers::NONE, Key::Slash),
        )
    });
    if down || up {
        let n = match (cur, down) {
            (None, _) => 0,
            (Some(c), true) => (c + 1).min(paths.len().saturating_sub(1)),
            (Some(c), false) => c.saturating_sub(1),
        };
        if let Some(p) = paths.get(n).cloned() {
            app.assets.selected = Some(p.clone());
            ctx.data_mut(|d| d.insert_temp(egui::Id::unique("res-scroll-to"), true));
            if hover_on(app, tab) {
                assets_mod::audition(app, &p, start_frac(tab));
            }
        }
    }
    let sel = app.assets.selected.clone();
    if space && let Some(p) = &sel {
        toggle_play(app, p, start_frac(tab));
    }
    if enter && let Some(p) = &sel {
        apply(app, tab, p);
    }
    if fav && let Some(p) = &sel {
        assets_mod::library_state(app).toggle_favorite(p);
        assets_mod::save_library_state(app);
    }
    if esc {
        if app.assets.audition.state().playing {
            app.assets.audition.stop();
        } else if !app.assets.query.is_empty() {
            app.assets.query.clear();
        }
    }
    if slash {
        ctx.memory_mut(|m| m.request_focus(egui::Id::unique("res-search")));
    }
}

/// Empty the light preview cache in a background thread (the waveforms are kept); the previews
/// are made again while browsing.
pub fn clear_previews(app: &mut FilmcraftApp, store: std::sync::Arc<assets_mod::peaks::PreviewStore>) {
    let _ = std::thread::Builder::new().name("resources-previews-clear".into()).spawn(move || {
        let freed = store.clear();
        log::info!("preview cache emptied: {freed} bytes in {}", store.dir.display());
    });
    app.ui.status = tl!("Preview cache emptied").to_string();
}

/// Progress of "Convert all shown" (Titles).
#[derive(Clone, Debug, Default)]
pub struct ConvertJob {
    pub total: usize,
    pub done: usize,
    pub failed: usize,
    pub running: bool,
    pub last_error: Option<String>,
}

/// "Convert all shown (N)" and its progress: converts the listed Premiere titles into the user's
/// graphics templates in a background thread (the library may be on a slow disk). Returns the new
/// top of the list.
fn convert_all_row(app: &mut FilmcraftApp, ui: &mut egui::Ui, snap: &index::Snapshot, rows: &[usize], rect: Rect, top: f32) -> f32 {
    let t = app.tokens;
    let todo: Vec<String> =
        rows.iter().map(|&i| &snap.entries[i]).filter(|e| e.ready && e.path.to_ascii_lowercase().ends_with(".mogrt")).map(|e| e.path.clone()).collect();
    let job = app.assets.convert_job.as_ref().map(|j| j.lock().unwrap_or_else(|e| e.into_inner()).clone());
    let r = Rect::from_min_size(pos2(rect.min.x + 6.0, top), vec2((rect.width() - 12.0).min(260.0), 20.0));
    let running = job.as_ref().is_some_and(|j| j.running);
    let label = match &job {
        Some(j) if j.running => tlf!("Converting {done} of {total}…", done = j.done, total = j.total),
        _ => tlf!("Convert all shown ({n})", n = todo.len()),
    };
    let resp = ui.interact(r, egui::Id::unique("res-convert-all"), Sense::click());
    app.auto.add("resources.convertAll", r, &label);
    let enabled = !running && !todo.is_empty();
    ui.painter().rect_filled(
        r,
        4.0,
        if resp.hovered() && enabled {
            t.accent_hover
        } else if enabled {
            t.accent
        } else {
            t.tab_bg
        },
    );
    ui.painter().text(r.center(), Align2::CENTER_CENTER, &label, Tokens::ui(11.0), egui::Color32::WHITE);
    let mut y = r.max.y + 4.0;
    if let Some(j) = job.filter(|j| !j.running && j.total > 0) {
        let msg = match &j.last_error {
            Some(e) => tlf!("{done} converted, {failed} not converted ({error})", done = j.done - j.failed, failed = j.failed, error = e),
            None => tlf!("{done} converted into Graphics Templates", done = j.done),
        };
        let g = ui.painter().layout(msg.to_string(), Tokens::ui(10.0), t.text_dim, rect.width() - 16.0);
        ui.painter().galley(pos2(rect.min.x + 8.0, y), g.clone(), t.text_dim);
        y += g.size().y + 4.0;
    }
    if running {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
    }
    if resp.clicked() && enabled {
        match filmcraft_engine::graphic_templates::user_templates_dir(&app.session) {
            Some(dir) => {
                let job = std::sync::Arc::new(std::sync::Mutex::new(ConvertJob { total: todo.len(), running: true, ..Default::default() }));
                app.assets.convert_job = Some(job.clone());
                let ctx = ui.ctx().clone();
                let spawned = std::thread::Builder::new().name("titles-convert".into()).spawn(move || {
                    for p in todo {
                        let r = filmcraft_engine::graphic_templates::convert_mogrt_file(&p, &dir);
                        let mut j = job.lock().unwrap_or_else(|e| e.into_inner());
                        j.done += 1;
                        if let Err(e) = r {
                            j.failed += 1;
                            j.last_error = Some(e);
                        }
                        ctx.request_repaint();
                    }
                    job.lock().unwrap_or_else(|e| e.into_inner()).running = false;
                    ctx.request_repaint();
                });
                if spawned.is_err() {
                    app.ui.status = tl!("Could not start the conversion").to_string();
                    app.assets.convert_job = None;
                }
            }
            None => app.ui.status = tl!("No data folder for graphics templates").to_string(),
        }
    }
    y
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
    if let Ok(v) = &r {
        assets_mod::note_used(app, v);
    }
    app.assets.last_drop = Some(match r {
        Ok(v) => json!({"params": q, "result": v}),
        Err(e) => json!({"params": q, "error": e.to_string()}),
    });
}

/// ▶ / Space: play the sound in the app, or pause / resume it.
pub fn toggle_play(app: &mut FilmcraftApp, path: &str, frac: f32) {
    let st = app.assets.audition.state();
    if st.path.as_deref() == Some(path) && app.assets.audition.owns_output() {
        app.assets.audition.toggle_pause();
    } else {
        assets_mod::audition(app, path, frac);
    }
}

/// `text` shortened in the middle ("start … end") to fit `width` at the row font.
fn elide_middle(ui: &egui::Ui, text: &str, width: f32) -> String {
    crate::theme::elide_middle(ui.painter(), text, 11.5, width)
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
