//! Headless UI tests of the Extensions menu (docs/extensions.md): the menu tree (alphabetical, a
//! submenu for several entries, the management items), running an entry from the menu and by command,
//! installing from a folder and an archive, removing, hot reload, the manager dialog by automation id,
//! `requires`, and extensions that try to start extensions.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::panels::extensions::ExtensionsState;
use serde_json::{Value, json};

fn temp(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let d = std::env::temp_dir().join(format!("fc-extui-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Write an extension folder `<parent>/<id>` with the given entries (`[(label, [(command, params)])]`).
fn write_ext(parent: &Path, id: &str, name: &str, entries: Value) -> PathBuf {
    let dir = parent.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let m = json!({"id": id, "name": name, "version": "1.0.0", "description": format!("{name} test extension"), "entries": entries});
    std::fs::write(dir.join("extension.json"), m.to_string()).unwrap();
    dir
}

fn marker_entry(label: &str, name: &str) -> Value {
    json!({"label": label, "action": {"commands": [{"id": "markers.add", "params": {"name": name}}]}})
}

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    /// The demo project, extensions under `root`.
    fn new(root: &Path) -> Self {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let (tx, rx) = channel();
        let mut app = FilmcraftApp::new(s).with_control(rx);
        app.extensions = ExtensionsState::new(Some(root.to_path_buf()));
        app.hooks.open_path = Some(Box::new(|_, _| Ok(())));
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    /// Frames until the entry running in the background (menu and dialog runs) has finished.
    fn settle(&mut self) {
        for _ in 0..3000 {
            if !self.app().extensions.busy() {
                return;
            }
            self.frames(1);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("the extension is still running");
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let v = self.call(method, params.clone());
        assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
        v["result"].clone()
    }

    /// An engine / UI / extension command; the whole reply (`ok`, `result` or `error`).
    fn try_exec(&mut self, command: &str, params: Value) -> Value {
        let v = self.call("engine.execute", json!({"command": command, "params": params}));
        self.settle();
        v
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        let v = self.ok("engine.execute", json!({"command": command, "params": params}));
        self.settle();
        v
    }

    /// Like `try_exec` but does not wait for a background entry to end.
    fn call_now(&mut self, command: &str, params: Value) -> Value {
        self.call("engine.execute", json!({"command": command, "params": params}))
    }

    fn menu_items(&mut self) -> Vec<Value> {
        self.ok("ui.menu.list", json!({})).as_array().unwrap().iter().filter(|m| m["path"][0] == json!("Extensions")).cloned().collect()
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    /// The sequence markers an extension added (the demo project has four of its own).
    fn markers(&mut self) -> Vec<String> {
        let own = ["Open", "Music hit", "Midpoint", "Closing shot"];
        self.app().session.active_sequence().unwrap().markers.iter().map(|m| m.name.clone()).filter(|n| !own.contains(&n.as_str())).collect()
    }

    /// Save what the window shows as `<FILMCRAFT_TEST_SHOT_DIR>/<name>.png` when that variable is set
    /// (how the dialogs are looked at; nothing is written otherwise).
    fn snap(&mut self, name: &str) {
        let Some(dir) = std::env::var_os("FILMCRAFT_TEST_SHOT_DIR") else { return };
        self.frames(3);
        let img = self.harness.render().expect("render the window");
        let _ = std::fs::create_dir_all(&dir);
        img.save(Path::new(&dir).join(format!("{name}.png"))).expect("save the screenshot");
    }

    fn has(&mut self, id: &str) -> bool {
        self.ok("ui.elements", json!({"prefix": id})).as_array().unwrap().iter().any(|e| e["id"] == json!(id))
    }

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
        self.settle();
    }
}

fn labels(items: &[Value]) -> Vec<(Vec<String>, String, String)> {
    items
        .iter()
        .map(|m| {
            (
                m["path"].as_array().unwrap().iter().map(|p| p.as_str().unwrap().to_string()).collect(),
                m["label"].as_str().unwrap().to_string(),
                m["id"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn the_menu_lists_extensions_alphabetically_with_submenus_and_management_items() {
    let root = temp("menu");
    write_ext(&root, "zulu", "zulu tool", json!([marker_entry("Mark", "z")]));
    write_ext(&root, "beta", "Beta Tool", json!([marker_entry("First", "b1"), marker_entry("Second", "b2")]));
    write_ext(&root, "alpha", "Alpha Tool", json!([marker_entry("Only", "a")]));
    // a broken folder is skipped by the menu (the manager reports it)
    std::fs::create_dir_all(root.join("broken")).unwrap();
    std::fs::write(root.join("broken/extension.json"), "{ nope").unwrap();
    let mut d = Driver::new(&root);
    let items = d.menu_items();
    let l = labels(&items);
    let ex = |p: &[&str]| p.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        l,
        vec![
            // extensions without a category: the "Autres" group (a disabled heading), alphabetical
            (ex(&["Extensions"]), "Autres".into(), "ext.group.other".into()),
            (ex(&["Extensions"]), "Alpha Tool".into(), "ext:alpha:0".into()),
            (ex(&["Extensions", "Beta Tool"]), "First".into(), "ext:beta:0".into()),
            (ex(&["Extensions", "Beta Tool"]), "Second".into(), "ext:beta:1".into()),
            (ex(&["Extensions"]), "zulu tool".into(), "ext:zulu:0".into()),
            (ex(&["Extensions"]), "Gestion".into(), "ext.group.management".into()),
            (ex(&["Extensions"]), "Installer une extension…".into(), "extensions.install".into()),
            (ex(&["Extensions"]), "Gérer les extensions…".into(), "extensions.manage".into()),
            (ex(&["Extensions"]), "Recharger les extensions".into(), "extensions.reload".into()),
        ],
        "{items:?}"
    );
    assert_eq!(items[5]["separator_before"], json!(true), "a separator sets the Gestion group apart");
    assert!(items[..5].iter().all(|m| m.get("separator_before").is_none()));
    assert!(items[0]["enabled"] == json!(false) && items[5]["enabled"] == json!(false), "headings are not commands");
    // between Window and Help, as a real top-level menu
    let all = d.ok("ui.menu.list", json!({}));
    assert!(all.as_array().unwrap().iter().any(|m| m["path"] == json!(["Window"])));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_entry_runs_from_the_menu_and_by_command() {
    let root = temp("run");
    write_ext(
        &root,
        "marker-maker",
        "Marker Maker",
        json!([
            marker_entry("Plain", "From the menu"),
            {"label": "With variable", "action": {"commands": [{"id": "markers.add", "params": {"name": "${var.who|\"nobody\"}"}}]}}
        ]),
    );
    let mut d = Driver::new(&root);
    assert!(d.markers().is_empty());
    // a menu item runs in the background: it returns at once and the window serves the steps
    d.ok("ui.menu.invoke", json!({"id": "ext:marker-maker:0"}));
    d.settle();
    assert_eq!(d.markers(), ["From the menu"]);
    assert!(d.app().ui.status.contains("Plain: 1 command done"), "{}", d.app().ui.status);
    // by command, an entry by label, with a variable
    let r = d.exec("extensions.run", json!({"extension": "marker-maker", "entry": "with variable", "vars": {"who": "Ada"}}));
    assert_eq!(r["steps"][0]["ok"], json!(true), "{r}");
    d.exec("extensions.run", json!({"extension": "marker-maker", "entry": 1}));
    assert_eq!(d.markers(), ["From the menu", "Ada", "nobody"]);
    // one undo step per command: the markers come off one at a time
    d.exec("edit.undo", json!({}));
    assert_eq!(d.markers(), ["From the menu", "Ada"]);
    // unknown extension / entry are errors, not panics
    let e = d.try_exec("extensions.run", json!({"extension": "nope"}));
    assert!(e["error"].as_str().unwrap().contains("not installed"), "{e}");
    let e = d.try_exec("extensions.run", json!({"extension": "marker-maker", "entry": 9}));
    assert!(e["error"].as_str().unwrap().contains("no entry number 9"), "{e}");
    let e = d.try_exec("extensions.run", json!({"extension": "marker-maker", "entry": "nope"}));
    assert!(e["error"].as_str().unwrap().contains("no entry"), "{e}");
    let e = d.try_exec("extensions.run", json!({}));
    assert!(e["error"].as_str().unwrap().contains("needs `extension`"), "{e}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_failing_step_is_reported_and_stops_the_entry() {
    let root = temp("fail");
    write_ext(
        &root,
        "broken-steps",
        "Broken Steps",
        json!([{"label": "Go", "action": {"commands": [
            {"id": "markers.add", "params": {"name": "before"}},
            {"id": "no.such.command"},
            {"id": "markers.add", "params": {"name": "after"}}
        ]}}]),
    );
    let mut d = Driver::new(&root);
    let e = d.try_exec("extensions.run", json!({"extension": "broken-steps"}));
    assert_eq!(e["ok"], json!(false));
    assert!(e["error"].as_str().unwrap().contains("step 2 (no.such.command) failed"), "{e}");
    assert_eq!(d.markers(), ["before"], "the step after the failure did not run");
    assert!(d.app().extensions.last.as_ref().unwrap().error.is_some());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn extensions_cannot_start_extensions() {
    let root = temp("recurse");
    write_ext(&root, "loop", "Loop", json!([{"label": "Go", "action": {"commands": [{"id": "extensions.run", "params": {"extension": "loop"}}]}}]));
    // a menu item id is not a command id a manifest may use: the extension does not even load
    write_ext(&root, "item", "Item", json!([{"label": "Go", "action": {"commands": [{"id": "ext:loop:0"}]}}]));
    let mut d = Driver::new(&root);
    let e = d.try_exec("extensions.run", json!({"extension": "loop"}));
    assert!(e["error"].as_str().unwrap().contains("can't run extensions"), "{e}");
    let e = d.try_exec("extensions.run", json!({"extension": "item"}));
    assert!(e["error"].as_str().unwrap().contains("not installed"), "{e}");
    let listed = d.exec("extensions.list", json!({}));
    assert!(
        listed["extensions"].as_array().unwrap().iter().any(|x| x["id"] == json!("item") && x["error"].as_str().unwrap().contains("not a command id")),
        "{listed}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn requirements_disable_the_menu_item_and_refuse_the_run() {
    let root = temp("requires");
    write_ext(
        &root,
        "needs-selection",
        "Needs Selection",
        json!([{"label": "Go", "requires": ["selection"], "action": {"commands": [{"id": "markers.add"}]}}]),
    );
    write_ext(&root, "needs-saved", "Needs Saved", json!([{"label": "Go", "requires": ["project"], "action": {"commands": [{"id": "markers.add"}]}}]));
    let mut d = Driver::new(&root);
    let by_id = |items: &[Value], id: &str| items.iter().find(|m| m["id"] == json!(id)).unwrap()["enabled"].clone();
    let items = d.menu_items();
    assert_eq!(by_id(&items, "ext:needs-selection:0"), json!(false));
    assert_eq!(by_id(&items, "ext:needs-saved:0"), json!(false), "the demo project has not been saved");
    let e = d.try_exec("extensions.run", json!({"extension": "needs-selection"}));
    assert!(e["error"].as_str().unwrap().contains("select clips"), "{e}");
    let clip = d.app().session.active_sequence().unwrap().video_tracks[0].items[0].id.0;
    d.exec("timeline.select", json!({"clips": [clip]}));
    d.frames(2);
    let items = d.menu_items();
    assert_eq!(by_id(&items, "ext:needs-selection:0"), json!(true));
    d.exec("extensions.run", json!({"extension": "needs-selection"}));
    assert_eq!(d.markers().len(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn install_from_a_folder_and_an_archive_remove_and_hot_reload() {
    let root = temp("install");
    let src = temp("install-src");
    let mut d = Driver::new(&root);
    // no extension yet: only the Gestion heading and the management items
    assert_eq!(d.menu_items().len(), 4);
    // a folder
    let one = write_ext(&src, "one", "One", json!([marker_entry("Go", "one")]));
    let r = d.exec("extensions.install", json!({"path": one.to_string_lossy()}));
    assert_eq!((r["id"].clone(), r["replaced"].clone()), (json!("one"), json!(null)), "{r}");
    assert!(root.join("one/extension.json").is_file());
    assert!(d.menu_items().iter().any(|m| m["id"] == json!("ext:one:0")));
    assert!(d.app().ui.status.contains("Installed One"), "{}", d.app().ui.status);
    // an update reports the version it replaced
    let r = d.exec("extensions.install", json!({"path": one.to_string_lossy()}));
    assert_eq!(r["replaced"], json!("1.0.0"));
    // an archive
    let two = write_ext(&src, "two", "Two", json!([marker_entry("Go", "two")]));
    let archive = src.join("two.fcext");
    filmcraft_extensions::pack::pack_dir(&two, &archive).unwrap();
    d.exec("extensions.install", json!({"path": archive.to_string_lossy()}));
    let ids: Vec<_> = d.menu_items().iter().map(|m| m["id"].as_str().unwrap().to_string()).collect();
    assert!(ids.contains(&"ext:one:0".to_string()) && ids.contains(&"ext:two:0".to_string()), "{ids:?}");
    // a bad package is refused with a readable error and nothing changes
    std::fs::write(src.join("junk.fcext"), b"not an archive").unwrap();
    let e = d.try_exec("extensions.install", json!({"path": src.join("junk.fcext").to_string_lossy()}));
    assert!(e["error"].as_str().unwrap().contains("not a valid archive"), "{e}");
    let e = d.try_exec("extensions.install", json!({"path": src.join("missing").to_string_lossy()}));
    assert!(e["error"].as_str().unwrap().contains("does not exist"), "{e}");
    // no picker in a test: Install without a path is an error, not a hang
    let e = d.try_exec("extensions.install", json!({}));
    assert!(e["error"].as_str().unwrap().contains("no file picker"), "{e}");
    // remove
    d.exec("extensions.remove", json!({"id": "one"}));
    assert!(!root.join("one").exists());
    assert!(!d.menu_items().iter().any(|m| m["id"] == json!("ext:one:0")));
    let e = d.try_exec("extensions.remove", json!({"id": "../one"}));
    assert_eq!(e["ok"], json!(false));
    // hot reload: an extension dropped into the folder by hand appears after Reload
    write_ext(&root, "three", "Three", json!([marker_entry("Go", "three")]));
    assert!(!d.menu_items().iter().any(|m| m["id"] == json!("ext:three:0")));
    d.exec("extensions.reload", json!({}));
    assert!(d.menu_items().iter().any(|m| m["id"] == json!("ext:three:0")));
    let list = d.exec("extensions.list", json!({}));
    assert_eq!(list["extensions"].as_array().unwrap().len(), 2, "{list}");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&src);
}

#[test]
fn the_manager_dialog_runs_and_removes_by_automation_id() {
    let root = temp("dialog");
    write_ext(&root, "alpha", "Alpha", json!([marker_entry("Go", "from the dialog")]));
    std::fs::create_dir_all(root.join("broken")).unwrap();
    std::fs::write(root.join("broken/extension.json"), "{ nope").unwrap();
    let mut d = Driver::new(&root);
    assert!(!d.has("extensions.dialog.close"));
    d.exec("extensions.manage", json!({}));
    d.frames(3);
    for id in [
        "extensions.dialog.close",
        "extensions.dialog.reload",
        "extensions.dialog.installFile",
        "extensions.dialog.installFolder",
        "extensions.dialog.run.alpha.0",
        "extensions.dialog.remove.alpha",
        "extensions.dialog.remove.broken",
    ] {
        assert!(d.has(id), "missing {id}");
    }
    d.click("extensions.dialog.run.alpha.0");
    assert_eq!(d.markers(), ["from the dialog"]);
    assert!(d.app().extensions.last.as_ref().unwrap().ok());
    // the broken folder can be cleared from the dialog
    d.click("extensions.dialog.remove.broken");
    assert!(!root.join("broken").exists());
    d.click("extensions.dialog.remove.alpha");
    assert!(!root.join("alpha").exists());
    assert!(!d.has("extensions.dialog.run.alpha.0"));
    // Install… without a picker reports instead of hanging
    d.click("extensions.dialog.installFile");
    assert!(d.app().ui.status.contains("no file picker"), "{}", d.app().ui.status);
    d.click("extensions.dialog.close");
    assert!(!d.has("extensions.dialog.close"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn bundled_extensions_are_seeded_once() {
    let root = temp("seed");
    let bundled = temp("seed-bundled");
    write_ext(&bundled, "shipped", "Shipped", json!([marker_entry("Go", "x")]));
    let mut d = Driver::new(&root);
    let (done, problems) = d.app().extensions.seed_bundled(&bundled);
    assert_eq!((done, problems.len()), (vec!["shipped".to_string()], 0));
    d.frames(2);
    assert!(d.menu_items().iter().any(|m| m["id"] == json!("ext:shipped:0")));
    d.exec("extensions.remove", json!({"id": "shipped"}));
    let (done, _) = d.app().extensions.seed_bundled(&bundled);
    assert!(done.is_empty(), "a removed extension stays removed");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&bundled);
}

#[test]
fn without_a_data_directory_the_menu_still_works() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let mut app = FilmcraftApp::new(s);
    assert!(app.extensions.root().is_none());
    let ctx = egui::Context::default();
    let items = filmcraft_ui_egui::menus::menu_items(&app);
    let install = items.iter().find(|i| i.id == "extensions.install").unwrap();
    assert!(!install.enabled);
    let e = filmcraft_ui_egui::menus::invoke(&mut app, &ctx, "extensions.install", json!({"path": "/nonexistent"})).unwrap_err();
    assert!(e.contains("no data directory"), "{e}");
}

/// Windows and Linux draw the menu bar in the window: Extensions is a real top-level menu there too,
/// with the extension in it, a submenu for a multi-entry extension and the management items.
#[test]
fn the_in_window_menu_bar_opens_the_extensions_menu() {
    use egui_kittest::kittest::Queryable;
    let root = temp("bar");
    write_ext(&root, "alpha", "Alpha Tool", json!([marker_entry("Only", "a")]));
    write_ext(&root, "beta", "Beta Tool", json!([marker_entry("First", "b1"), marker_entry("Second", "b2")]));
    let mut d = Driver::new(&root);
    assert!(d.app().ui.show_menu_bar);
    d.harness.get_by_label("Extensions").click();
    d.frames(4);
    for label in ["Alpha Tool", "Beta Tool", "Installer une extension…", "Gérer les extensions…", "Recharger les extensions"] {
        let n = d.harness.query_all_by_label_contains(label).count();
        assert!(n >= 1, "menu item {label} not shown");
    }
    // the multi-entry extension opens as a submenu holding its entries
    d.harness.get_by_label_contains("Beta Tool").click();
    d.frames(4);
    for label in ["First", "Second"] {
        assert!(d.harness.query_all_by_label_contains(label).count() >= 1, "submenu entry {label} not shown");
    }
    // clicking an item runs it
    d.harness.get_by_label("Alpha Tool").click();
    d.frames(4);
    assert_eq!(d.markers(), ["a"]);
    // the manager opens from the menu
    d.harness.get_by_label("Extensions").click();
    d.frames(4);
    d.harness.get_by_label("Gérer les extensions…").click();
    d.frames(4);
    assert!(d.has("extensions.dialog.close"));
    let _ = std::fs::remove_dir_all(&root);
}

/// A shipped extension that starts a program: `scripts/go.sh` through `exec`.
fn write_program_ext(parent: &Path, id: &str, name: &str) -> PathBuf {
    let dir = parent.join(id);
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::write(dir.join("scripts/go.sh"), "#!/bin/sh\n").unwrap();
    let m = json!({"formatVersion": 1, "id": id, "name": name, "version": "1.0.0", "description": format!("{name} makes things"),
        "entries": [{"label": "Go", "action": {"exec": {"program": "scripts/go.sh"}}}]});
    std::fs::write(dir.join("extension.json"), m.to_string()).unwrap();
    dir
}

#[test]
fn a_shipped_extension_that_starts_a_program_is_asked_about_never_installed_silently() {
    let root = temp("offer");
    let bundled = temp("offer-bundled");
    write_ext(&bundled, "safe", "Safe Tool", json!([marker_entry("Go", "s")]));
    write_program_ext(&bundled, "risky", "Risky Tool");
    let mut d = Driver::new(&root);
    let (done, problems) = d.app().extensions.seed_bundled(&bundled);
    assert_eq!((done, problems.len()), (vec!["safe".to_string()], 0), "only the extension that drives FilmCraft is installed");
    // the question waits a moment so the project is drawn first (ext-all-v2): not on screen yet…
    d.frames(3);
    assert!(!d.has("extensions.offer.later"), "the question is not asked over the first frames");
    d.app().extensions.offer_delay = std::time::Duration::ZERO;
    d.frames(3);
    assert!(!root.join("risky").exists(), "nothing installed behind the user's back");
    assert!(d.menu_items().iter().all(|m| m["id"] != json!("ext:risky:0")));
    // the question is on screen, with the three answers
    for id in ["extensions.offer.install.risky", "extensions.offer.later", "extensions.offer.never.risky"] {
        assert!(d.has(id), "{id}");
    }
    d.snap("offer-dialog");
    let o = d.exec("extensions.offers", json!({}));
    assert_eq!(o["asking"], json!(true));
    assert_eq!(o["offers"][0]["programs"], json!(["scripts/go.sh"]), "{o}");
    // Plus tard: no more question, the status bar says where to find it, the manager lists it with the warning
    d.click("extensions.offer.later");
    assert!(!d.has("extensions.offer.later"));
    assert!(d.app().ui.status.contains("Gérer les extensions"), "{}", d.app().ui.status);
    assert!(!root.join("risky").exists());
    // …and it is remembered for seven days with this version: the next launch does not ask
    let seeded: Value = serde_json::from_str(&std::fs::read_to_string(root.join(".bundled-seeded.json")).unwrap()).unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let until = seeded["snoozed_until"].as_u64().unwrap();
    assert!(until > now + 6 * 24 * 3600 && until <= now + 7 * 24 * 3600 + 5, "{seeded}");
    assert_eq!(seeded["snoozed_version"], json!(env!("CARGO_PKG_VERSION")));
    d.app().extensions = ExtensionsState::new(Some(root.clone()));
    d.app().extensions.offer_delay = std::time::Duration::ZERO;
    d.app().extensions.seed_bundled(&bundled);
    d.frames(3);
    assert!(!d.has("extensions.offer.later"), "snoozed: not asked at the next launch");
    assert_eq!(d.exec("extensions.offers", json!({}))["asking"], json!(false));
    d.exec("extensions.manage", json!({}));
    d.frames(3);
    d.snap("manager-with-offer");
    assert!(d.has("extensions.dialog.offer.install.risky"));
    d.click("extensions.dialog.offer.install.risky");
    assert!(root.join("risky/extension.json").is_file(), "installed after the explicit click");
    assert!(d.app().extensions.message.contains("Installée"), "{}", d.app().extensions.message);
    assert!(d.menu_items().iter().any(|m| m["id"] == json!("ext:risky:0")));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&bundled);
}

/// A data folder seeded by an older FilmCraft (manifests without `category` / `models`) gets the menu
/// groups and the model lines after one launch: the untouched bundled extensions are refreshed from the
/// current bundle (ext-all-v3).
#[test]
fn an_older_seeded_folder_is_refreshed_from_the_bundle_and_gets_the_menu_groups() {
    let root = temp("legacy");
    let bundled_old = temp("legacy-bundle-old");
    let bundled_new = temp("legacy-bundle-new");
    let old_dir = write_ext(&bundled_old, "cleaner", "Cleaner", json!([marker_entry("Clean", "c")]));
    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(old_dir.join("extension.json")).unwrap()).unwrap();
    m["author"] = json!("FilmCraft");
    std::fs::write(old_dir.join("extension.json"), m.to_string()).unwrap();
    // the new bundle: a newer version with the menu fields and one more entry (as ext-all v1 → v3)
    let dir = write_ext(&bundled_new, "cleaner", "Cleaner", json!([marker_entry("Clean", "c")]));
    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("extension.json")).unwrap()).unwrap();
    m["author"] = json!("FilmCraft");
    m["version"] = json!("1.1.0");
    m["category"] = json!("audio");
    m["models"] = json!(["deepfilternet3"]);
    m["entries"].as_array_mut().unwrap().push(marker_entry("Clean more", "c2"));
    std::fs::write(dir.join("extension.json"), m.to_string()).unwrap();
    // the older FilmCraft seeded it, recording no fingerprint (its marker file only listed the ids)
    let mut d = Driver::new(&root);
    d.app().extensions.seed_bundled(&bundled_old);
    std::fs::write(root.join(".bundled-seeded.json"), json!({"ids": ["cleaner"]}).to_string()).unwrap();
    d.frames(2);
    let items = d.menu_items();
    assert!(items.iter().any(|i| i["id"] == json!("ext.group.other")) && items.iter().all(|i| i["id"] != json!("ext.group.audio")), "{items:?}");
    // the new launch: refreshed, filed under Audio, with the model line
    let (done, problems) = d.app().extensions.seed_bundled(&bundled_new);
    assert!(problems.is_empty() && done.len() == 1 && done[0].contains("mis à jour depuis le bundle"), "{done:?} {problems:?}");
    d.frames(2);
    let items = d.menu_items();
    let ids: Vec<&str> = items.iter().filter_map(|i| i["id"].as_str()).collect();
    assert!(ids.contains(&"ext.group.audio") && !ids.contains(&"ext.group.other"), "{ids:?}");
    assert!(ids.contains(&"ext.model.cleaner.0") && ids.contains(&"ext:cleaner:1"), "{ids:?}");
    assert!(root.join(".replaced/cleaner-1.0.0/extension.json").is_file(), "the folder of the older version is kept aside");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&bundled_old);
    let _ = std::fs::remove_dir_all(&bundled_new);
}

#[test]
fn refusing_a_shipped_program_extension_is_remembered_and_install_still_possible() {
    let root = temp("never");
    let bundled = temp("never-bundled");
    write_program_ext(&bundled, "risky", "Risky Tool");
    let mut d = Driver::new(&root);
    d.app().extensions.offer_delay = std::time::Duration::ZERO;
    d.app().extensions.seed_bundled(&bundled);
    d.frames(3);
    d.click("extensions.offer.never.risky");
    assert!(!d.has("extensions.offer.never.risky"));
    assert!(!root.join("risky").exists());
    // the next launch does not ask again…
    d.app().extensions.seed_bundled(&bundled);
    d.frames(3);
    assert!(!d.has("extensions.offer.never.risky"), "not asked twice");
    // …but the manager still offers it
    let o = d.exec("extensions.offers", json!({}));
    assert_eq!((o["asking"].clone(), o["offers"][0]["answered"].clone()), (json!(false), json!(true)), "{o}");
    d.exec("extensions.installBundled", json!({"id": "risky"}));
    assert!(root.join("risky/extension.json").is_file());
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&bundled);
}

/// A recogniser that takes its time and reports progress, like Whisper on a long recording.
struct Slow {
    inner: filmcraft_speech::FixedTranscriber,
    /// 40 ms each; a test that cancels needs many so a loaded machine cannot let the run finish first.
    steps: u32,
}

impl filmcraft_speech::Transcriber for Slow {
    fn id(&self) -> String {
        "slow".into()
    }
    fn transcribe(
        &self,
        audio: &[f32],
        o: &filmcraft_speech::Options,
        progress: filmcraft_speech::ProgressFn,
    ) -> Result<filmcraft_project::Transcript, filmcraft_speech::SpeechError> {
        for i in 0..self.steps {
            std::thread::sleep(std::time::Duration::from_millis(40));
            if !progress(i as f32 / self.steps as f32, "Transcribing") {
                return Err(filmcraft_speech::SpeechError::Cancelled);
            }
        }
        self.inner.transcribe(audio, o, progress)
    }
}

fn install_slow_transcriber(d: &mut Driver, steps: u32) {
    use filmcraft_project::{Transcript, Word};
    use filmcraft_time::Tick;
    let s = &mut d.app().session;
    let a = &s.active_sequence().unwrap().audio_tracks[0].items[0];
    let sin = a.source_in;
    let mut t = Transcript { language: "en".into(), ..Default::default() };
    for (text, x, y) in [("Hello", 0.2, 0.5), ("um", 0.6, 0.9), ("world.", 1.0, 1.4)] {
        t.words.push(Word::new(text, sin + Tick::from_seconds_f64(x), sin + Tick::from_seconds_f64(y)));
    }
    t.normalize();
    s.transcriber = Some(std::sync::Arc::new(Slow { inner: filmcraft_speech::FixedTranscriber { transcript: t, id: "slow".into() }, steps }));
}

#[test]
fn a_transcription_runs_in_the_background_with_progress_and_the_window_keeps_painting() {
    let root = temp("bg");
    write_ext(
        &root,
        "talk",
        "Talk",
        json!([{"label": "Transcribe and caption", "action": {"commands": [
            {"id": "sequence.transcribe", "params": {"language": "en"}},
            {"id": "transcript.createCaptions", "params": {}}]}}]),
    );
    let mut d = Driver::new(&root);
    install_slow_transcriber(&mut d, 20);
    let words_before = d.app().session.project.transcripts.len();
    // start it like the menu does (background), without waiting
    let (req, reply) = ControlRequest::new("engine.execute", json!({"command": "ext:talk:0", "params": {}}));
    d.tx.send(req).unwrap();
    d.frames(2);
    let started = reply.try_recv().expect("the menu item returns at once");
    assert_eq!(started["ok"], json!(true), "{started}");
    assert_eq!(started["result"]["started"], json!(true));
    // while it runs: frames go on, progress rises, the lock refuses edits, the cancel button is there
    let (mut frames, mut seen, mut refused) = (0, std::collections::BTreeSet::new(), false);
    while d.app().extensions.busy() && frames < 3000 {
        d.frames(1);
        frames += 1;
        std::thread::sleep(std::time::Duration::from_millis(5));
        let st = d.call_now("extensions.status", json!({}))["result"].clone();
        if st["waiting"] == json!(true) {
            seen.insert((st["progress"].as_f64().unwrap() * 10.0) as i64);
            assert!(d.app().ui.status.contains("transcription"), "{}", d.app().ui.status);
            if !refused {
                d.snap("running-window");
                let e = d.call_now("edit.undo", json!({}));
                assert_eq!(e["ok"], json!(false), "edits are refused while an extension runs: {e}");
                assert!(e["error"].as_str().unwrap().contains("Extension"), "{e}");
                assert!(d.has("extensions.running.cancel") && d.has("extensions.running.progress"));
                refused = true;
            }
        }
    }
    assert!(frames > 8, "the window painted {frames} frames while the recogniser worked");
    assert!(seen.len() >= 3, "the progress went through {seen:?}");
    assert!(refused);
    let last = d.app().extensions.last.clone().expect("a report");
    assert!(last.ok(), "{:?}", last.error);
    let line = last.summary();
    assert!(line.contains("21 mots transcrits dans 7 médias") && line.contains("5 sous-titres créés"), "{line}");
    assert!(d.app().session.project.transcripts.len() > words_before);
    assert_eq!(d.app().ui.status, line);
    // one undo step per stage, and the sequence is editable again
    assert_eq!(d.try_exec("edit.undo", json!({}))["ok"], json!(true));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cancelling_a_background_transcription_stops_the_entry_and_keeps_the_project_untouched() {
    let root = temp("cancel");
    write_ext(
        &root,
        "talk",
        "Talk",
        json!([{"label": "Transcribe", "action": {"commands": [
            {"id": "sequence.transcribe", "params": {"language": "en"}}, {"id": "markers.add", "params": {"name": "after"}}]}}]),
    );
    let mut d = Driver::new(&root);
    install_slow_transcriber(&mut d, 400);
    let transcripts = d.app().session.project.transcripts.len();
    d.call_now("extensions.run", json!({"extension": "talk", "entry": 0, "background": true}));
    for _ in 0..400 {
        d.frames(1);
        std::thread::sleep(std::time::Duration::from_millis(5));
        if d.call_now("extensions.status", json!({}))["result"]["waiting"] == json!(true) {
            break;
        }
    }
    d.click("extensions.running.cancel");
    d.settle();
    let last = d.app().extensions.last.clone().expect("a report");
    assert!(!last.ok());
    assert!(last.summary().contains("annulée"), "{}", last.summary());
    assert_eq!(d.app().session.project.transcripts.len(), transcripts, "nothing was stored");
    assert!(d.markers().is_empty(), "the next step did not run");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_program_entry_runs_off_the_ui_thread() {
    let root = temp("prog");
    let dir = root.join("slowprog");
    std::fs::create_dir_all(&dir).unwrap();
    let m = json!({"formatVersion": 1, "id": "slowprog", "name": "Slow Program", "version": "1.0.0",
        "entries": [{"label": "Go", "action": {"exec": {"program": "sh", "args": ["-c", "sleep 1; echo '{\"id\":\"markers.add\",\"params\":{\"name\":\"late\"}}'; echo '{\"log\":\"hello\"}'"]}}}]});
    std::fs::write(dir.join("extension.json"), m.to_string()).unwrap();
    let mut d = Driver::new(&root);
    d.call_now("extensions.run", json!({"extension": "slowprog", "entry": 0, "background": true}));
    let t0 = std::time::Instant::now();
    let mut frames = 0;
    while d.app().extensions.busy() && t0.elapsed().as_secs() < 20 {
        d.frames(1);
        frames += 1;
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(frames > 30, "{frames} frames while the program slept for a second");
    assert_eq!(d.markers(), ["late"]);
    assert!(d.app().extensions.last.as_ref().unwrap().ok());
    let _ = std::fs::remove_dir_all(&root);
}
