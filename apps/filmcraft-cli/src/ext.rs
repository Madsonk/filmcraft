//! `filmcraft-cli extensions …`: manage the extensions folder and run an extension entry headless.
//! See docs/extensions.md.

use std::path::{Path, PathBuf};

use filmcraft_extensions::{ExtensionStore, Host, Manifest, RunContext, pack, run_entry};
use serde_json::{Value, json};

use crate::args::{Args, parse_value};
use crate::{Backend, fail, print, usage};

pub const HELP: &str = "\
EXTENSIONS (docs/extensions.md)
  extensions list                      the installed extensions (id, name, version, entries) as JSON
  extensions install <folder|.zip|.fcext>
                                       install or update an extension into <data dir>/extensions/<id>;
                                       a folder holding several extensions installs each of them
  extensions remove <id>               delete an installed extension
  extensions run <id> [entry] [--var name=value ...] [--select id,id,...]
                                       run an entry (number or label; default 0) on the --project /
                                       --demo session, or in the running app with --bridge; commands
                                       can use ${var.name}
  extensions validate <folder|.zip|.fcext>
                                       check a package without installing it; names commands this CLI
                                       does not know (the window has a few more)
  extensions pack <folder> <out.fcext> pack an extension folder for sharing
  --data-dir <dir> chooses the data directory (default: the app's)
";

fn data_dir(a: &Args) -> PathBuf {
    a.opt("--data-dir")
        .map(PathBuf::from)
        .or_else(filmcraft_engine::autosave::default_data_dir)
        .unwrap_or_else(|| fail("no data directory (pass --data-dir <dir>)"))
}

fn store(a: &Args) -> ExtensionStore {
    ExtensionStore::in_data_dir(&data_dir(a))
}

fn entry_index(ext: &filmcraft_extensions::Extension, which: Option<&str>) -> usize {
    let Some(w) = which else { return 0 };
    if let Ok(n) = w.parse::<usize>() {
        return n;
    }
    ext.manifest.entries.iter().position(|e| e.label.eq_ignore_ascii_case(w)).unwrap_or_else(|| {
        fail(format!(
            "`{}` has no entry `{w}` (entries: {})",
            ext.manifest.id,
            ext.manifest.entries.iter().map(|e| e.label.as_str()).collect::<Vec<_>>().join(", ")
        ))
    })
}

fn summary(l: &filmcraft_extensions::Listed) -> Value {
    match &l.result {
        Ok(x) => json!({
            "id": x.manifest.id, "name": x.manifest.name, "version": x.manifest.version, "description": x.manifest.description,
            "author": x.manifest.author, "dir": x.dir, "runsPrograms": x.manifest.runs_programs(),
            "entries": x.manifest.entries.iter().map(|e| json!({"label": e.label, "kind": e.action.kind(), "requires": e.requires})).collect::<Vec<_>>(),
        }),
        Err(e) => json!({"id": l.dir_name, "error": e}),
    }
}

/// Command ids a package uses that this CLI's engine does not have.
fn unknown_commands(m: &Manifest, dir: &Path) -> Vec<String> {
    let mut s = filmcraft_engine::Session::default();
    let known: Vec<String> = s
        .execute("command.list", json!({}))
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|c| c["id"].as_str().map(str::to_string))
        .collect();
    let mut used: Vec<String> = Vec::new();
    for e in &m.entries {
        if let Some(steps) = &e.action.commands {
            used.extend(steps.iter().map(|s| s.id.clone()));
        }
        if let Some(p) = &e.action.jsonl
            && let Ok(text) = std::fs::read_to_string(dir.join(p))
            && let Ok(steps) = filmcraft_extensions::run::parse_jsonl(&text)
        {
            used.extend(steps.into_iter().map(|s| s.id));
        }
    }
    used.sort();
    used.dedup();
    used.retain(|u| !known.contains(u));
    used
}

pub async fn main(a: &Args) {
    let Some(sub) = a.pos(1) else { usage("extensions: list | install <path> | remove <id> | run <id> | validate <path> | pack <dir> <out>") };
    match sub {
        "list" => {
            let list: Vec<Value> = store(a).list().iter().map(summary).collect();
            print(a, &Value::Array(list));
        }
        "install" => {
            let src = a.pos(2).unwrap_or_else(|| usage("extensions install <folder|.zip|.fcext>"));
            let st = store(a);
            // a folder of extensions (such as extensions-bundled/) installs each one
            let many: Vec<PathBuf> = match std::fs::read_dir(src) {
                Ok(rd) if !Path::new(src).join(filmcraft_extensions::manifest::MANIFEST_FILE).exists() => {
                    let mut v: Vec<PathBuf> =
                        rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.join(filmcraft_extensions::manifest::MANIFEST_FILE).is_file()).collect();
                    v.sort();
                    v
                }
                _ => Vec::new(),
            };
            let targets = if many.is_empty() { vec![PathBuf::from(src)] } else { many.clone() };
            let mut reports = Vec::new();
            let mut failed = false;
            for t in &targets {
                match st.install(t) {
                    Ok(r) => {
                        if r.runs_programs {
                            eprintln!("note: `{}` starts external programs when you run it. Only keep extensions you trust.", r.name);
                        }
                        reports.push(serde_json::to_value(&r).unwrap_or_default());
                    }
                    Err(e) if many.is_empty() => fail(format!("extensions install: {e}")),
                    Err(e) => {
                        eprintln!("filmcraft-cli: extensions install {}: {e}", t.display());
                        failed = true;
                    }
                }
            }
            print(a, &if many.is_empty() { reports.into_iter().next().unwrap_or_default() } else { Value::Array(reports) });
            if failed {
                std::process::exit(1);
            }
        }
        "remove" => {
            let id = a.pos(2).unwrap_or_else(|| usage("extensions remove <id>"));
            match store(a).remove(id) {
                Ok(()) => print(a, &json!({"removed": id})),
                Err(e) => fail(format!("extensions remove: {e}")),
            }
        }
        "pack" => {
            let (Some(dir), Some(out)) = (a.pos(2), a.pos(3)) else { usage("extensions pack <folder> <out.fcext>") };
            match pack::pack_dir(Path::new(dir), Path::new(out)) {
                Ok(m) => print(a, &json!({"packed": m.id, "version": m.version, "path": out})),
                Err(e) => fail(format!("extensions pack: {e}")),
            }
        }
        "validate" => {
            let src = a.pos(2).unwrap_or_else(|| usage("extensions validate <folder|.zip|.fcext>"));
            let tmp = std::env::temp_dir().join(format!("fc-ext-validate-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&tmp);
            if let Err(e) = std::fs::create_dir_all(&tmp) {
                fail(format!("{}: {e}", tmp.display()));
            }
            let r = pack::stage(Path::new(src), &tmp);
            let unknown = r.as_ref().map(|m| unknown_commands(m, &tmp)).unwrap_or_default();
            let _ = std::fs::remove_dir_all(&tmp);
            match r {
                Ok(m) => print(
                    a,
                    &json!({"valid": true, "id": m.id, "name": m.name, "version": m.version, "entries": m.entries.len(), "runsPrograms": m.runs_programs(), "commandsUnknownToThisCli": unknown}),
                ),
                Err(e) => fail(format!("extensions validate: {e}")),
            }
        }
        "run" => run(a).await,
        other => usage(format!("extensions: unknown `{other}`")),
    }
}

async fn run(a: &Args) {
    let id = a.pos(2).unwrap_or_else(|| usage("extensions run <id> [entry] [--var name=value ...] [--select id,id,...]"));
    let ext = store(a).get(id).unwrap_or_else(|e| fail(format!("extensions run: {e}")));
    let entry = entry_index(&ext, a.pos(3));
    let mut vars = std::collections::BTreeMap::new();
    for kv in a.all("--var") {
        let Some((k, v)) = kv.split_once('=') else { usage(format!("--var expects name=value, got `{kv}`")) };
        vars.insert(k.to_string(), parse_value(v));
    }
    // `--select 12,13`: the clips selected when the entry runs (a headless session starts with none)
    let select: Vec<u64> = match a.opt("--select") {
        None => Vec::new(),
        Some(list) => list
            .split(',')
            .filter(|x| !x.trim().is_empty())
            .map(|x| x.trim().parse::<u64>().unwrap_or_else(|_| usage(format!("--select expects clip ids such as 12,13, got `{x}`"))))
            .collect(),
    };
    let mut b = Backend::open(a);
    if !select.is_empty()
        && let Err(e) = b.exec("timeline.select", json!({"clips": select})).await
    {
        fail(format!("extensions run: --select: {e}"));
    }
    let Backend::Local(session) = &mut b else {
        // the running app has the same command: its window runs the entry
        let mut p = json!({"extension": id, "entry": entry});
        if !vars.is_empty() {
            p["vars"] = json!(vars);
        }
        match b.exec("extensions.run", p).await {
            Ok(v) => print(a, &v),
            Err(e) => fail(format!("extensions run: {e}")),
        }
        return;
    };
    let mut ctx = RunContext::new(Host::Cli);
    ctx.project = session.path.clone();
    ctx.selection = session.state.selection.iter().map(|c| c.0).collect();
    ctx.playhead_ticks = session.playhead().0;
    ctx.playhead_seconds = session.playhead().seconds();
    ctx.has_sequence = session.active_sequence().is_some();
    ctx.data_dir = Some(data_dir(a));
    ctx.vars = vars;
    let report = run_entry(&ext, entry, &ctx, &mut |cmd, params| session.execute(cmd, params).map_err(|e| e.to_string()));
    let ok = match report {
        Ok(r) => {
            print(a, &serde_json::to_value(&r).unwrap_or_default());
            // the same line as the window's status bar, on stderr so that stdout stays the JSON report
            eprintln!("{}", r.summary());
            r.ok()
        }
        Err(e) => fail(format!("extensions run: {e}")),
    };
    b.finish(a).await;
    if !ok {
        std::process::exit(1);
    }
}
