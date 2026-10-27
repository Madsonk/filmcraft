//! The licence dialog for the optional neural models (voice clean-up, auto-reframe).
//!
//! `models.install {model}` without `accept: true` — from a menu, an extension entry or the
//! control channel — opens this dialog instead of downloading: it shows the model's size, source
//! and licence, and nothing is downloaded until the user presses the accept button. The download
//! runs on its own thread (SHA-256 checked by `filmcraft-modelstore`), with a progress bar.
//! Automation ids: `models.consent.accept`, `models.consent.cancel`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::FilmcraftApp;

enum State {
    Idle,
    Asking(&'static filmcraft_modelstore::ModelInfo),
    Downloading(&'static filmcraft_modelstore::ModelInfo, Arc<Progress>),
}

struct Progress {
    done: AtomicU64,
    total: AtomicU64,
    cancel: AtomicBool,
    result: Mutex<Option<Result<String, String>>>,
}

fn state() -> &'static Mutex<State> {
    static S: std::sync::OnceLock<Mutex<State>> = std::sync::OnceLock::new();
    S.get_or_init(|| Mutex::new(State::Idle))
}

/// `models.install` from the window: `Some(result)` when this module answers it (a dialog was
/// opened, or the model is already there), `None` to let the engine run it (`accept: true`).
pub fn route(id: &str, params: &Value) -> Option<Result<Value, String>> {
    if id != "models.install" || params.get("accept").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    let want = params.get("model").and_then(Value::as_str)?;
    let m = filmcraft_modelstore::find(want)?;
    let root = filmcraft_engine::transcript::models_dir()?;
    if filmcraft_modelstore::installed(&root, m) {
        return Some(Ok(json!({"model": m.id, "installed": true, "summary": format!("{} est déjà installé", m.name)})));
    }
    let mut s = state().lock().unwrap_or_else(|e| e.into_inner());
    if matches!(*s, State::Idle) {
        *s = State::Asking(m);
    }
    Some(Ok(json!({"model": m.id, "asking": true, "summary": format!("{} : en attente de votre accord sur la licence (fenêtre ouverte)", m.name)})))
}

/// Draws the dialog (called every frame).
pub fn dialog(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let mut s = state().lock().unwrap_or_else(|e| e.into_inner());
    let mut elems: Vec<(String, egui::Rect, String)> = Vec::new();
    let mut next: Option<State> = None;
    match &*s {
        State::Idle => return,
        State::Asking(m) => {
            let m = *m;
            egui::Window::new("Modèle optionnel")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .default_width(480.0)
                .show(ctx, |ui| {
                    ui.strong(format!("{} n'est pas installé.", m.name));
                    ui.label(m.task);
                    ui.label(m.description);
                    ui.add_space(4.0);
                    ui.label(format!("À télécharger : {:.1} Mo", m.size() as f64 / 1e6));
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Source :");
                        ui.hyperlink_to(m.source, m.source);
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!("Licence : {} ·", m.license));
                        ui.hyperlink_to("texte de la licence", m.license_url);
                    });
                    ui.label("Chaque fichier est vérifié (SHA-256) avant d'être utilisé. Le modèle s'exécute sur votre ordinateur ; rien n'est envoyé.");
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let r = ui.button("J'ai lu la licence : installer");
                        elems.push(("models.consent.accept".into(), r.rect, "Accept".into()));
                        if r.clicked() {
                            let p = Arc::new(Progress {
                                done: AtomicU64::new(0),
                                total: AtomicU64::new(m.size()),
                                cancel: AtomicBool::new(false),
                                result: Mutex::new(None),
                            });
                            let (p2, id) = (p.clone(), m.id);
                            std::thread::spawn(move || {
                                let r = filmcraft_engine::voice_clean::install_model(id, &mut |d, t| {
                                    p2.done.store(d, Ordering::Relaxed);
                                    p2.total.store(t, Ordering::Relaxed);
                                    !p2.cancel.load(Ordering::Relaxed)
                                });
                                *p2.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                            });
                            next = Some(State::Downloading(m, p));
                        }
                        let r = ui.button("Annuler");
                        elems.push(("models.consent.cancel".into(), r.rect, "Cancel".into()));
                        if r.clicked() {
                            next = Some(State::Idle);
                        }
                    });
                });
        }
        State::Downloading(m, p) => {
            let finished = p.result.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(r) = finished {
                app.ui.status = match r {
                    Ok(msg) => msg,
                    Err(e) => format!("Téléchargement de {} impossible : {e}", m.name),
                };
                app.extensions.models_stale();
                next = Some(State::Idle);
            } else {
                let (d, t) = (p.done.load(Ordering::Relaxed), p.total.load(Ordering::Relaxed).max(1));
                egui::Window::new("Téléchargement du modèle")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -48.0])
                    .default_width(380.0)
                    .show(ctx, |ui| {
                        ui.label(format!("{} : {:.1} / {:.1} Mo", m.name, d as f64 / 1e6, t as f64 / 1e6));
                        ui.add(egui::ProgressBar::new((d as f32 / t as f32).clamp(0.0, 1.0)));
                        let r = ui.button("Annuler");
                        elems.push(("models.consent.cancel".into(), r.rect, "Cancel".into()));
                        if r.clicked() {
                            p.cancel.store(true, Ordering::Relaxed);
                        }
                    });
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
    }
    if let Some(n) = next {
        *s = n;
    }
    drop(s);
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
}
