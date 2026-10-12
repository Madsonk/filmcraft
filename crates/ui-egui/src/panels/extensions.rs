//! The Extensions menu and its manager (`filmcraft-extensions`, docs/extensions.md).
//!
//! * **Menu.** A top-level *Extensions* menu grouped by the manifest's `category` (Montage · Audio ·
//!   Sous-titres et langue · Image et IA · Export · Gestion, a disabled heading and a separator above
//!   each group), one item per installed extension in alphabetical order inside a group (a submenu
//!   named after the extension when it has several entries), then *Installer une extension…*, *Gérer
//!   les extensions…* and *Recharger les extensions* under Gestion. Items are disabled when the entry's
//!   `requires` (project, sequence, selection) are not met. An extension whose manifest lists `models`
//!   shows the state of each (✓ installé / ↓ à télécharger, with the size) at the top of its submenu,
//!   or after its name when it has a single entry ([`ModelState`], refreshed from `models.list` and
//!   `ml.models`).
//! * **Commands** (menu, control channel, MCP bridge): `extensions.list`, `extensions.run {extension,
//!   entry?, vars?, background?}`, `extensions.status`, `extensions.cancel`, `extensions.install {path?,
//!   folder?}`, `extensions.remove {id}`, `extensions.reload`, `extensions.manage`, `extensions.offers`,
//!   `extensions.installBundled {id}`, `extensions.declineBundled {id}`; the menu items themselves are
//!   `ext:<id>:<entry>`.
//! * **Dialog.** *Gérer les extensions…*: the installed extensions with their entries, install from a
//!   file or a folder, remove, reload, the shipped extensions not installed yet, the last run's result.
//!   Automation ids `extensions.dialog.*`.
//! * **Shipped extensions that start a program** are never installed silently: [`offers_dialog`] asks
//!   (`extensions.offer.*`) — a small window in the corner, once the project is on screen, never over
//!   the dialogs; *Plus tard* puts the question off for seven days or until the next version
//!   (`ExtensionStore::snooze_offers`) — and the manager lists them with a warning.
//! * **Optional neural models** are installed from one dialog ([`licence_dialog`] for the `ml.*`
//!   catalogue, `model_consent` for `models.*`): what the model does, download size, licence in one
//!   line with its link, an Install button, then a progress window with Cancel.
//!
//! **Running.** A menu or manager run goes to a worker thread (`Running`): the entry's commands are asked
//! of the window, which serves them between two frames through [`crate::menus::invoke`] (so UI commands
//! and file dialogs work as in a script); a transcription is started on its own thread instead of being
//! waited for, with a progress bar and a Cancel button, and the sequence is locked against other edits
//! until the entry ends. `extensions.run` through the control channel is synchronous (it returns the
//! report) unless `background: true`. Extension commands can't start other extensions.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use filmcraft_extensions::manifest::CATEGORIES;
use filmcraft_extensions::{Extension, ExtensionStore, Host, Listed, Offer, RunContext, RunReport};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::menus::{MenuItem, invoke};

/// Top-level menu name.
pub const MENU: &str = "Extensions";

/// The extensions of this user: the folder, what loaded, and the manager's state.
#[derive(Default)]
pub struct ExtensionsState {
    store: Option<ExtensionStore>,
    listed: Vec<Listed>,
    /// Bumped whenever the list changes (the native menu is rebuilt on a change).
    pub rev: u64,
    /// The manager dialog is open.
    pub open: bool,
    /// The last run's result.
    pub last: Option<RunReport>,
    /// The last install / remove outcome, shown in the manager.
    pub message: String,
    /// The folder of the extensions shipped with the app.
    bundled_dir: Option<PathBuf>,
    /// Shipped extensions that are not installed (see [`ExtensionStore::offers`]).
    offers: Vec<Offer>,
    /// The "install this extension that starts a program?" question is open.
    pub offer_prompt: bool,
    /// "Plus tard" was chosen: no question again before the next launch.
    offer_snoozed: bool,
    /// The entry running in the background (one at a time).
    running: Option<Running>,
    /// An extension's own command is being served: edits are allowed (they are the extension's).
    serving: bool,
    /// An optional neural model whose licence must be accepted before it is downloaded (id, text):
    /// set when an entry's `ml.install` was refused for lack of `acceptLicense`.
    ml_licence: Option<(String, String)>,
    /// A download started from [`licence_dialog`]: (model id, job id), shown with progress and Cancel.
    ml_download: Option<(String, u64)>,
    /// The optional neural models (both catalogues) and whether they are installed.
    models: Vec<ModelState>,
    /// When `models` was last read (a few seconds between reads: the menu shows the state live).
    models_checked: Option<Instant>,
    /// When the launch question became due: it is shown a moment later, once the project is drawn.
    offer_due: Option<Instant>,
}

/// One optional neural model as the menu and the licence dialog describe it.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelState {
    pub id: String,
    pub name: String,
    pub installed: bool,
    pub bytes: u64,
    /// What FilmCraft uses it for.
    pub use_for: String,
    pub licence: String,
    pub licence_url: String,
    pub source: String,
    pub personal_only: bool,
}

/// How long "Plus tard" puts the launch question off.
const SNOOZE: Duration = Duration::from_secs(7 * 24 * 3600);
/// The launch question waits this long after it becomes due, so the project is drawn first.
const OFFER_DELAY: Duration = Duration::from_millis(1500);
/// The version the snooze is tied to (the workspace version: the app's).
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// `7,9 Mo`, `1,6 Go`.
pub fn size_text(bytes: u64) -> String {
    if bytes >= 1_000_000_000 { format!("{:.1} Go", bytes as f64 / 1e9).replace('.', ",") } else { format!("{:.1} Mo", bytes as f64 / 1e6).replace('.', ",") }
}

/// What the worker thread running an entry asks of the window, and its end.
enum Msg {
    Exec { cmd: String, params: Value, reply: Sender<Result<Value, String>> },
    Done(Result<RunReport, String>),
}

/// An entry running on its own thread: its commands are served by the window between two frames, so
/// the window stays alive while a program or a transcription takes minutes.
struct Running {
    id: String,
    name: String,
    entry: String,
    rx: Receiver<Msg>,
    /// The worker waits for a transcription started for its request.
    waiting: Option<Waiting>,
    /// What the entry is doing now, for the status bar.
    step: String,
    started: Instant,
    /// (fraction 0…1, what) reported by the background step.
    progress: Arc<Mutex<(f32, String)>>,
    cancel: Arc<AtomicBool>,
}

struct Waiting {
    cmd: String,
    params: Value,
    reply: Sender<Result<Value, String>>,
    done: Receiver<Result<filmcraft_engine::transcript::TranscribeOutput, String>>,
}

impl ExtensionsState {
    /// Extensions under `root` (`<data dir>/extensions`); `None` = no data directory (nothing can be
    /// installed, the menu is empty).
    pub fn new(root: Option<PathBuf>) -> Self {
        let mut s = ExtensionsState { store: root.map(ExtensionStore::new), ..Default::default() };
        s.reload();
        s
    }

    pub fn reload(&mut self) {
        self.listed = self.store.as_ref().map(ExtensionStore::list).unwrap_or_default();
        self.rev += 1;
    }

    pub fn root(&self) -> Option<&Path> {
        self.store.as_ref().map(ExtensionStore::root)
    }

    pub fn listed(&self) -> &[Listed] {
        &self.listed
    }

    /// The extensions that loaded, in menu order.
    pub fn loaded(&self) -> impl Iterator<Item = &Extension> {
        self.listed.iter().filter_map(|l| l.result.as_ref().ok())
    }

    pub fn get(&self, id: &str) -> Option<&Extension> {
        self.loaded().find(|e| e.manifest.id == id)
    }

    /// Install the shipped extensions that only drive FilmCraft's commands, and **offer** the ones that
    /// start an external program: those are installed only after the user said yes (the question is
    /// asked in a dialog, and they stay available in *Gérer les extensions…*). See
    /// `ExtensionStore::seed_bundled`.
    pub fn seed_bundled(&mut self, dir: &Path) -> (Vec<String>, Vec<String>) {
        let r = self.store.as_ref().map(|s| s.seed_bundled(dir)).unwrap_or_default();
        self.bundled_dir = Some(dir.to_path_buf());
        self.refresh_offers();
        self.reload();
        r
    }

    /// Re-read the shipped extensions that are not installed; ask about the ones not yet answered,
    /// unless "Plus tard" was chosen in this session or in the last seven days with this version.
    pub fn refresh_offers(&mut self) {
        self.offers = match (&self.store, &self.bundled_dir) {
            (Some(s), Some(d)) => s.offers(d),
            _ => Vec::new(),
        };
        let snoozed = self.offer_snoozed || self.store.as_ref().is_some_and(|s| s.offers_snoozed(now_unix(), APP_VERSION));
        self.offer_prompt = !snoozed && self.offers.iter().any(|o| o.runs_programs && !o.answered);
        if !self.offer_prompt {
            self.offer_due = None;
        }
    }

    /// The optional neural models and their state (see [`refresh_models`]).
    pub fn models(&self) -> &[ModelState] {
        &self.models
    }

    pub fn model(&self, id: &str) -> Option<&ModelState> {
        self.models.iter().find(|m| m.id == id)
    }

    /// Forget the model states: the next frame reads them again (after an install).
    pub fn models_stale(&mut self) {
        self.models_checked = None;
    }

    /// An entry is running in the background.
    pub fn busy(&self) -> bool {
        self.running.is_some()
    }

    /// The window refuses edits that are not the running extension's own.
    pub fn locked(&self) -> bool {
        self.running.is_some() && !self.serving
    }

    /// The shipped extensions waiting for a yes (start a program, not answered yet).
    pub fn pending_offers(&self) -> impl Iterator<Item = &Offer> {
        self.offers.iter().filter(|o| o.runs_programs && !o.answered)
    }

    pub fn offers(&self) -> &[Offer] {
        &self.offers
    }
}

/// The editing context of `app` for an entry.
pub fn run_context(app: &FilmcraftApp) -> RunContext {
    let mut c = RunContext::new(Host::App);
    c.project = app.session.path.clone();
    c.selection = app.session.state.selection.iter().map(|id| id.0).collect();
    c.playhead_ticks = app.session.playhead().0;
    c.playhead_seconds = app.session.playhead().seconds();
    c.has_sequence = app.session.active_sequence().is_some();
    c.data_dir = app.extensions.root().and_then(Path::parent).map(Path::to_path_buf);
    c
}

/// `ext:<extension id>:<entry number>`.
fn item_id(ext: &str, entry: usize) -> String {
    format!("ext:{ext}:{entry}")
}

/// Read the state of the optional neural models from both catalogues (`models.list`: voice clean-up
/// and auto-reframe; `ml.models`: the `ml` feature). Cheap (file sizes), done every few seconds so the
/// menu follows an install made elsewhere; the menu is rebuilt when something changed.
pub fn refresh_models(app: &mut FilmcraftApp) {
    if app.extensions.models_checked.is_some_and(|t| t.elapsed() < Duration::from_secs(3)) {
        return;
    }
    app.extensions.models_checked = Some(Instant::now());
    let mut out = Vec::new();
    if let Ok(v) = app.session.execute("models.list", json!({})) {
        for m in v["models"].as_array().into_iter().flatten() {
            out.push(ModelState {
                id: m["id"].as_str().unwrap_or_default().to_string(),
                name: m["name"].as_str().unwrap_or_default().to_string(),
                installed: m["installed"].as_bool().unwrap_or(false),
                bytes: m["size"].as_u64().unwrap_or(0),
                use_for: m["task"].as_str().unwrap_or_default().to_string(),
                licence: m["license"].as_str().unwrap_or_default().to_string(),
                licence_url: m["licenseUrl"].as_str().unwrap_or_default().to_string(),
                source: m["source"].as_str().unwrap_or_default().to_string(),
                personal_only: false,
            });
        }
    }
    if let Ok(v) = app.session.execute("ml.models", json!({})) {
        for m in v["models"].as_array().into_iter().flatten() {
            out.push(ModelState {
                id: m["id"].as_str().unwrap_or_default().to_string(),
                name: m["title"].as_str().unwrap_or_default().to_string(),
                installed: m["installed"].as_bool().unwrap_or(false),
                bytes: m["bytes"].as_u64().unwrap_or(0),
                use_for: m["use"].as_str().unwrap_or_default().to_string(),
                licence: m["licence"].as_str().unwrap_or_default().to_string(),
                licence_url: m["licenceUrl"].as_str().unwrap_or_default().to_string(),
                source: m["source"].as_str().unwrap_or_default().to_string(),
                personal_only: m["personalOnly"].as_bool().unwrap_or(false),
            });
        }
    }
    if out != app.extensions.models {
        app.extensions.models = out;
        app.extensions.rev += 1;
    }
}

/// `DeepFilterNet3 ✓ installé` / `RIFE v4 ↓ à télécharger (46,5 Mo)`; an id missing from both
/// catalogues (this build has no `ml` feature) is said so.
fn model_line(state: &ExtensionsState, id: &str) -> String {
    match state.model(id) {
        Some(m) if m.installed => format!("{} ✓ installé", m.name),
        Some(m) => format!("{} ↓ à télécharger ({})", m.name, size_text(m.bytes)),
        None => format!("{id} : absent de cette version"),
    }
}

/// What follows a single-entry extension's name: ` (modèle ✓)` or ` (modèle ↓ à télécharger)`.
fn model_suffix(state: &ExtensionsState, models: &[String]) -> String {
    if models.is_empty() {
        return String::new();
    }
    let missing: Vec<&ModelState> = models.iter().filter_map(|id| state.model(id)).filter(|m| !m.installed).collect();
    let unknown = models.iter().any(|id| state.model(id).is_none());
    if unknown && missing.is_empty() {
        " (modèle absent de cette version)".into()
    } else if missing.is_empty() {
        " (modèle ✓)".into()
    } else {
        format!(" (modèle ↓ à télécharger, {})", size_text(missing.iter().map(|m| m.bytes).sum()))
    }
}

/// The group heading an extension files under: (index in [`CATEGORIES`] or its length for "Autres").
fn group_of(category: &str) -> usize {
    CATEGORIES.iter().position(|(id, _)| *id == category).unwrap_or(CATEGORIES.len())
}

/// Add the Extensions menu to the menu tree: the groups of [`CATEGORIES`] in order (plus "Autres" for
/// an extension without a known category), each with a disabled heading, a separator above it and its
/// extensions in alphabetical order; the management items close the Gestion group.
pub fn menu(app: &FilmcraftApp, items: &mut Vec<MenuItem>) {
    let ctx = run_context(app);
    let state = &app.extensions;
    let item = |id: String, label: String, path: Vec<String>, enabled: bool, separator_before: bool| MenuItem {
        id,
        label,
        path,
        shortcut: None,
        enabled,
        checked: None,
        separator_before,
    };
    let mut groups: Vec<Vec<&Extension>> = vec![Vec::new(); CATEGORIES.len() + 1];
    for e in state.loaded() {
        groups[group_of(&e.manifest.category)].push(e);
    }
    for g in &mut groups {
        g.sort_by_key(|e| e.manifest.name.to_lowercase());
    }
    let management = CATEGORIES.len() - 1;
    let others = CATEGORIES.len();
    // menu order: the categories but Gestion, then Autres, then Gestion (which holds the management items)
    let order: Vec<usize> = (0..management).chain([others, management]).collect();
    let mut first = true;
    for n in order {
        let exts = &groups[n];
        if exts.is_empty() && n != management {
            continue;
        }
        let (gid, label) = CATEGORIES.get(n).copied().unwrap_or(("other", "Autres"));
        items.push(item(format!("ext.group.{gid}"), label.to_string(), vec![MENU.into()], false, !first));
        first = false;
        for e in exts {
            let m = &e.manifest;
            if let [only] = m.entries.as_slice() {
                let label = format!("{}{}", m.name, model_suffix(state, &m.models));
                items.push(item(item_id(&m.id, 0), label, vec![MENU.into()], ctx.satisfies(&only.requires).is_ok(), false));
            } else {
                for (k, id) in m.models.iter().enumerate() {
                    items.push(item(format!("ext.model.{}.{k}", m.id), model_line(state, id), vec![MENU.into(), m.name.clone()], false, false));
                }
                for (k, en) in m.entries.iter().enumerate() {
                    let sep = k == 0 && !m.models.is_empty();
                    items.push(item(item_id(&m.id, k), en.label.clone(), vec![MENU.into(), m.name.clone()], ctx.satisfies(&en.requires).is_ok(), sep));
                }
            }
        }
    }
    let installable = state.store.is_some();
    items.push(item("extensions.install".into(), "Installer une extension…".into(), vec![MENU.into()], installable, false));
    items.push(item("extensions.manage".into(), "Gérer les extensions…".into(), vec![MENU.into()], true, false));
    items.push(item("extensions.reload".into(), "Recharger les extensions".into(), vec![MENU.into()], installable, false));
}

/// Commands of this module (None = not mine).
pub fn route(app: &mut FilmcraftApp, ctx: &egui::Context, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if let Some(r) = crate::panels::model_consent::route(id, params) {
        return Some(r);
    }
    let r = if let Some(rest) = id.strip_prefix("ext:") {
        let Some((ext, n)) = rest.rsplit_once(':') else { return Some(Err(format!("bad extension item `{id}`"))) };
        run(app, ctx, ext, &json!(n.parse::<usize>().unwrap_or(usize::MAX)), params.get("vars"), true)
    } else {
        match id {
            "extensions.list" => Ok(list(app)),
            "extensions.run" => match params.get("extension").and_then(Value::as_str) {
                Some(ext) => run(
                    app,
                    ctx,
                    ext,
                    params.get("entry").unwrap_or(&json!(0)),
                    params.get("vars"),
                    params.get("background").and_then(Value::as_bool).unwrap_or(false),
                ),
                None => Err("extensions.run needs `extension` (the extension id)".into()),
            },
            "extensions.cancel" => Ok(json!({"cancelled": cancel(app)})),
            "extensions.status" => Ok(status_json(app)),
            "extensions.install" => install(app, params),
            "extensions.offers" => Ok(offers_json(app)),
            "extensions.installBundled" => match params.get("id").and_then(Value::as_str) {
                Some(x) => install_bundled(app, x),
                None => Err("extensions.installBundled needs `id`".into()),
            },
            "extensions.declineBundled" => match params.get("id").and_then(Value::as_str) {
                Some(x) => decline_bundled(app, x),
                None => Err("extensions.declineBundled needs `id`".into()),
            },
            "extensions.remove" => match params.get("id").and_then(Value::as_str) {
                Some(x) => remove(app, x),
                None => Err("extensions.remove needs `id`".into()),
            },
            "extensions.reload" => {
                app.extensions.reload();
                Ok(list(app))
            }
            "extensions.manage" => {
                app.extensions.open = true;
                Ok(json!({"dialog": "extensions"}))
            }
            _ => return None,
        }
    };
    if let Err(e) = &r {
        app.ui.status = e.clone();
    }
    ctx.request_repaint();
    Some(r)
}

fn list(app: &FilmcraftApp) -> Value {
    let context = run_context(app);
    let l: Vec<Value> = app
        .extensions
        .listed()
        .iter()
        .map(|l| match &l.result {
            Ok(e) => json!({
                "id": e.manifest.id, "name": e.manifest.name, "version": e.manifest.version, "description": e.manifest.description,
                "dir": e.dir, "runsPrograms": e.manifest.runs_programs(),
                "entries": e.manifest.entries.iter().enumerate().map(|(n, en)| json!({
                    "index": n, "label": en.label, "item": item_id(&e.manifest.id, n), "enabled": context.satisfies(&en.requires).is_ok(),
                })).collect::<Vec<_>>(),
            }),
            Err(why) => json!({"id": l.dir_name, "error": why}),
        })
        .collect();
    json!({"root": app.extensions.root(), "extensions": l})
}

/// Run an entry. `background`: on its own thread, served between frames (menu, dialog); the result
/// comes later (`extensions.status`, the status bar, `last`). Otherwise it runs here and now and the
/// report is the return value (automation, scripts), which keeps the window busy meanwhile.
fn run(app: &mut FilmcraftApp, ctx: &egui::Context, ext_id: &str, entry: &Value, vars: Option<&Value>, background: bool) -> Result<Value, String> {
    if app.extensions.busy() {
        let r = app.extensions.running.as_ref().map(|r| format!("« {} » est en cours : attendez la fin ou annulez-la", r.name)).unwrap_or_default();
        return Err(r);
    }
    let ext = app.extensions.get(ext_id).cloned().ok_or_else(|| format!("extension `{ext_id}` is not installed"))?;
    let index = match entry {
        Value::Number(n) => n.as_u64().and_then(|n| usize::try_from(n).ok()).unwrap_or(usize::MAX),
        Value::String(s) => match s.parse::<usize>() {
            Ok(n) => n,
            Err(_) => ext.manifest.entries.iter().position(|e| e.label.eq_ignore_ascii_case(s)).ok_or_else(|| format!("`{ext_id}` has no entry `{s}`"))?,
        },
        _ => 0,
    };
    let mut rc = run_context(app);
    if let Some(Value::Object(m)) = vars {
        rc.vars = m.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    }
    if background {
        return start_background(app, ctx, ext, index, rc);
    }
    app.ui.status = format!("{}: running…", ext.manifest.name);
    app.extensions.serving = true;
    let report = filmcraft_extensions::run_entry(&ext, index, &rc, &mut |cmd, params| {
        if cmd.starts_with("extensions.") || cmd.starts_with("ext:") {
            return Err("extensions can't run extensions".into());
        }
        invoke(app, ctx, cmd, params)
    })
    .map_err(|e| e.to_string());
    app.extensions.serving = false;
    let report = report?;
    app.ui.status = report.summary();
    let value = serde_json::to_value(&report).unwrap_or_default();
    // the caller gets the full reason (step number and command), the status bar the short line
    let failure = report.error.as_ref().map(|e| format!("{}: {e}", report.entry));
    if let Some(e) = &failure {
        capture_licence(app, e);
    }
    app.extensions.last = Some(report);
    match failure {
        Some(e) => Err(e),
        None => Ok(value),
    }
}

/// Start `ext`'s entry on a worker thread (see [`Running`]).
fn start_background(app: &mut FilmcraftApp, ctx: &egui::Context, ext: Extension, index: usize, rc: RunContext) -> Result<Value, String> {
    let entry = ext.manifest.entries.get(index).map(|e| e.label.clone()).ok_or_else(|| format!("`{}` has no entry number {index}", ext.manifest.id))?;
    // a missing requirement is refused here, like the synchronous run, not inside the thread
    rc.satisfies(&ext.manifest.entries[index].requires).map_err(|m| format!("{entry}: {m}"))?;
    let (tx, rx) = mpsc::channel::<Msg>();
    let (id, name) = (ext.manifest.id.clone(), ext.manifest.name.clone());
    let repaint = ctx.clone();
    std::thread::Builder::new()
        .name("filmcraft-extension".into())
        .spawn(move || {
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                filmcraft_extensions::run_entry(&ext, index, &rc, &mut |cmd, params| {
                    let (reply, answer) = mpsc::channel();
                    if tx.send(Msg::Exec { cmd: cmd.to_string(), params, reply }).is_err() {
                        return Err("the window was closed".into());
                    }
                    repaint.request_repaint();
                    answer.recv().unwrap_or_else(|_| Err("the window was closed".into()))
                })
                .map_err(|e| e.to_string())
            }))
            .unwrap_or_else(|_| Err("internal error in the extension runner".into()));
            let _ = tx.send(Msg::Done(res));
            repaint.request_repaint();
        })
        .map_err(|e| format!("can't start the extension thread: {e}"))?;
    app.extensions.running = Some(Running {
        id,
        name: name.clone(),
        entry: entry.clone(),
        rx,
        waiting: None,
        step: String::new(),
        started: Instant::now(),
        progress: Arc::new(Mutex::new((0.0, String::new()))),
        cancel: Arc::new(AtomicBool::new(false)),
    });
    app.ui.status = format!("{name} · {entry}: en cours…");
    ctx.request_repaint();
    Ok(json!({"started": true, "extension": app.extensions.running.as_ref().map(|r| r.id.clone()), "entry": entry}))
}

/// Ask the running entry to stop: a transcription in progress ends at its next progress report, and no
/// further command is started.
fn cancel(app: &mut FilmcraftApp) -> bool {
    match &app.extensions.running {
        Some(r) => {
            r.cancel.store(true, Ordering::Relaxed);
            app.ui.status = format!("{} · {}: annulation…", r.name, r.entry);
            true
        }
        None => false,
    }
}

fn status_json(app: &FilmcraftApp) -> Value {
    match &app.extensions.running {
        Some(r) => {
            let (f, what) = r.progress.lock().unwrap_or_else(PoisonError::into_inner).clone();
            json!({"running": true, "extension": r.id, "entry": r.entry, "step": r.step, "progress": f, "what": what, "seconds": r.started.elapsed().as_secs_f64(), "waiting": r.waiting.is_some()})
        }
        None => json!({"running": false, "last": app.extensions.last.as_ref().map(|r| json!({"ok": r.ok(), "summary": r.summary()}))}),
    }
}

/// Serve the background entry: run the commands its thread asks for (one at a time, on this thread, so
/// UI commands and file dialogs work), start a transcription on a worker instead of waiting for it, and
/// collect the final report. Called every frame.
pub fn pump(app: &mut FilmcraftApp, ctx: &egui::Context) {
    if app.extensions.running.is_none() {
        return;
    }
    ctx.request_repaint_after(Duration::from_millis(100));
    // a transcription finished?
    let waiting = app.extensions.running.as_mut().and_then(|r| r.waiting.take());
    if let Some(w) = waiting {
        match w.done.try_recv() {
            Ok(Ok(out)) => {
                // the command runs now and finds the recognised speech instead of recognising it again
                app.session.finished_transcription = Some(out);
                let r = serve(app, ctx, &w.cmd, w.params);
                app.session.finished_transcription = None;
                let _ = w.reply.send(r);
            }
            Ok(Err(e)) => {
                let _ = w.reply.send(Err(e));
            }
            Err(TryRecvError::Empty) => {
                if let Some(r) = app.extensions.running.as_mut() {
                    r.waiting = Some(w);
                }
            }
            Err(TryRecvError::Disconnected) => {
                let _ = w.reply.send(Err("la transcription s'est arrêtée de façon inattendue".into()));
            }
        }
    }
    for _ in 0..64 {
        let Some(run) = app.extensions.running.as_mut() else { break };
        if run.waiting.is_some() {
            break;
        }
        match run.rx.try_recv() {
            Ok(Msg::Exec { cmd, params, reply }) => {
                if run.cancel.load(Ordering::Relaxed) {
                    let _ = reply.send(Err("annulé".into()));
                    continue;
                }
                run.step = cmd.clone();
                begin_step(app, ctx, cmd, params, reply);
            }
            Ok(Msg::Done(res)) => {
                finish(app, res);
                break;
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                finish(app, Err("l'extension s'est arrêtée de façon inattendue".into()));
                break;
            }
        }
    }
    if let Some(r) = app.extensions.running.as_ref() {
        let (f, what) = r.progress.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let pct = if r.waiting.is_some() { format!(" {:.0} %", f * 100.0) } else { String::new() };
        let what = if r.waiting.is_some() && !what.is_empty() { format!(" ({what})") } else { String::new() };
        app.ui.status = format!("{} · {}: {}{pct}{what}", r.name, r.entry, if r.step.is_empty() { "en cours" } else { step_label(&r.step) });
    }
}

fn step_label(cmd: &str) -> &str {
    match cmd {
        "sequence.transcribe" | "transcript.generate" => "transcription…",
        "sequence.removeSilence" => "recherche des silences…",
        "transcript.removeFillers" => "suppression des hésitations…",
        "sequence.markBeats" | "sequence.cutOnBeats" => "analyse des beats…",
        "export.quick" => "export…",
        _ => "en cours…",
    }
}

/// Run `cmd` for the thread, or start it on a worker when it is a transcription.
fn begin_step(app: &mut FilmcraftApp, ctx: &egui::Context, cmd: String, params: Value, reply: Sender<Result<Value, String>>) {
    if cmd.starts_with("extensions.") || cmd.starts_with("ext:") {
        let _ = reply.send(Err("extensions can't run extensions".into()));
        return;
    }
    if let Some(Ok(generate_params)) = filmcraft_engine::transcript::async_transcribe_params(&app.session, &cmd, &params)
        && let Ok(job) = filmcraft_engine::transcript::begin_transcribe(&app.session, &generate_params)
        && app.session.is_enabled("transcript.generate")
    {
        let Some(run) = app.extensions.running.as_mut() else { return };
        let (progress, cancel) = (run.progress.clone(), run.cancel.clone());
        *progress.lock().unwrap_or_else(PoisonError::into_inner) = (0.0, String::new());
        let (tx, done) = mpsc::channel();
        let repaint = ctx.clone();
        let spawned = std::thread::Builder::new().name("filmcraft-transcribe".into()).spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                job.run(&mut |f, what| {
                    *progress.lock().unwrap_or_else(PoisonError::into_inner) = (f, what.to_string());
                    repaint.request_repaint_after(Duration::from_millis(50));
                    !cancel.load(Ordering::Relaxed)
                })
                .map_err(|e| e.to_string())
            }))
            .unwrap_or_else(|_| Err("erreur interne pendant la transcription".into()));
            let _ = tx.send(r);
            repaint.request_repaint();
        });
        match spawned {
            Ok(_) => run.waiting = Some(Waiting { cmd, params, reply, done }),
            Err(e) => {
                let _ = reply.send(Err(format!("can't start the transcription thread: {e}")));
            }
        }
        return;
    }
    let r = serve(app, ctx, &cmd, params);
    let _ = reply.send(r);
}

/// Execute an extension's command in the window (its edits are allowed while the window is locked).
fn serve(app: &mut FilmcraftApp, ctx: &egui::Context, cmd: &str, params: Value) -> Result<Value, String> {
    app.extensions.serving = true;
    let r = invoke(app, ctx, cmd, params);
    app.extensions.serving = false;
    r
}

fn finish(app: &mut FilmcraftApp, res: Result<RunReport, String>) {
    let Some(run) = app.extensions.running.take() else { return };
    match res {
        Ok(report) => {
            app.ui.status = report.summary();
            if let Some(e) = report.error.clone() {
                capture_licence(app, &e);
            }
            app.extensions.last = Some(report);
        }
        Err(e) => app.ui.status = format!("{}: {e}", run.entry),
    }
}

/// An `ml.install` refused without `acceptLicense` carries `ML_LICENCE[<model id>] <text>`: remember it so the
/// window can ask.
fn capture_licence(app: &mut FilmcraftApp, error: &str) {
    if let Some(i) = error.find("ML_LICENCE[") {
        let rest = &error[i + "ML_LICENCE[".len()..];
        if let Some((id, text)) = rest.split_once(']') {
            app.extensions.ml_licence = Some((id.to_string(), text.trim().to_string()));
        }
    }
}

/// "Plus tard": no launch question for seven days with this version (and none in this session).
fn snooze(app: &mut FilmcraftApp) {
    app.extensions.offer_snoozed = true;
    app.extensions.offer_prompt = false;
    app.extensions.offer_due = None;
    if let Some(store) = &app.extensions.store
        && let Err(e) = store.snooze_offers(now_unix() + SNOOZE.as_secs(), APP_VERSION)
    {
        app.ui.status = format!("« Plus tard » non mémorisé : {e}");
    }
}

/// One dialog for an optional neural model of the `ml` catalogue, asked before its download
/// (automation ids `ml.licence.*`): what it does, download size, licence in one line with its link, the
/// source, an Install button; then a small progress window with Cancel while the job runs
/// (`ml.download.cancel`). The `models.*` catalogue has the same dialog in `model_consent`.
pub fn licence_dialog(app: &mut FilmcraftApp, ctx: &egui::Context) {
    download_window(app, ctx);
    let Some((id, text)) = app.extensions.ml_licence.clone() else { return };
    let model = app.extensions.model(&id).cloned();
    let mut elems: Vec<(String, egui::Rect, String)> = Vec::new();
    let mut accept = false;
    let mut cancel = false;
    let title = model.as_ref().map(|m| m.name.clone()).unwrap_or_else(|| id.clone());
    egui::Window::new("Modèle optionnel").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).default_width(520.0).show(
        ctx,
        |ui| {
            ui.strong(format!("{title} n'est pas installé."));
            match &model {
                Some(m) => {
                    if !m.use_for.is_empty() {
                        ui.label(&m.use_for);
                    }
                    ui.add_space(4.0);
                    ui.label(format!("À télécharger : {}", size_text(m.bytes)));
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!("Licence : {} ·", m.licence));
                        ui.hyperlink_to("texte de la licence", &m.licence_url);
                    });
                    if m.personal_only {
                        ui.colored_label(ui.visuals().warn_fg_color, "Usage personnel / non commercial uniquement.");
                    }
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Source :");
                        ui.hyperlink_to(&m.source, &m.source);
                    });
                }
                None => {
                    ui.label(&text);
                }
            }
            ui.label("Téléchargé depuis son éditeur, vérifié (SHA-256), puis exécuté sur votre ordinateur : rien n'est envoyé.");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let r = ui.button("J'ai lu la licence : installer");
                elems.push((format!("ml.licence.accept.{id}"), r.rect, "Accept".into()));
                accept = r.clicked();
                let r = ui.button("Annuler");
                elems.push(("ml.licence.cancel".into(), r.rect, "Cancel".into()));
                cancel = r.clicked();
            });
        },
    );
    for (eid, r, l) in elems {
        app.auto.add(&eid, r, &l);
    }
    if accept {
        app.extensions.ml_licence = None;
        match invoke(app, ctx, "ml.install", json!({"model": id, "acceptLicense": true, "wait": false})) {
            Ok(v) => {
                app.ui.status = v["summary"].as_str().unwrap_or("téléchargement lancé").to_string();
                if let Some(job) = v["job"].as_u64() {
                    app.extensions.ml_download = Some((id, job));
                }
            }
            Err(e) => app.ui.status = e,
        }
    } else if cancel {
        app.extensions.ml_licence = None;
    }
}

/// The download started by [`licence_dialog`]: bytes done / total, a Cancel button; the status bar
/// says the outcome when it ends (automation ids `ml.download.*`).
fn download_window(app: &mut FilmcraftApp, ctx: &egui::Context) {
    use std::sync::atomic::Ordering;
    let Some((id, jid)) = app.extensions.ml_download.clone() else { return };
    let Some(job) = app.session.jobs.iter().find(|j| j.id == jid).cloned() else {
        app.extensions.ml_download = None;
        return;
    };
    let name = app.extensions.model(&id).map(|m| m.name.clone()).unwrap_or_else(|| id.clone());
    if job.progress.finished.load(Ordering::Relaxed) {
        let err = job.progress.error.lock().unwrap_or_else(PoisonError::into_inner).clone();
        app.ui.status = match err {
            None if job.progress.cancel.load(Ordering::Relaxed) => format!("Téléchargement de {name} annulé"),
            None => format!("{name} installé"),
            Some(e) => format!("Téléchargement de {name} impossible : {e}"),
        };
        app.extensions.ml_download = None;
        app.extensions.models_stale();
        return;
    }
    let (d, t) = (job.progress.done.load(Ordering::Relaxed), job.progress.total.load(Ordering::Relaxed).max(1));
    let mut elems: Vec<(String, egui::Rect, String)> = Vec::new();
    egui::Window::new("Téléchargement du modèle")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -48.0])
        .default_width(380.0)
        .show(ctx, |ui| {
            ui.label(format!("{name} : {} / {}", size_text(d), size_text(t)));
            let bar = ui.add(egui::ProgressBar::new((d as f32 / t as f32).clamp(0.0, 1.0)).show_percentage());
            elems.push(("ml.download.progress".into(), bar.rect, "Progress".into()));
            let cancelling = job.progress.cancel.load(Ordering::Relaxed);
            let r = ui.add_enabled(!cancelling, egui::Button::new(if cancelling { "Annulation…" } else { "Annuler" }));
            elems.push(("ml.download.cancel".into(), r.rect, "Cancel".into()));
            if r.clicked() {
                job.progress.cancel.store(true, Ordering::Relaxed);
            }
        });
    for (eid, r, l) in elems {
        app.auto.add(&eid, r, &l);
    }
    ctx.request_repaint_after(Duration::from_millis(100));
}

/// The small window shown while an entry runs: what it is doing, the progress of a transcription and a
/// Cancel button (automation ids `extensions.running.*`).
pub fn running_window(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(r) = app.extensions.running.as_ref() else { return };
    let (f, what) = r.progress.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let (title, step, waiting, secs) =
        (format!("{} · {}", r.name, r.entry), step_label(&r.step).to_string(), r.waiting.is_some(), r.started.elapsed().as_secs());
    let cancelling = r.cancel.load(Ordering::Relaxed);
    let mut elems: Vec<(String, egui::Rect, String)> = Vec::new();
    let mut clicked = false;
    egui::Window::new("Extension en cours").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_BOTTOM, [0.0, -48.0]).default_width(380.0).show(
        ctx,
        |ui| {
            ui.strong(&title);
            ui.label(format!("{step}  ({secs} s)"));
            if waiting {
                let bar = ui.add(egui::ProgressBar::new(f.clamp(0.0, 1.0)).show_percentage());
                elems.push(("extensions.running.progress".into(), bar.rect, "Progress".into()));
                if !what.is_empty() {
                    ui.label(what);
                }
            } else {
                ui.add(egui::ProgressBar::new(0.0).animate(true));
            }
            ui.label("La fenêtre reste utilisable, mais la séquence est verrouillée jusqu'à la fin.");
            let b = ui.add_enabled(!cancelling, egui::Button::new(if cancelling { "Annulation…" } else { "Annuler" }));
            elems.push(("extensions.running.cancel".into(), b.rect, "Cancel".into()));
            clicked = b.clicked();
        },
    );
    for (id, rect, l) in elems {
        app.auto.add(&id, rect, &l);
    }
    if clicked {
        cancel(app);
    }
}

fn install(app: &mut FilmcraftApp, params: &Value) -> Result<Value, String> {
    if app.extensions.store.is_none() {
        return Err("there is no data directory to install extensions into".into());
    }
    let path = match params.get("path").and_then(Value::as_str) {
        Some(p) => p.to_string(),
        None => {
            let picked = if params.get("folder").and_then(Value::as_bool) == Some(true) {
                app.hooks.pick_folder.as_mut().ok_or("no folder picker here; pass `path`")?()
            } else {
                app.hooks.pick_open_file.as_mut().ok_or("no file picker here; pass `path`")?("FilmCraft extension", &["fcext", "zip"])
            };
            match picked {
                Some(p) => p,
                None => return Ok(Value::Null), // cancelled
            }
        }
    };
    let store = app.extensions.store.clone().ok_or("there is no data directory to install extensions into")?;
    let r = store.install(Path::new(&path)).map_err(|e| e.to_string());
    app.extensions.message = match &r {
        Ok(r) => {
            let verb = if r.replaced.is_some() { "Updated" } else { "Installed" };
            let warn = if r.runs_programs { " (it starts external programs when run)" } else { "" };
            format!("{verb} {} {}{warn}", r.name, r.version)
        }
        Err(e) => format!("Not installed: {e}"),
    };
    app.ui.status = app.extensions.message.clone();
    app.extensions.reload();
    r.map(|r| serde_json::to_value(r).unwrap_or_default())
}

fn offers_json(app: &FilmcraftApp) -> Value {
    json!({"offers": app.extensions.offers(), "asking": app.extensions.offer_prompt})
}

/// Install a shipped extension the user chose to install (it may start programs: the dialogs say so
/// before this is called).
fn install_bundled(app: &mut FilmcraftApp, id: &str) -> Result<Value, String> {
    let store = app.extensions.store.clone().ok_or("there is no data directory to install extensions into")?;
    let offer =
        app.extensions.offers.iter().find(|o| o.id == id).cloned().ok_or_else(|| format!("`{id}` is not a shipped extension waiting to be installed"))?;
    let r = store.install_offer(&offer.dir).map_err(|e| e.to_string());
    app.extensions.message = match &r {
        Ok(r) => format!("Installée : {} {}{}", r.name, r.version, if r.runs_programs { " (elle lance des programmes quand vous l'exécutez)" } else { "" }),
        Err(e) => format!("Non installée : {e}"),
    };
    app.ui.status = app.extensions.message.clone();
    app.extensions.reload();
    app.extensions.refresh_offers();
    r.map(|r| serde_json::to_value(r).unwrap_or_default())
}

/// "Ne plus proposer": remembered, nothing installed (the manager still lists it).
fn decline_bundled(app: &mut FilmcraftApp, id: &str) -> Result<Value, String> {
    let store = app.extensions.store.clone().ok_or("there is no data directory")?;
    store.decline_offer(id).map_err(|e| e.to_string())?;
    app.extensions.message = format!("{id} ne sera plus proposée (elle reste installable depuis « Gérer les extensions… »).");
    app.ui.status = app.extensions.message.clone();
    app.extensions.refresh_offers();
    Ok(json!({"declined": id}))
}

fn remove(app: &mut FilmcraftApp, id: &str) -> Result<Value, String> {
    let store = app.extensions.store.clone().ok_or("there is no data directory")?;
    let r = store.remove(id).map_err(|e| e.to_string());
    app.extensions.message = match &r {
        Ok(()) => format!("Removed {id}"),
        Err(e) => e.clone(),
    };
    app.ui.status = app.extensions.message.clone();
    app.extensions.reload();
    app.extensions.refresh_offers();
    r.map(|()| json!({"removed": id}))
}

enum Act {
    Run(String, usize),
    InstallBundled(String),
    DeclineBundled(String),
    Snooze,
    Remove(String),
    Install { folder: bool },
    Reload,
    Reveal,
    Close,
}

/// Gérer les extensions… (automation ids `extensions.dialog.*`).
pub fn dialog(app: &mut FilmcraftApp, ctx: &egui::Context) {
    if !app.extensions.open {
        return;
    }
    let listed = app.extensions.listed.clone();
    let last = app.extensions.last.clone();
    let message = app.extensions.message.clone();
    let offers = app.extensions.offers.clone();
    let root = app.extensions.root().map(|p| p.display().to_string());
    let context = run_context(app);
    let mut elems: Vec<(String, egui::Rect, String)> = Vec::new();
    let mut push = |id: String, r: &egui::Response, label: &str| elems.push((id, r.rect, label.to_string()));
    let mut act: Option<Act> = None;
    let mut open = true;
    egui::Window::new("Gérer les extensions")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(560.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(root.as_deref().map_or_else(|| "No data directory: extensions can't be installed.".to_string(), |r| format!("Dossier : {r}")));
            ui.horizontal_wrapped(|ui| {
                let r = ui.button("Installer un fichier .zip / .fcext…");
                push("extensions.dialog.installFile".into(), &r, "Install from file");
                if r.clicked() {
                    act = Some(Act::Install { folder: false });
                }
                let r = ui.button("Installer un dossier…");
                push("extensions.dialog.installFolder".into(), &r, "Install from folder");
                if r.clicked() {
                    act = Some(Act::Install { folder: true });
                }
                let r = ui.button("Recharger");
                push("extensions.dialog.reload".into(), &r, "Reload");
                if r.clicked() {
                    act = Some(Act::Reload);
                }
                let r = ui.button("Ouvrir le dossier");
                push("extensions.dialog.reveal".into(), &r, "Reveal folder");
                if r.clicked() {
                    act = Some(Act::Reveal);
                }
            });
            if !message.is_empty() {
                ui.label(&message);
            }
            ui.separator();
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                if listed.is_empty() {
                    ui.label("Aucune extension installée.");
                }
                for l in &listed {
                    match &l.result {
                        Ok(e) => {
                            let m = &e.manifest;
                            ui.horizontal(|ui| {
                                ui.strong(&m.name);
                                ui.label(format!("{}  ·  {}", m.version, m.id));
                                let r = ui.button("Supprimer");
                                push(format!("extensions.dialog.remove.{}", m.id), &r, "Remove");
                                if r.clicked() {
                                    act = Some(Act::Remove(m.id.clone()));
                                }
                            });
                            if !m.description.is_empty() {
                                ui.label(&m.description);
                            }
                            if m.runs_programs() {
                                ui.colored_label(ui.visuals().warn_fg_color, "Lance des programmes externes : ne gardez que les extensions de confiance.");
                            }
                            ui.horizontal_wrapped(|ui| {
                                for (n, en) in m.entries.iter().enumerate() {
                                    let ok = context.satisfies(&en.requires);
                                    let r = ui.add_enabled(ok.is_ok(), egui::Button::new(&en.label));
                                    let r = match &ok {
                                        Err(why) => r.on_disabled_hover_text(why.clone()),
                                        Ok(()) if !en.description.is_empty() => r.on_hover_text(&en.description),
                                        Ok(()) => r,
                                    };
                                    push(format!("extensions.dialog.run.{}.{n}", m.id), &r, &en.label);
                                    if r.clicked() {
                                        act = Some(Act::Run(m.id.clone(), n));
                                    }
                                }
                            });
                        }
                        Err(why) => {
                            ui.horizontal(|ui| {
                                ui.colored_label(ui.visuals().error_fg_color, format!("{} : {why}", l.dir_name));
                                let r = ui.button("Supprimer");
                                push(format!("extensions.dialog.remove.{}", l.dir_name), &r, "Remove");
                                if r.clicked() {
                                    act = Some(Act::Remove(l.dir_name.clone()));
                                }
                            });
                        }
                    }
                    ui.separator();
                }
            });
            if !offers.is_empty() {
                ui.strong("Fournies avec FilmCraft, pas installées");
                for o in &offers {
                    ui.horizontal(|ui| {
                        ui.strong(&o.name);
                        ui.label(format!("{}  ·  {}", o.version, o.id));
                        let r = ui.button(if o.runs_programs { "Installer (lance un programme)…" } else { "Installer" });
                        push(format!("extensions.dialog.offer.install.{}", o.id), &r, "Install shipped extension");
                        if r.clicked() {
                            act = Some(Act::InstallBundled(o.id.clone()));
                        }
                    });
                    if !o.description.is_empty() {
                        ui.label(&o.description);
                    }
                    if o.runs_programs {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            format!(
                                "Lance des programmes de votre ordinateur ({}) quand vous l'exécutez : n'installez que ce en quoi vous avez confiance.",
                                o.programs.join(", ")
                            ),
                        );
                    }
                    ui.separator();
                }
            }
            if let Some(r) = &last {
                ui.label(egui::RichText::new(r.summary()).strong());
                for line in r.log.iter().take(8) {
                    ui.label(line);
                }
            }
            ui.horizontal(|ui| {
                let r = ui.button("Fermer");
                push("extensions.dialog.close".into(), &r, "Close");
                if r.clicked() {
                    act = Some(Act::Close);
                }
            });
        });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.extensions.open = false;
    }
    match act {
        Some(Act::Run(id, n)) => {
            let _ = route(app, ctx, "extensions.run", &json!({"extension": id, "entry": n}));
        }
        Some(Act::Remove(id)) => {
            let _ = route(app, ctx, "extensions.remove", &json!({"id": id}));
        }
        Some(Act::InstallBundled(id)) => {
            let _ = route(app, ctx, "extensions.installBundled", &json!({"id": id}));
        }
        Some(Act::DeclineBundled(id)) => {
            let _ = route(app, ctx, "extensions.declineBundled", &json!({"id": id}));
        }
        Some(Act::Snooze) => snooze(app),
        Some(Act::Install { folder }) => {
            let _ = route(app, ctx, "extensions.install", &json!({"folder": folder}));
        }
        Some(Act::Reload) => {
            let _ = route(app, ctx, "extensions.reload", &json!({}));
        }
        Some(Act::Reveal) => {
            if let (Some(root), Some(open)) = (app.extensions.root().map(|p| p.to_string_lossy().to_string()), app.hooks.open_path.as_mut()) {
                let _ = std::fs::create_dir_all(&root);
                if let Err(e) = open(&root, false) {
                    app.ui.status = e;
                }
            }
        }
        Some(Act::Close) => app.extensions.open = false,
        None => {}
    }
}

/// The question asked at launch about a shipped extension that starts a program: nothing is installed
/// until the user answers yes (automation ids `extensions.offer.*`).
pub fn offers_dialog(app: &mut FilmcraftApp, ctx: &egui::Context) {
    if !app.extensions.offer_prompt {
        return;
    }
    let pending: Vec<Offer> = app.extensions.pending_offers().cloned().collect();
    let Some(o) = pending.first() else {
        app.extensions.offer_prompt = false;
        return;
    };
    // once the project is on screen, not at the first frame; and never while another dialog is open
    let due = *app.extensions.offer_due.get_or_insert_with(Instant::now);
    if due.elapsed() < OFFER_DELAY || app.dialog.is_some() || app.extensions.open || app.extensions.ml_licence.is_some() {
        ctx.request_repaint_after(OFFER_DELAY);
        return;
    }
    let mut elems: Vec<(String, egui::Rect, String)> = Vec::new();
    let mut act: Option<Act> = None;
    egui::Window::new("Extension fournie avec FilmCraft")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::RIGHT_BOTTOM, [-16.0, -40.0])
        .default_width(420.0)
        .show(ctx, |ui| {
            ui.strong(format!("« {} » {} n'est pas installée.", o.name, o.version));
            if !o.description.is_empty() {
                ui.label(&o.description);
            }
            ui.add_space(4.0);
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!(
                    "Elle lance un programme de votre ordinateur ({}) quand vous l'exécutez, avec les droits de votre session. L'installer revient à lui faire confiance.",
                    o.programs.join(", ")
                ),
            );
            ui.label("Rien n'a été installé. Vous pourrez aussi l'installer plus tard depuis Extensions ▸ Gestion ▸ « Gérer les extensions… ».");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let r = ui.button(format!("Installer {}", o.name));
                elems.push((format!("extensions.offer.install.{}", o.id), r.rect, "Install".into()));
                if r.clicked() {
                    act = Some(Act::InstallBundled(o.id.clone()));
                }
                let r = ui.button("Plus tard (7 jours)");
                elems.push(("extensions.offer.later".into(), r.rect, "Later".into()));
                if r.clicked() {
                    act = Some(Act::Snooze);
                }
                let r = ui.button("Ne plus proposer");
                elems.push((format!("extensions.offer.never.{}", o.id), r.rect, "Never".into()));
                if r.clicked() {
                    act = Some(Act::DeclineBundled(o.id.clone()));
                }
            });
        });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    match act {
        Some(Act::InstallBundled(id)) => {
            let _ = route(app, ctx, "extensions.installBundled", &json!({"id": id}));
        }
        Some(Act::DeclineBundled(id)) => {
            let _ = route(app, ctx, "extensions.declineBundled", &json!({"id": id}));
        }
        Some(Act::Snooze) => {
            snooze(app);
            app.ui.status = format!("« {} » reste à installer : menu Extensions ▸ Gestion ▸ Gérer les extensions… (pas de rappel avant 7 jours)", o.name);
        }
        _ => {}
    }
}
