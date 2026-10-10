//! Hostile input: nothing here may panic, whatever the bytes are. Mutated manifests, mutated and
//! truncated archives, hostile ids and paths; and the manifests shipped in `extensions-bundled/` stay valid.

use std::panic::catch_unwind;
use std::path::{Path, PathBuf};

use filmcraft_extensions::manifest::Manifest;
use filmcraft_extensions::{ExtensionStore, pack};

fn temp(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let d = std::env::temp_dir().join(format!("fcext-rb-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

const GOOD: &str = r#"{"formatVersion":1,"id":"demo","name":"Demo","version":"1.0.0","description":"d","author":"a","entries":[
 {"label":"A","requires":["sequence"],"action":{"commands":[{"id":"markers.add","params":{"name":"${project_name}"}},{"id":"edit.undo","continueOnError":true}]}},
 {"label":"B","action":{"jsonl":"steps.jsonl"}},
 {"label":"C","action":{"exec":{"program":"scripts/run.sh","args":["${selection}"],"timeoutSeconds":5}}}]}"#;

#[test]
fn mutated_manifests_never_panic() {
    assert!(Manifest::parse(GOOD).is_ok());
    let bytes = GOOD.as_bytes();
    let mut seed = 0x9e37_79b9u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let mut accepted = 0;
    for round in 0..4000 {
        let mut b = bytes.to_vec();
        match round % 4 {
            0 => b.truncate(next() as usize % bytes.len()),
            1 => {
                let at = next() as usize % b.len();
                b[at] ^= 1 << (next() % 8);
            }
            2 => {
                let at = next() as usize % b.len();
                b[at] = (next() & 0xff) as u8;
            }
            _ => {
                let at = next() as usize % b.len();
                let n = (next() % 20) as usize;
                b.splice(at..at, std::iter::repeat_n(b'[', n));
            }
        }
        let text = String::from_utf8_lossy(&b).to_string();
        let r = catch_unwind(|| Manifest::parse(&text).is_ok());
        assert!(r.is_ok(), "panic on round {round}: {text}");
        accepted += r.unwrap() as u32;
    }
    assert!(accepted < 4000, "some mutations must be refused");
}

#[test]
fn hostile_ids_and_paths_are_refused_by_the_store() {
    let root = temp("store");
    let store = ExtensionStore::new(root.join("extensions"));
    let probe = root.join("precious");
    std::fs::write(&probe, "keep me").unwrap();
    for id in ["..", "../precious", "../../etc", "a/../..", "/", "", "\0", "x\u{202e}y", &"a".repeat(1000), "CON:", "a b", "é"] {
        assert!(catch_unwind(|| store.remove(id)).is_ok());
        assert!(store.remove(id).is_err(), "{id:?}");
        assert!(store.get(id).is_err(), "{id:?}");
    }
    assert_eq!(std::fs::read_to_string(&probe).unwrap(), "keep me");
    for path in ["", "/", "\0", "/dev/null", "/nonexistent/x.fcext", ".."] {
        assert!(catch_unwind(|| store.install(Path::new(path))).is_ok(), "{path:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn mutated_archives_never_panic_or_escape() {
    let dir = temp("zip");
    let src = dir.join("src");
    std::fs::create_dir_all(src.join("scripts")).unwrap();
    std::fs::write(src.join("extension.json"), GOOD).unwrap();
    std::fs::write(src.join("steps.jsonl"), "{\"id\":\"edit.undo\"}\n").unwrap();
    std::fs::write(src.join("scripts/run.sh"), "#!/bin/sh\n").unwrap();
    let archive = dir.join("demo.fcext");
    pack::pack_dir(&src, &archive).unwrap();
    let good = std::fs::read(&archive).unwrap();
    let mut seed = 12345u32;
    for round in 0..600 {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let mut b = good.clone();
        let at = (seed >> 8) as usize % b.len();
        match round % 3 {
            0 => b.truncate(at),
            1 => b[at] ^= 1 << (seed % 8),
            _ => {
                let end = (at + 16).min(b.len());
                b[at..end].fill((seed & 0xff) as u8);
            }
        }
        let bad = dir.join("mutated.fcext");
        std::fs::write(&bad, &b).unwrap();
        let dest = temp("dest");
        assert!(catch_unwind(|| pack::stage(&bad, &dest)).is_ok(), "panic on round {round}");
        // whatever was extracted stayed inside the destination
        assert!(!dir.join("scripts").exists() && !dir.join("steps.jsonl").exists());
        let _ = std::fs::remove_dir_all(&dest);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_bundled_extensions_are_valid_and_complete() {
    let bundled = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions-bundled");
    let mut seen = Vec::new();
    for e in std::fs::read_dir(&bundled).unwrap().filter_map(|e| e.ok()).filter(|e| e.path().is_dir()) {
        let dir = e.path();
        let m = Manifest::load(&dir).unwrap_or_else(|err| panic!("{}: {err}", dir.display()));
        assert_eq!(m.id, e.file_name().to_string_lossy(), "folder name = id");
        pack::check_files(&m, &dir).unwrap_or_else(|err| panic!("{}: {err}", dir.display()));
        assert!(!m.description.is_empty() && !m.entries.is_empty());
        // only the Storyboard Maker starts a program (a script of its own folder: check_files above
        // proved it exists); every other bundled extension is a plain list of commands
        assert_eq!(m.runs_programs(), m.id == "storyboard-maker" || m.id == "doublage-voix", "{}: runs_programs", m.id);
        seen.push(m.id);
    }
    seen.sort();
    assert_eq!(
        seen,
        [
            "autoedit",
            "beat-edit",
            "copy-pasta",
            "export-en-un-clic",
            "silence-remover",
            "sous-titres",
            "sous-titres-animes",
            "storyboard-maker",
            "tics-de-langage",
            "traduction-sous-titres"
        ]
    );
}
