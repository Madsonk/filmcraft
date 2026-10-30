//! Colour dialogs: Clip ▸ Modify ▸ Interpret Footage… (Color Management) and Sequence ▸ Color
//! Management… (working space, wide gamut, Auto Tone Map Media). Each applies one engine command
//! on OK, so it is a single undo step. Automation ids: `colorDialog.space.<id>`,
//! `colorDialog.working.<id>`, `colorDialog.wideGamut`, `colorDialog.autoToneMap`,
//! `colorDialog.ok`, `colorDialog.cancel`.
//!
//! Clip ▸ Match Shot Colour… (`color.matchShots`): reference clip, clip to match (lists of the
//! sequence's video clips, or the timeline selection), method, strength, apply. The window is not
//! modal, so clips can be picked in the timeline while it is open; the Program monitor is the
//! preview (undo puts the clip back). Ids: `colorDialog.match.reference` / `.target` (lists,
//! options `.reference.<clip>` / `.target.<clip>`), `colorDialog.match.referenceFromSelection`,
//! `colorDialog.match.targetFromSelection`, `colorDialog.match.method.<id>`,
//! `colorDialog.match.strength`, `colorDialog.match.apply`.

use filmcraft_color::{ColorSpace, WorkingSpace};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::state::ColorDialog;

pub fn open_interpret(app: &mut FilmcraftApp, params: &Value) {
    let items: Vec<u64> = match params.get("items").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(Value::as_u64).collect(),
        None => {
            let s = &app.session;
            let mut v: Vec<u64> = s.state.project_selection.iter().map(|i| i.0).collect();
            if v.is_empty()
                && let Some(q) = s.active_sequence()
            {
                v = s.state.selection.iter().filter_map(|c| q.find_item(*c).map(|(_, it)| it.item.0)).collect();
                v.dedup();
            }
            v
        }
    };
    let current = items
        .first()
        .and_then(|i| app.session.execute("media.colorInfo", json!({"item": i})).ok())
        .and_then(|v| v["override"].as_str().map(str::to_string))
        .unwrap_or_else(|| "auto".into());
    app.ui.color_dialog = Some(ColorDialog::Interpret { items, color_space: current });
}

pub fn open_sequence(app: &mut FilmcraftApp) {
    let c = app.session.active_sequence().map(|q| q.settings.color).unwrap_or(filmcraft_color::ColorPipeline::REC709);
    app.ui.color_dialog = Some(ColorDialog::Sequence { working_space: c.working.id().into(), wide_gamut: c.wide_gamut, auto_tone_map: c.auto_tone_map });
}

/// The active sequence's video clips, in timeline order: (clip id, "V2 · name · 1:23.4").
fn video_clips(app: &FilmcraftApp) -> Vec<(u64, String)> {
    let s = &app.session;
    let Some(q) = s.active_sequence() else { return Vec::new() };
    let mut out = Vec::new();
    for (n, tr) in q.video_tracks.iter().enumerate() {
        for it in &tr.items {
            let name = s.project.item(it.item).map(|i| i.name.clone()).unwrap_or_default();
            let sec = it.start.seconds().max(0.0);
            out.push((it.id.0, format!("V{} · {name} · {}:{:04.1}", n + 1, (sec / 60.0).floor() as u64, sec % 60.0)));
        }
    }
    out
}

/// The selected video clips of the timeline, in selection order.
fn selected_video(app: &FilmcraftApp, clips: &[(u64, String)]) -> Vec<u64> {
    app.session.state.selection.iter().map(|c| c.0).filter(|c| clips.iter().any(|(id, _)| id == c)).collect()
}

pub fn open_match(app: &mut FilmcraftApp) {
    let clips = video_clips(app);
    let sel = selected_video(app, &clips);
    // two selected: the first is the reference; one selected: it is the clip to match
    let (reference, target) = match sel.as_slice() {
        [a, b, ..] => (*a, *b),
        [t] => (clips.iter().map(|c| c.0).find(|c| c != t).unwrap_or(0), *t),
        [] => (0, 0),
    };
    app.ui.color_dialog = Some(ColorDialog::MatchShots { clips, reference, target, method: "reinhard".into(), strength: 100.0, apply: true });
}

fn clip_combo(app: &mut FilmcraftApp, ui: &mut egui::Ui, id: &str, clips: &[(u64, String)], value: &mut u64) {
    let current = clips.iter().find(|c| c.0 == *value).map(|c| c.1.clone()).unwrap_or_else(|| tl!("(choose a clip)").to_string());
    let r = craft_ui::ComboBox::from_id_salt(id).width(360.0).selected_text(current).show_ui(ui, |ui| {
        for (c, label) in clips {
            let o = ui.selectable_value(value, *c, label);
            app.auto.add(&format!("{id}.{c}"), o.rect, label);
        }
    });
    app.auto.add(id, r.response.rect, id);
}

fn radio(app: &mut FilmcraftApp, ui: &mut egui::Ui, id: &str, selected: bool, label: &str) -> bool {
    let r = ui.radio(selected, label);
    app.auto.add(id, r.rect, label);
    r.clicked()
}

fn buttons(app: &mut FilmcraftApp, ui: &mut egui::Ui) -> (bool, bool) {
    let mut out = (false, false);
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let c = ui.button(tl!("Cancel"));
        app.auto.add("colorDialog.cancel", c.rect, "Cancel");
        let o = ui.button(tl!("OK"));
        app.auto.add("colorDialog.ok", o.rect, "OK");
        out = (o.clicked(), c.clicked());
    });
    out
}

pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.color_dialog.clone() else { return };
    let mut open = true;
    let mut apply: Option<(&'static str, Value)> = None;
    let mut close = false;
    match &mut d {
        ColorDialog::Interpret { items, color_space } => {
            let detected = items
                .first()
                .and_then(|i| app.session.execute("media.colorInfo", json!({"item": i})).ok())
                .and_then(|v| v["detectedLabel"].as_str().map(str::to_string))
                .unwrap_or_else(|| "—".into());
            let n = items.len();
            egui::Window::new(tl!("Interpret Footage"))
                .id(egui::Id::unique("Interpret Footage"))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_width(380.0)
                .show(ctx, |ui| {
                    ui.label(
                        egui::RichText::new(if n == 1 { tlf!("Color Management · {n} item", n) } else { tlf!("Color Management · {n} items", n) }).strong(),
                    );
                    ui.label(tlf!("Media colour space (from file metadata): {detected}", detected));
                    ui.add_space(4.0);
                    ui.label(tl!("Override media colour space:"));
                    if radio(app, ui, "colorDialog.space.auto", color_space == "auto", tl!("Use file metadata")) {
                        *color_space = "auto".into();
                    }
                    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                        for cs in ColorSpace::ALL {
                            if radio(app, ui, &format!("colorDialog.space.{}", cs.id()), color_space == cs.id(), cs.label()) {
                                *color_space = cs.id().into();
                            }
                        }
                    });
                    let (ok, cancel) = buttons(app, ui);
                    if ok {
                        apply = Some(("clip.interpretFootage", json!({"items": items, "colorSpace": color_space})));
                    }
                    close = ok || cancel;
                });
        }
        ColorDialog::Sequence { working_space, wide_gamut, auto_tone_map } => {
            egui::Window::new(tl!("Sequence Color Management"))
                .id(egui::Id::unique("Sequence Color Management"))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_width(340.0)
                .show(ctx, |ui| {
                    ui.label(egui::RichText::new(tl!("Working Color Space")).strong());
                    for w in WorkingSpace::ALL {
                        if radio(app, ui, &format!("colorDialog.working.{}", w.id()), working_space == w.id(), w.label()) {
                            *working_space = w.id().into();
                        }
                    }
                    ui.add_space(4.0);
                    let r = ui.checkbox(wide_gamut, tl!("Wide gamut color (composite in BT.2020)"));
                    app.auto.add("colorDialog.wideGamut", r.rect, "Wide gamut");
                    let r = ui.checkbox(auto_tone_map, tl!("Auto tone map media (HDR and log into SDR)"));
                    app.auto.add("colorDialog.autoToneMap", r.rect, "Auto tone map");
                    let (ok, cancel) = buttons(app, ui);
                    if ok {
                        apply =
                            Some(("sequence.colorSettings", json!({"workingSpace": working_space, "wideGamut": *wide_gamut, "autoToneMap": *auto_tone_map})));
                    }
                    close = ok || cancel;
                });
        }
        ColorDialog::MatchShots { clips, reference, target, method, strength, apply: apply_look } => {
            egui::Window::new(tl!("Match Shot Colour"))
                .id(egui::Id::unique("Match Shot Colour"))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_width(420.0)
                .show(ctx, |ui| {
                    ui.label(egui::RichText::new(tl!("Reference clip (the look to reach)")).strong());
                    ui.horizontal(|ui| {
                        clip_combo(app, ui, "colorDialog.match.reference", clips, reference);
                        let b = ui.button(tl!("Use selected clip"));
                        app.auto.add("colorDialog.match.referenceFromSelection", b.rect, "Use selected clip");
                        if b.clicked()
                            && let Some(c) = selected_video(app, clips).first()
                        {
                            *reference = *c;
                        }
                    });
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(tl!("Clip to match")).strong());
                    ui.horizontal(|ui| {
                        clip_combo(app, ui, "colorDialog.match.target", clips, target);
                        let b = ui.button(tl!("Use selected clip"));
                        app.auto.add("colorDialog.match.targetFromSelection", b.rect, "Use selected clip");
                        if b.clicked()
                            && let Some(c) = selected_video(app, clips).last()
                        {
                            *target = *c;
                        }
                    });
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(tl!("Method")).strong());
                    for (m, label) in [
                        ("reinhard", tl!("Mean and spread (Reinhard), recommended")),
                        ("mkl", tl!("Linear transport 3×3 (Monge–Kantorovich)")),
                        ("histogram", tl!("Histogram per channel")),
                    ] {
                        if radio(app, ui, &format!("colorDialog.match.method.{m}"), method == m, label) {
                            *method = m.into();
                        }
                    }
                    ui.add_space(4.0);
                    let r = ui.add(egui::Slider::new(strength, 0.0..=100.0).suffix(" %").text(tl!("Strength")));
                    app.auto.add("colorDialog.match.strength", r.rect, "Strength");
                    let r = ui.checkbox(apply_look, tl!("Apply to the clip (Lumetri Creative Look)"));
                    app.auto.add("colorDialog.match.apply", r.rect, "Apply");
                    ui.add_space(4.0);
                    let note1 = tl!("Measured after each clip's Input LUT and Basic Correction: convert log or flat footage to Rec.709 first.");
                    let note2 = tl!("The LUT joins the project's LUT library; Edit ▸ Undo removes it.");
                    ui.label(egui::RichText::new(note1).small().weak());
                    ui.label(egui::RichText::new(note2).small().weak());
                    let ready = *reference != 0 && *target != 0 && reference != target;
                    if !ready {
                        ui.label(egui::RichText::new(tl!("Choose two different clips.")).color(ui.visuals().warn_fg_color));
                    }
                    let (ok, cancel) = buttons(app, ui);
                    if ok && ready {
                        apply = Some((
                            "color.matchShots",
                            json!({"referenceClip": *reference, "targetClip": *target, "method": method, "strength": strength.round(), "apply": *apply_look}),
                        ));
                    }
                    close = (ok && ready) || cancel;
                });
        }
    }
    app.ui.color_dialog = if close || !open { None } else { Some(d) };
    if let Some((cmd, p)) = apply {
        match app.session.execute(cmd, p) {
            Err(e) => app.ui.status = e.to_string(),
            Ok(r) if cmd == "color.matchShots" => {
                let d = &r["meanDistance"];
                let f = |v: &Value| v.as_f64().map(|x| format!("{x:.1}")).unwrap_or_else(|| "—".into());
                let (before, after) = (f(&d["before"]), f(if d["measuredAfter"].is_null() { &d["predictedAfter"] } else { &d["measuredAfter"] }));
                let name = r["name"].as_str().unwrap_or_default().to_string();
                app.ui.status = tlf!("Shot match: mean colour distance {before} → {after} (LUT “{name}”)", before, after, name);
            }
            Ok(_) => {}
        }
    }
}
