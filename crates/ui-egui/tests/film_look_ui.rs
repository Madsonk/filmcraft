//! The Film Look bin in the Effects panel, driven headless through the control channel: the bin is
//! listed, opens on the nine effects, and an effect applied to a clip shows in Effect Controls and
//! in the Program monitor.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render the window offscreen with wgpu (the GPU
//! compositor draws the Program monitor) and write `film-look-*.png`.

use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

const FILM_LOOK_IDS: [&str; 9] = ["film_look", "film_grain", "halation", "bloom", "diffusion", "film_stock", "gate_weave", "film_damage", "lens_character"];

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    snapshots: Option<PathBuf>,
}

impl Driver {
    fn new() -> Self {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(s).with_control(rx);
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if snapshots.is_some() {
            b = b.wgpu();
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, snapshots };
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

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    /// Let the background frame workers deliver (the Program monitor renders off the UI thread).
    fn settle(&mut self) {
        for _ in 0..40 {
            self.frames(2);
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    fn snapshot(&mut self, name: &str) {
        let Some(dir) = self.snapshots.clone() else { return };
        self.settle();
        match self.harness.render() {
            Ok(img) => {
                std::fs::create_dir_all(&dir).unwrap();
                img.save(dir.join(format!("film-look-{name}.png"))).unwrap();
            }
            Err(e) => eprintln!("snapshot {name} skipped: {e}"),
        }
    }
}

#[test]
fn film_look_bin_lists_its_effects_and_applies_them() {
    let mut d = Driver::new();
    if d.ids("effects.folder.").is_empty() {
        // Window ▸ Effects toggles the panel: only when it is not already shown
        d.ok("ui.menu.invoke", json!({"id": "window.panel.Effects", "params": {}}));
        d.frames(3);
    }
    assert!(d.ids("effects.folder.").iter().any(|i| i == "effects.folder.Film Look"), "the Film Look bin is listed");
    // every folder closed (the default layout opens Video Transitions, whose rows would push
    // the bin's effects out of the panel's view), then open the bin by clicking it
    d.app().ui.expanded_fx.clear();
    d.frames(2);
    d.ok("ui.click", json!({"id": "effects.folder.Film Look"}));
    d.frames(3);
    let items = d.ids("effects.item.");
    for id in FILM_LOOK_IDS {
        assert!(items.iter().any(|i| *i == format!("effects.item.{id}")), "{id} under Film Look: {items:?}");
    }
    let clip = d.app().session.active_sequence().unwrap().video_tracks[0].items[0].id.0;
    d.exec("timeline.select", json!({"clips": [clip]}));
    d.snapshot("panel");
    d.exec("effects.apply", json!({"clips": [clip], "effect": "film_look"}));
    d.frames(3);
    let has = d.app().session.active_sequence().unwrap().video_tracks[0].items[0].effects.iter().any(|e| e.effect == "film_look");
    assert!(has, "film_look applied to the clip");
    d.snapshot("applied");
}
