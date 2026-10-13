//! Background index of a resource folder (LUTs, sound effects, music) for Window ▸ Resources.
//!
//! The libraries live on slow disks (Madson's NAS is a USB 2 drive: a recursive scan takes many
//! minutes), so nothing here runs on the UI thread and nothing ever rescans what has not changed:
//!
//! - the index is cached as JSON in the data directory (`assets/index-<kind>.json`) and shown at
//!   once on the next launch;
//! - a walk in a background thread re-lists a folder only when its modification time changed
//!   (adding or removing a file changes it), and still visits the sub-folders it already knew;
//! - the walk publishes what it has every few seconds and saves the cache as it goes, so a walk
//!   stopped half-way (app closed) resumes from what it had;
//! - for sounds, an existing SQLite index of the same library (`audio_index.db` of the `/sfx`
//!   tool: path, duration, family) seeds the first index, read with the system `sqlite3` tool, so
//!   40 000 sounds are searchable in seconds while the walk confirms them.
//!
//! Folders whose name starts with `.` or `_` (Adobe Bridge caches, preview folders) are skipped.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// What a library holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Kind {
    Luts,
    Sfx,
    Music,
    Titles,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Luts, Kind::Sfx, Kind::Music, Kind::Titles];
    pub fn id(self) -> &'static str {
        match self {
            Kind::Luts => "luts",
            Kind::Sfx => "sfx",
            Kind::Music => "music",
            Kind::Titles => "titles",
        }
    }
    fn wants(self, name: &str) -> bool {
        let ext = Path::new(name).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        match self {
            Kind::Luts => ext == "cube" || ext == "3dl",
            Kind::Sfx | Kind::Music => filmcraft_engine::assets::AssetKind::of(name) == filmcraft_engine::assets::AssetKind::Sound,
            Kind::Titles => false,
        }
    }
}

/// One resource of a library.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    /// The file dropped on the timeline.
    pub path: String,
    /// File name (shown, searched).
    pub name: String,
    /// Folder relative to the library root (searched), or the pack of a title.
    pub dir: String,
    /// Seconds, when known (sound index, title definition).
    pub dur: Option<f32>,
    /// Family (sound index) or authoring app (titles).
    pub family: Option<String>,
    /// Titles: the `.mogrt` this entry comes from.
    pub source: Option<String>,
    /// Titles: a video with transparency exists (`path` is it); false = `path` is only a preview.
    pub ready: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
struct FileRec {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    dur: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    family: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
struct DirRec {
    /// Modification time (seconds); 0 = unknown (seeded), listed again by the walk.
    mtime: u64,
    subdirs: Vec<String>,
    files: Vec<FileRec>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
struct Cache {
    version: u32,
    root: String,
    dirs: BTreeMap<String, DirRec>,
}

const CACHE_VERSION: u32 = 1;

/// What the panel reads: the entries so far and the walk's progress.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub entries: Arc<Vec<Entry>>,
    /// Bumped at each publication.
    pub rev: u64,
    pub dirs_seen: usize,
    pub running: bool,
    pub complete: bool,
    pub error: Option<String>,
}

/// A library being indexed in the background.
pub struct Library {
    pub kind: Kind,
    pub root: String,
    shared: Arc<Mutex<Snapshot>>,
    cancel: Arc<AtomicBool>,
}

impl Drop for Library {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Library {
    /// Start indexing `root` (a walk in a background thread; `cache` = the cache file, `seed_db`
    /// = an `audio_index.db` for sounds). Returns at once.
    pub fn start(kind: Kind, root: &str, cache: Option<PathBuf>, seed_db: Option<PathBuf>) -> Library {
        let shared = Arc::new(Mutex::new(Snapshot { running: true, ..Default::default() }));
        let cancel = Arc::new(AtomicBool::new(false));
        let lib = Library { kind, root: root.to_string(), shared: shared.clone(), cancel: cancel.clone() };
        let root = root.to_string();
        let spawned = std::thread::Builder::new().name(format!("assets-index-{}", kind.id())).spawn(move || {
            let r = if kind == Kind::Titles { load_titles(&root, &shared) } else { walk(kind, &root, cache.as_deref(), seed_db.as_deref(), &shared, &cancel) };
            let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
            s.running = false;
            s.complete = r.is_ok() && !cancel.load(Ordering::Relaxed);
            s.error = r.err();
            s.rev += 1;
        });
        if let Err(e) = spawned {
            let mut s = lib.shared.lock().unwrap_or_else(|e| e.into_inner());
            s.running = false;
            s.error = Some(e.to_string());
        }
        lib
    }

    pub fn snapshot(&self) -> Snapshot {
        self.shared.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Block until the walk is over (tests, scripted runs).
    pub fn wait(&self, timeout: std::time::Duration) -> Snapshot {
        let t0 = web_time::Instant::now();
        loop {
            let s = self.snapshot();
            if !s.running || t0.elapsed() > timeout {
                return s;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}

fn join(rel: &str, name: &str) -> String {
    if rel.is_empty() { name.to_string() } else { format!("{rel}/{name}") }
}

fn mtime_of(m: &std::fs::Metadata) -> u64 {
    m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs().max(1)).unwrap_or(1)
}

fn entries_of(root: &str, dirs: &BTreeMap<String, DirRec>) -> Vec<Entry> {
    let mut out = Vec::new();
    for (rel, d) in dirs {
        for f in &d.files {
            out.push(Entry {
                path: Path::new(root).join(join(rel, &f.name)).to_string_lossy().into_owned(),
                name: f.name.clone(),
                dir: rel.clone(),
                dur: f.dur,
                family: f.family.clone(),
                source: None,
                ready: true,
            });
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

fn publish(shared: &Mutex<Snapshot>, root: &str, dirs: &BTreeMap<String, DirRec>, seen: usize) {
    let entries = Arc::new(entries_of(root, dirs));
    let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
    s.entries = entries;
    s.dirs_seen = seen;
    s.rev += 1;
}

fn save(path: Option<&Path>, cache: &Cache) {
    let Some(path) = path else { return };
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = path.with_extension("json.tmp");
    if let Ok(b) = serde_json::to_vec(cache)
        && std::fs::write(&tmp, b).is_ok()
    {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// Rows `path<TAB>duration<TAB>family` of the sound index whose path is under `root`.
fn seed_from_db(db: &Path, root: &str) -> BTreeMap<String, DirRec> {
    let mut dirs: BTreeMap<String, DirRec> = BTreeMap::new();
    let tool = ["/usr/bin/sqlite3", "/opt/homebrew/bin/sqlite3", "sqlite3"].into_iter().find(|t| !t.starts_with('/') || Path::new(t).exists());
    let Some(tool) = tool else { return dirs };
    let out = std::process::Command::new(tool)
        .arg("-readonly")
        .arg("-noheader")
        .arg("-separator")
        .arg("\t")
        .arg(db)
        .arg("select path, coalesce(duree, ''), coalesce(famille, '') from sons")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(out) = out else { return dirs };
    let prefix = format!("{}/", root.trim_end_matches('/'));
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut it = line.split('\t');
        let (Some(path), dur, fam) = (it.next(), it.next(), it.next()) else { continue };
        let Some(rel) = path.strip_prefix(&prefix) else { continue };
        let (dir, name) = match rel.rsplit_once('/') {
            Some((d, n)) => (d.to_string(), n.to_string()),
            None => (String::new(), rel.to_string()),
        };
        if dir.split('/').any(|c| c.starts_with('.') || c.starts_with('_')) {
            continue;
        }
        let rec = dirs.entry(dir).or_default();
        rec.files.push(FileRec { name, dur: dur.and_then(|d| d.parse().ok()), family: fam.filter(|f| !f.is_empty()).map(str::to_string) });
    }
    dirs
}

fn list_dir(kind: Kind, abs: &Path, mtime: u64, old: Option<&DirRec>) -> std::io::Result<DirRec> {
    let mut rec = DirRec { mtime, ..Default::default() };
    for e in std::fs::read_dir(abs)? {
        let Ok(e) = e else { continue };
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if !name.starts_with('_') && name != "__MACOSX" {
                rec.subdirs.push(name);
            }
        } else if kind.wants(&name) {
            let known = old.and_then(|o| o.files.iter().find(|f| f.name == name));
            rec.files.push(FileRec { dur: known.and_then(|f| f.dur), family: known.and_then(|f| f.family.clone()), name });
        }
    }
    rec.subdirs.sort();
    Ok(rec)
}

fn walk(kind: Kind, root: &str, cache_path: Option<&Path>, seed_db: Option<&Path>, shared: &Mutex<Snapshot>, cancel: &AtomicBool) -> Result<(), String> {
    let mut cache = cache_path
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<Cache>(&b).ok())
        .filter(|c| c.version == CACHE_VERSION && c.root == root)
        .unwrap_or_else(|| Cache { version: CACHE_VERSION, root: root.to_string(), dirs: BTreeMap::new() });
    if cache.dirs.is_empty()
        && let Some(db) = seed_db.filter(|d| d.exists())
    {
        cache.dirs = seed_from_db(db, root);
    }
    publish(shared, root, &cache.dirs, 0);
    let root_path = Path::new(root);
    std::fs::metadata(root_path).map_err(|e| format!("{root}: {e}"))?;
    let mut fresh: BTreeMap<String, DirRec> = BTreeMap::new();
    let mut stack = vec![String::new()];
    let mut last = web_time::Instant::now();
    while let Some(rel) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        let abs = root_path.join(&rel);
        let Ok(meta) = std::fs::metadata(&abs) else { continue };
        let mtime = mtime_of(&meta);
        let old = cache.dirs.get(&rel);
        let rec = match old {
            Some(o) if o.mtime == mtime => o.clone(),
            _ => match list_dir(kind, &abs, mtime, old) {
                Ok(r) => r,
                Err(_) => continue,
            },
        };
        for s in rec.subdirs.iter().rev() {
            stack.push(join(&rel, s));
        }
        fresh.insert(rel, rec);
        if last.elapsed() > std::time::Duration::from_secs(3) {
            last = web_time::Instant::now();
            // what is known so far: the folders walked, and the cached ones not reached yet
            let mut merged = cache.dirs.clone();
            merged.extend(fresh.iter().map(|(k, v)| (k.clone(), v.clone())));
            publish(shared, root, &merged, fresh.len());
            save(cache_path, &Cache { dirs: merged, ..cache.clone() });
        }
    }
    cache.dirs = fresh;
    publish(shared, root, &cache.dirs, cache.dirs.len());
    save(cache_path, &cache);
    Ok(())
}

/// The converted-titles index written by `mogrt_to_alpha.py index|render` (`<root>/index.json`).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct TitleRec {
    source: String,
    pack: String,
    name: String,
    video: Option<String>,
    preview: Option<String>,
    thumb: Option<String>,
    duration: Option<f32>,
    app: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct TitleIndex {
    items: Vec<TitleRec>,
}

fn load_titles(root: &str, shared: &Mutex<Snapshot>) -> Result<(), String> {
    let p = Path::new(root).join("index.json");
    let b = std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    let idx: TitleIndex = serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", p.display()))?;
    let mut entries: Vec<Entry> = idx
        .items
        .into_iter()
        .filter_map(|t| {
            let video = t.video.filter(|v| Path::new(v).exists());
            let ready = video.is_some();
            let path = video.or(t.preview).or(t.thumb)?;
            Some(Entry { path, name: t.name, dir: t.pack, dur: t.duration, family: t.app, source: Some(t.source), ready })
        })
        .collect();
    entries.sort_by(|a, b| (!a.ready, a.dir.to_lowercase(), a.name.to_lowercase()).cmp(&(!b.ready, b.dir.to_lowercase(), b.name.to_lowercase())));
    let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
    s.dirs_seen = 1;
    s.entries = Arc::new(entries);
    s.rev += 1;
    Ok(())
}

/// Entries matching every word of `query` (in the name or the folder, case-insensitive).
pub fn search(entries: &[Entry], query: &str) -> Vec<usize> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return (0..entries.len()).collect();
    }
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            let hay = format!("{} {}", e.name, e.dir).to_lowercase();
            words.iter().all(|w| hay.contains(w.as_str()))
        })
        .map(|(i, _)| i)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let d = std::env::temp_dir().join(format!("fc-assets-{name}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_walk_finds_the_files_skips_caches_and_the_next_walk_reuses_unchanged_folders() {
        let root = tmp("walk");
        std::fs::create_dir_all(root.join("01 HITS/sub")).unwrap();
        std::fs::create_dir_all(root.join("_Mister Horse Previews")).unwrap();
        for f in ["01 HITS/boom.wav", "01 HITS/sub/clap.mp3", "_Mister Horse Previews/skip.wav", "notes.txt", "riser.aiff"] {
            std::fs::write(root.join(f), b"x").unwrap();
        }
        let cache = root.join("cache/index-sfx.json");
        let r = root.to_string_lossy().into_owned();
        let lib = Library::start(Kind::Sfx, &r, Some(cache.clone()), None);
        let s = lib.wait(std::time::Duration::from_secs(20));
        assert!(s.complete, "{s:?}");
        let mut names: Vec<&str> = s.entries.iter().map(|e| e.name.as_str()).collect();
        names.sort();
        assert_eq!(names, ["boom.wav", "clap.mp3", "riser.aiff"]);
        assert!(cache.exists());
        // the cache answers before any disk walk; an unchanged folder is not listed again: a file
        // slipped in without changing the folder's time stays unseen, a new folder is seen
        let c: Cache = serde_json::from_slice(&std::fs::read(&cache).unwrap()).unwrap();
        assert_eq!(c.dirs.len(), 3);
        let mut c2 = c.clone();
        c2.dirs.get_mut("01 HITS").unwrap().files.push(FileRec { name: "ghost.wav".into(), ..Default::default() });
        std::fs::write(&cache, serde_json::to_vec(&c2).unwrap()).unwrap();
        let lib = Library::start(Kind::Sfx, &r, Some(cache.clone()), None);
        let s = lib.wait(std::time::Duration::from_secs(20));
        assert!(s.entries.iter().any(|e| e.name == "ghost.wav"), "unchanged folder reused from the cache");
        // the search matches every word, in the name or the folder
        let hits = search(&s.entries, "hits CLAP");
        assert_eq!(hits.len(), 1);
        assert_eq!(s.entries[hits[0]].name, "clap.mp3");
        assert_eq!(search(&s.entries, "").len(), s.entries.len());
    }

    #[test]
    fn a_missing_root_is_an_error_not_a_hang() {
        let lib = Library::start(Kind::Luts, "/nonexistent/fc-assets-root", None, None);
        let s = lib.wait(std::time::Duration::from_secs(5));
        assert!(!s.running && s.error.is_some(), "{s:?}");
    }

    #[test]
    fn the_sound_database_seeds_the_index_with_durations() {
        if !Path::new("/usr/bin/sqlite3").exists() {
            return;
        }
        let root = tmp("seed");
        std::fs::create_dir_all(root.join("A")).unwrap();
        std::fs::write(root.join("A/x.wav"), b"x").unwrap();
        let db = root.join("audio_index.db");
        let sql = format!(
            "create table sons(path text primary key, duree real, famille text); insert into sons values('{}/A/x.wav', 1.5, 'one-shot / impact'); insert into sons values('/elsewhere/y.wav', 2, 'x');",
            root.display()
        );
        let ok = std::process::Command::new("/usr/bin/sqlite3").arg(&db).arg(sql).status().unwrap();
        assert!(ok.success());
        let seeded = seed_from_db(&db, &root.to_string_lossy());
        assert_eq!(seeded.len(), 1);
        let lib = Library::start(Kind::Sfx, &root.to_string_lossy(), None, Some(db));
        let s = lib.wait(std::time::Duration::from_secs(20));
        let e = s.entries.iter().find(|e| e.name == "x.wav").expect("entry");
        assert_eq!(e.dur, Some(1.5), "duration kept from the database after the walk");
        assert_eq!(e.family.as_deref(), Some("one-shot / impact"));
    }

    #[test]
    fn titles_come_from_the_converter_index_ready_ones_first() {
        let root = tmp("titles");
        let video = root.join("Pack/Title A.mov");
        std::fs::create_dir_all(video.parent().unwrap()).unwrap();
        std::fs::write(&video, b"x").unwrap();
        let idx = serde_json::json!({"version": 1, "items": [
            {"source": "/lib/Pack/Title B.mogrt", "pack": "Pack", "name": "Title B", "video": null, "preview": "/x/b.mp4", "duration": 3.0, "app": "aefx"},
            {"source": "/lib/Pack/Title A.mogrt", "pack": "Pack", "name": "Title A", "video": video.to_string_lossy(), "preview": "/x/a.mp4", "duration": 2.0, "app": "aefx"},
            {"source": "/lib/Pack/Title C.mogrt", "pack": "Pack", "name": "Title C"}
        ]});
        std::fs::write(root.join("index.json"), idx.to_string()).unwrap();
        let lib = Library::start(Kind::Titles, &root.to_string_lossy(), None, None);
        let s = lib.wait(std::time::Duration::from_secs(5));
        assert_eq!(s.entries.len(), 2, "an entry with nothing to show is left out");
        assert!(s.entries[0].ready && s.entries[0].name == "Title A");
        assert_eq!(s.entries[1].path, "/x/b.mp4");
        assert_eq!(s.entries[1].source.as_deref(), Some("/lib/Pack/Title B.mogrt"));
    }
}
