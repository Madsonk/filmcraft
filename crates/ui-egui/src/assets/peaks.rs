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

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

/// Columns of a waveform thumbnail.
pub const COLUMNS: usize = 1000;
const MAGIC: &[u8; 4] = b"FCPK";
const PACK_MAGIC: &[u8; 8] = b"FCPKPAK1";

/// A waveform thumbnail: min/max per column, in [-1, 1].
#[derive(Clone, Debug, PartialEq)]
pub struct Peaks {
    pub duration: f32,
    pub cols: Vec<(i8, i8)>,
}

impl Peaks {
    /// The `FCPK` blob.
    pub fn to_blob(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(12 + self.cols.len() * 2);
        b.extend_from_slice(MAGIC);
        b.push(1);
        b.push(1);
        b.extend_from_slice(&(self.cols.len() as u16).to_le_bytes());
        b.extend_from_slice(&self.duration.to_le_bytes());
        for (lo, hi) in &self.cols {
            b.push(*lo as u8);
            b.push(*hi as u8);
        }
        b
    }

    /// Read a `FCPK` blob (None if it is not one).
    pub fn from_blob(b: &[u8]) -> Option<Peaks> {
        if b.len() < 12 || &b[..4] != MAGIC || b[4] != 1 {
            return None;
        }
        let n = u16::from_le_bytes([b[6], b[7]]) as usize;
        let duration = f32::from_le_bytes([b[8], b[9], b[10], b[11]]);
        let body = b.get(12..12 + n * 2)?;
        let cols = body.as_chunks::<2>().0.iter().map(|c| (c[0] as i8, c[1] as i8)).collect();
        duration.is_finite().then_some(Peaks { duration, cols })
    }

    /// Min/max of the columns falling in `[a, b)` (fractions of the duration), in [-1, 1].
    pub fn range(&self, a: f32, b: f32) -> (f32, f32) {
        let n = self.cols.len();
        if n == 0 {
            return (0.0, 0.0);
        }
        let i0 = ((a.clamp(0.0, 1.0) * n as f32) as usize).min(n - 1);
        let i1 = ((b.clamp(0.0, 1.0) * n as f32).ceil() as usize).clamp(i0 + 1, n);
        let (mut lo, mut hi) = (0i8, 0i8);
        for (l, h) in &self.cols[i0..i1] {
            lo = lo.min(*l);
            hi = hi.max(*h);
        }
        (lo as f32 / 127.0, hi as f32 / 127.0)
    }
}

/// Min/max per column of planar samples (any number of channels), `total` frames in all,
/// `offset` = index of the first frame of `chunk` in the file.
fn accumulate(cols: &mut [(f32, f32)], chunk: &[Vec<f32>], offset: u64, total: u64) {
    let n = cols.len() as u64;
    let frames = chunk.iter().map(Vec::len).min().unwrap_or(0);
    for i in 0..frames {
        let c = (((offset + i as u64) * n) / total.max(1)).min(n - 1) as usize;
        for ch in chunk {
            let v = ch[i];
            let e = &mut cols[c];
            e.0 = e.0.min(v);
            e.1 = e.1.max(v);
        }
    }
}

fn quantise(cols: &[(f32, f32)], duration: f32) -> Peaks {
    let q = |v: f32| (v.clamp(-1.0, 1.0) * 127.0).round() as i8;
    Peaks { duration, cols: cols.iter().map(|(lo, hi)| (q(*lo), q(*hi))).collect() }
}

/// Decode a sound through FilmCraft's media layer and make its thumbnail (reads the whole file).
pub fn compute(media: &filmcraft_engine::media_pool::MediaPool, services: &dyn filmcraft_engine::Services, path: &str) -> Result<Peaks, String> {
    let src = media.open_file(path, services).map_err(|e| e.to_string())?;
    let info = src.info().clone();
    let stream = info.audio_streams.first().ok_or_else(|| format!("{path}: no sound"))?;
    // decode at a reduced rate: the thumbnail needs 1 000 columns, not every sample
    let sr = stream.sample_rate.clamp(8_000, 22_050);
    let duration = info.duration.seconds() as f32;
    let total = ((info.duration.seconds()) * sr as f64).round().max(1.0) as u64;
    let mut cols = vec![(0f32, 0f32); COLUMNS];
    let step = sr as usize * 4;
    let mut at = 0u64;
    while at < total {
        let n = step.min((total - at) as usize);
        let buf = src.audio(at as i64, n, sr).map_err(|e| e.to_string())?;
        accumulate(&mut cols, &buf.channels, at, total);
        at += n as u64;
    }
    Ok(quantise(&cols, duration))
}

fn hash(path: &str) -> u64 {
    path.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

fn stamp(path: &str) -> Option<(u64, i64)> {
    let m = std::fs::metadata(path).ok()?;
    let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    Some((m.len(), t))
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

fn read_pack(path: &Path, map: &mut HashMap<u64, Rec>) {
    let Ok(mut f) = std::fs::File::open(path) else { return };
    let mut b = Vec::new();
    if f.read_to_end(&mut b).is_err() || b.len() < 8 || &b[..8] != PACK_MAGIC {
        return;
    }
    let mut i = 8;
    while i + 28 <= b.len() {
        let u64_at = |o: usize| u64::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3], b[o + 4], b[o + 5], b[o + 6], b[o + 7]]);
        let (h, size, mtime) = (u64_at(i), u64_at(i + 8), u64_at(i + 16) as i64);
        let len = u32::from_le_bytes([b[i + 24], b[i + 25], b[i + 26], b[i + 27]]) as usize;
        let Some(blob) = b.get(i + 28..i + 28 + len) else { break };
        if let Some(p) = Peaks::from_blob(blob) {
            map.insert(h, Rec { size, mtime, peaks: Arc::new(p) });
        }
        i += 28 + len;
    }
}

fn append(path: &Path, h: u64, size: u64, mtime: i64, p: &Peaks) -> std::io::Result<()> {
    let new = !path.exists();
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    let blob = p.to_blob();
    let mut rec = Vec::with_capacity(36 + blob.len());
    if new {
        rec.extend_from_slice(PACK_MAGIC);
    }
    rec.extend_from_slice(&h.to_le_bytes());
    rec.extend_from_slice(&size.to_le_bytes());
    rec.extend_from_slice(&mtime.to_le_bytes());
    rec.extend_from_slice(&(blob.len() as u32).to_le_bytes());
    rec.extend_from_slice(&blob);
    f.write_all(&rec)
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
            read_pack(&pack, &mut map);
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
                let skip = match shared.map.read().unwrap_or_else(|e| e.into_inner()).get(&h) {
                    None => false,
                    Some(_) if !background => true,
                    Some(r) => r.size == 0 || st.is_none_or(|(size, mtime)| r.size == size && r.mtime == mtime),
                };
                if skip {
                    continue;
                }
                let t0 = web_time::Instant::now();
                match compute(&media, &*services, &path) {
                    Ok(p) => {
                        let (size, mtime) = st.unwrap_or((0, 0));
                        let _ = append(&pack, h, size, mtime, &p);
                        shared.map.write().unwrap_or_else(|e| e.into_inner()).insert(h, Rec { size, mtime, peaks: Arc::new(p) });
                        shared.computed.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => {
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
                if !m.contains_key(&h) && !failed.contains(&h) {
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

    /// The thumbnail of `path`, if known.
    pub fn get(&self, path: &str) -> Option<Arc<Peaks>> {
        self.shared.map.read().unwrap_or_else(|e| e.into_inner()).get(&hash(path)).map(|r| r.peaks.clone())
    }

    /// The rows on screen this frame (first served, nearest the top last in the list so popped first).
    pub fn want_visible(&self, paths: Vec<String>) {
        let failed = self.shared.failed.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let missing: Vec<String> = {
            let m = self.shared.map.read().unwrap_or_else(|e| e.into_inner());
            paths.into_iter().rev().filter(|p| !m.contains_key(&hash(p)) && !failed.contains(&hash(p))).collect()
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

    #[test]
    fn columns_follow_the_signal() {
        // a sound silent in its first half and loud in its second
        let total = 2000u64;
        let ch: Vec<f32> = (0..total)
            .map(|i| {
                if i < 1000 {
                    0.0
                } else if i % 2 == 0 {
                    0.5
                } else {
                    -0.5
                }
            })
            .collect();
        let mut cols = vec![(0f32, 0f32); 10];
        accumulate(&mut cols, &[ch[..700].to_vec()], 0, total);
        accumulate(&mut cols, &[ch[700..].to_vec()], 700, total);
        let p = quantise(&cols, 2.0);
        assert_eq!(p.cols[0], (0, 0));
        assert_eq!(p.cols[9], (-64, 64));
    }

    #[test]
    fn the_pack_keeps_the_last_record_of_a_path() {
        let d = std::env::temp_dir().join(format!("fc-peaks-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&d);
        let pack = d.join("peaks-v1.bin");
        append(&pack, 1, 10, 20, &Peaks { duration: 1.0, cols: vec![(0, 1)] }).unwrap();
        append(&pack, 2, 10, 20, &Peaks { duration: 2.0, cols: vec![(0, 2)] }).unwrap();
        append(&pack, 1, 11, 21, &Peaks { duration: 3.0, cols: vec![(0, 3)] }).unwrap();
        let mut m = HashMap::new();
        read_pack(&pack, &mut m);
        assert_eq!(m.len(), 2);
        assert_eq!(m[&1].peaks.duration, 3.0);
        assert_eq!((m[&1].size, m[&1].mtime), (11, 21));
    }
}
