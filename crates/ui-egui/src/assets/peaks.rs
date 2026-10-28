//! Waveform thumbnails of the Resources panel (ressources-v2, L1): computed once per sound, kept on
//! disk, drawn in every row.
//!
//! - **Format** (shared with `~/audio-tools/index_build.py`, column `peaks` of `audio_index.db`):
//!   `b"FCPK"`, version `1`, channels `1`, `u16` columns, `f32` duration in seconds, then per column
//!   `(i8 min, i8 max)` of every channel (±127 = full scale). 1 000 columns ≈ 2 KB per sound.
//! - **Store**: one append-only pack file per cache folder (`peaks-v1.bin`: records of path hash,
//!   file size, modification time and the blob), read whole in the background at start; a later
//!   record of the same path wins. 30 000 sounds ≈ 60 MB, on `/Volumes/Data` (never the NAS).
//! - **Worker** ([`PeakWorker`]): one thread, **one file read at a time** (the NAS is a USB 2
//!   disk: parallel reads slow everything down). Two queues: the rows on screen first (replaced
//!   at every frame the panel is drawn), then a background sweep of the rest. Paused while the
//!   timeline or the Source monitor plays. A file is computed again only when its size or
//!   modification time changed.
//! - Sounds the `/sfx` index already analysed bring their peaks from `audio_index.db` (read with
//!   the system `sqlite3`, read only): the NAS is read once for them.
//! - **Previews** (previews-cache): when a preview store is given, the same single reading also
//!   makes the sound's light preview ([`filmcraft_engine::sound_previews`]); a sound whose peaks
//!   are known but whose preview is missing is read once more for it.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

pub use filmcraft_engine::sound_previews::{COLUMNS, Peaks, PreviewKind, PreviewStore};
use filmcraft_engine::sound_previews::{append_pack, hash, read_pack, stamp};

/// Make the thumbnail of a sound (reads the whole file; no preview).
pub fn compute(media: &filmcraft_engine::media_pool::MediaPool, services: &dyn filmcraft_engine::Services, path: &str) -> Result<Peaks, String> {
    filmcraft_engine::sound_previews::analyse(media, services, path, None).map(|a| a.peaks)
}

#[derive(Clone, Debug)]
struct Rec {
    size: u64,
    mtime: i64,
    peaks: Arc<Peaks>,
}

/// Progress of the store, for the panel's footer.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PeakStats {
    pub known: usize,
    pub computed: usize,
    pub failed: usize,
    pub queued: usize,
    pub loaded: bool,
    /// Milliseconds the last computed file took (decode included).
    pub last_ms: u64,
    /// Rows on screen still waiting for their waveform.
    pub visible_pending: usize,
}

struct Shared {
    map: RwLock<HashMap<u64, Rec>>,
    visible: Mutex<Vec<String>>,
    background: Mutex<VecDeque<String>>,
    queued_bg: Mutex<HashSet<u64>>,
    failed: Mutex<HashSet<u64>>,
    /// The last few files that could not be read, with the reason.
    errors: Mutex<Vec<(String, String)>>,
    paused: AtomicBool,
    quit: AtomicBool,
    loaded: AtomicBool,
    computed: AtomicUsize,
    failed_n: AtomicUsize,
    last_ms: AtomicUsize,
    wake: (Mutex<bool>, std::sync::Condvar),
    repaint: Mutex<Option<egui::Context>>,
    /// The light previews made in the same reading (none = waveforms only).
    previews: Option<Arc<PreviewStore>>,
    /// Files under this folder are music (preview = an excerpt), the others sound effects.
    music_root: String,
}

impl Shared {
    fn kind_of(&self, path: &str) -> PreviewKind {
        if !self.music_root.is_empty() && path.starts_with(&self.music_root) { PreviewKind::Music } else { PreviewKind::Sfx }
    }

    /// Whether a row on screen still needs a reading: no waveform, or no preview yet (a cheap
    /// test: the index is not asked for the file's date, which would read the slow disk).
    fn wants(&self, m: &HashMap<u64, Rec>, path: &str) -> bool {
        if self.previews.as_ref().is_some_and(|p| p.failed_before(path)) {
            return false; // not a readable sound: recorded once, not read again at every session
        }
        !m.contains_key(&hash(path)) || self.previews.as_ref().is_some_and(|p| !p.knows(path))
    }
}

/// Waveform thumbnails of the libraries: the store and its single worker thread.
pub struct PeakWorker {
    shared: Arc<Shared>,
    pub dir: PathBuf,
}

impl Drop for PeakWorker {
    fn drop(&mut self) {
        self.shared.quit.store(true, Ordering::Relaxed);
        self.poke();
    }
}

/// Peaks stored in `audio_index.db` (column `peaks`), by path. Missing column or tool = none.
fn seed_from_db(db: &Path) -> Vec<(String, Peaks)> {
    let tool = ["/usr/bin/sqlite3", "/opt/homebrew/bin/sqlite3"].into_iter().find(|t| Path::new(t).exists());
    let Some(tool) = tool else { return Vec::new() };
    let out = std::process::Command::new(tool)
        .arg("-readonly")
        .arg("-noheader")
        .arg("-separator")
        .arg("\t")
        .arg(db)
        .arg("select path, hex(peaks) from sons where peaks is not null")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(out) = out else { return Vec::new() };
    let unhex = |s: &str| -> Option<Vec<u8>> { (0..s.len() / 2).map(|i| u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()).collect() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let (p, h) = l.split_once('\t')?;
            Some((p.to_string(), Peaks::from_blob(&unhex(h.trim())?)?))
        })
        .collect()
}

impl PeakWorker {
    /// Start the store (pack file in `dir`, seeded from `db` when given) and its worker.
    pub fn start(
        dir: PathBuf,
        db: Option<PathBuf>,
        media: Arc<filmcraft_engine::media_pool::MediaPool>,
        services: Arc<dyn filmcraft_engine::Services>,
        previews: Option<Arc<PreviewStore>>,
        music_root: String,
    ) -> PeakWorker {
        let shared = Arc::new(Shared {
            map: RwLock::new(HashMap::new()),
            visible: Mutex::new(Vec::new()),
            background: Mutex::new(VecDeque::new()),
            queued_bg: Mutex::new(HashSet::new()),
            failed: Mutex::new(HashSet::new()),
            errors: Mutex::new(Vec::new()),
            paused: AtomicBool::new(false),
            quit: AtomicBool::new(false),
            loaded: AtomicBool::new(false),
            computed: AtomicUsize::new(0),
            failed_n: AtomicUsize::new(0),
            last_ms: AtomicUsize::new(0),
            wake: (Mutex::new(false), std::sync::Condvar::new()),
            repaint: Mutex::new(None),
            previews,
            music_root,
        });
        let w = PeakWorker { shared: shared.clone(), dir: dir.clone() };
        let pack = dir.join("peaks-v1.bin");
        let _ = std::thread::Builder::new().name("resources-peaks".into()).spawn(move || {
            // load: the database first, our own pack over it (later = fresher)
            let mut map = HashMap::new();
            if let Some(db) = db.filter(|d| d.exists()) {
                for (p, pk) in seed_from_db(&db) {
                    map.insert(hash(&p), Rec { size: 0, mtime: 0, peaks: Arc::new(pk) });
                }
            }
            let mut packed = HashMap::new();
            read_pack(&pack, &mut packed);
            map.extend(packed.into_iter().map(|(h, r)| (h, Rec { size: r.size, mtime: r.mtime, peaks: Arc::new(r.peaks) })));
            *shared.map.write().unwrap_or_else(|e| e.into_inner()) = map;
            shared.loaded.store(true, Ordering::Relaxed);
            Self::repaint(&shared);
            loop {
                if shared.quit.load(Ordering::Relaxed) {
                    return;
                }
                let next = if shared.paused.load(Ordering::Relaxed) { None } else { Self::next(&shared) };
                let Some((path, background)) = next else {
                    let (lock, cv) = &shared.wake;
                    let mut woken = lock.lock().unwrap_or_else(|e| e.into_inner());
                    if !*woken {
                        woken = cv.wait_timeout(woken, std::time::Duration::from_millis(500)).map(|r| r.0).unwrap_or_else(|e| e.into_inner().0);
                    }
                    *woken = false;
                    continue;
                };
                let h = hash(&path);
                let st = stamp(&path);
                // a visible row is computed when unknown; the background sweep also re-checks a
                // known file and computes it again only if its size or date changed (records
                // from the sound database carry no size: trusted)
                let need_peaks = match shared.map.read().unwrap_or_else(|e| e.into_inner()).get(&h) {
                    None => true,
                    Some(_) if !background => false,
                    Some(r) => !(r.size == 0 || st.is_none_or(|(size, mtime)| r.size == size && r.mtime == mtime)),
                };
                let need_preview = shared.previews.as_ref().is_some_and(|p| match (background, st) {
                    (false, _) => !p.knows(&path),
                    (true, Some((size, mtime))) => p.record(&path, size, mtime).is_none(),
                    (true, None) => false,
                });
                if !need_peaks && !need_preview {
                    continue;
                }
                let t0 = web_time::Instant::now();
                // ONE reading of the file: the waveform and the preview together
                let previews = shared.previews.as_deref().map(|p| (p, shared.kind_of(&path)));
                match filmcraft_engine::sound_previews::analyse(&media, &*services, &path, previews) {
                    Ok(a) => {
                        let _ = append_pack(&pack, h, a.size, a.mtime, &a.peaks);
                        shared.map.write().unwrap_or_else(|e| e.into_inner()).insert(h, Rec { size: a.size, mtime: a.mtime, peaks: Arc::new(a.peaks) });
                        shared.computed.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => {
                        if let (Some(p), Some((size, mtime))) = (&shared.previews, st) {
                            let _ = p.record_failure(&path, size, mtime, &e);
                        }
                        let mut errs = shared.errors.lock().unwrap_or_else(|x| x.into_inner());
                        if errs.len() < 20 {
                            errs.push((path.clone(), e));
                        }
                        drop(errs);
                        shared.failed.lock().unwrap_or_else(|e| e.into_inner()).insert(h);
                        shared.failed_n.fetch_add(1, Ordering::Relaxed);
                    }
                }
                shared.last_ms.store(t0.elapsed().as_millis() as usize, Ordering::Relaxed);
                Self::repaint(&shared);
            }
        });
        w
    }

    fn repaint(s: &Shared) {
        if let Some(c) = s.repaint.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            c.request_repaint();
        }
    }

    /// The next file to read: a visible row without peaks, else the background queue.
    fn next(s: &Shared) -> Option<(String, bool)> {
        let failed = s.failed.lock().unwrap_or_else(|e| e.into_inner()).clone();
        {
            let mut v = s.visible.lock().unwrap_or_else(|e| e.into_inner());
            let m = s.map.read().unwrap_or_else(|e| e.into_inner());
            while let Some(p) = v.pop() {
                let h = hash(&p);
                if s.wants(&m, &p) && !failed.contains(&h) {
                    return Some((p, false));
                }
            }
        }
        let mut b = s.background.lock().unwrap_or_else(|e| e.into_inner());
        while let Some(p) = b.pop_front() {
            if !failed.contains(&hash(&p)) {
                return Some((p, true));
            }
        }
        None
    }

    fn poke(&self) {
        let (lock, cv) = &self.shared.wake;
        *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
        cv.notify_one();
    }

    pub fn set_repaint(&self, ctx: &egui::Context) {
        let mut r = self.shared.repaint.lock().unwrap_or_else(|e| e.into_inner());
        if r.is_none() {
            *r = Some(ctx.clone());
        }
    }

    /// The preview store fed by this worker (None = waveforms only).
    pub fn previews(&self) -> Option<Arc<PreviewStore>> {
        self.shared.previews.clone()
    }

    /// The thumbnail of `path`, if known.
    pub fn get(&self, path: &str) -> Option<Arc<Peaks>> {
        self.shared.map.read().unwrap_or_else(|e| e.into_inner()).get(&hash(path)).map(|r| r.peaks.clone())
    }

    /// The rows on screen this frame (first served, nearest the top last in the list so popped first).
    pub fn want_visible(&self, paths: Vec<String>) {
        let failed = self.shared.failed.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let missing: Vec<String> = {
            let m = self.shared.map.read().unwrap_or_else(|e| e.into_inner());
            paths.into_iter().rev().filter(|p| self.shared.wants(&m, p) && !failed.contains(&hash(p))).collect()
        };
        let changed = {
            let mut v = self.shared.visible.lock().unwrap_or_else(|e| e.into_inner());
            let c = *v != missing;
            *v = missing;
            c
        };
        if changed {
            self.poke();
        }
    }

    /// Queue `paths` for the background sweep (each once per session).
    pub fn want_background(&self, paths: impl IntoIterator<Item = String>) {
        let mut q = self.shared.queued_bg.lock().unwrap_or_else(|e| e.into_inner());
        let mut b = self.shared.background.lock().unwrap_or_else(|e| e.into_inner());
        for p in paths {
            if q.insert(hash(&p)) {
                b.push_back(p);
            }
        }
        drop((q, b));
        self.poke();
    }

    /// Pause the worker (timeline or Source monitor playing): the NAS is left to playback.
    pub fn set_paused(&self, paused: bool) {
        if self.shared.paused.swap(paused, Ordering::Relaxed) != paused && !paused {
            self.poke();
        }
    }

    pub fn stats(&self) -> PeakStats {
        // one lock at a time: two guards of the same mutex in one expression would deadlock
        let s = &self.shared;
        let known = s.map.read().unwrap_or_else(|e| e.into_inner()).len();
        let background = s.background.lock().unwrap_or_else(|e| e.into_inner()).len();
        let visible_pending = s.visible.lock().unwrap_or_else(|e| e.into_inner()).len();
        PeakStats {
            known,
            computed: s.computed.load(Ordering::Relaxed),
            failed: s.failed_n.load(Ordering::Relaxed),
            queued: background + visible_pending,
            loaded: s.loaded.load(Ordering::Relaxed),
            last_ms: s.last_ms.load(Ordering::Relaxed) as u64,
            visible_pending,
        }
    }

    /// Whether `path` could not be read (not a sound FilmCraft decodes, or damaged).
    pub fn failed(&self, path: &str) -> bool {
        self.shared.failed.lock().unwrap_or_else(|e| e.into_inner()).contains(&hash(path))
            || self.shared.previews.as_ref().is_some_and(|p| p.failed_before(path))
    }

    /// The last files that could not be read, with the reason.
    pub fn errors(&self) -> Vec<(String, String)> {
        self.shared.errors.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Block until nothing visible is pending (tests).
    pub fn wait_idle(&self, timeout: std::time::Duration) -> bool {
        let t0 = web_time::Instant::now();
        while t0.elapsed() < timeout {
            let st = self.stats();
            if st.loaded && st.queued == 0 {
                std::thread::sleep(std::time::Duration::from_millis(30));
                if self.stats().queued == 0 {
                    return true;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_roundtrip_and_ranges() {
        let p = Peaks { duration: 1.5, cols: vec![(-127, 127), (0, 0), (-10, 20)] };
        let b = p.to_blob();
        assert_eq!(b.len(), 12 + 6);
        assert_eq!(Peaks::from_blob(&b), Some(p.clone()));
        assert_eq!(Peaks::from_blob(b"nope"), None);
        let (lo, hi) = p.range(0.0, 1.0);
        assert!((lo + 1.0).abs() < 1e-6 && (hi - 1.0).abs() < 1e-6);
        let (lo, hi) = p.range(0.34, 0.66);
        assert_eq!((lo, hi), (0.0, 0.0));
    }
}
