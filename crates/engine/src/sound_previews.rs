//! Waveform thumbnails and light audio previews of the user's sound library (Window ▸ Resources).
//!
//! The library lives on a slow disk (a NAS on USB 2: 8-26 MB/s), so every sound is read **once**:
//! one decoding pass makes both its waveform thumbnail ([`Peaks`]) and, when asked, its **preview**:
//! a small mono AAC-LC file (32 kHz, 96 kbps, in an `.m4a` the media layer reads) kept on a local
//! disk. Listening then reads the preview instead of the original; dragging a sound onto the
//! timeline always uses the original.
//!
//! - **Peaks** (shared with `~/audio-tools/index_build.py`, column `peaks` of `audio_index.db`):
//!   `b"FCPK"`, version `1`, channels `1`, `u16` columns, `f32` duration in seconds, then per column
//!   `(i8 min, i8 max)` (±127 = full scale). Stored in an append-only pack (`peaks-v1.bin`).
//! - **Previews**: a sound effect whole; a music from 30 % of its length, [`MUSIC_SECONDS`] long.
//!   Files `<previews dir>/<hash>-<size>-<mtime>.m4a`, listed in `previews-v1.jsonl` (one record
//!   per line, later wins; failures recorded too, so a night run does not retry them). Key = the
//!   path, the size and the modification time of the original: a changed file is made again.
//! - Writes are atomic (temporary file + rename, then the record): an interrupted run (quota, power,
//!   Ctrl-C) leaves no half file and resumes where it stopped.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

use crate::Services;
use crate::media_pool::MediaPool;

/// Columns of a waveform thumbnail.
pub const COLUMNS: usize = 1000;
/// Length of a music preview, seconds.
pub const MUSIC_SECONDS: f64 = 30.0;
/// Where a music preview starts (fraction of its length).
pub const MUSIC_START: f64 = 0.30;
/// Sample rate of the previews.
pub const PREVIEW_RATE: u32 = 32_000;
/// Bit rate of the previews (mono).
pub const PREVIEW_BPS: u32 = 96_000;
const MAGIC: &[u8; 4] = b"FCPK";
const PACK_MAGIC: &[u8; 8] = b"FCPKPAK1";
/// Fade at the cut ends of a music preview, seconds.
const CUT_FADE: f64 = 0.015;

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
pub fn accumulate(cols: &mut [(f32, f32)], chunk: &[Vec<f32>], offset: u64, total: u64) {
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

pub fn quantise(cols: &[(f32, f32)], duration: f32) -> Peaks {
    let q = |v: f32| (v.clamp(-1.0, 1.0) * 127.0).round() as i8;
    Peaks { duration, cols: cols.iter().map(|(lo, hi)| (q(*lo), q(*hi))).collect() }
}

/// FNV-1a of a path: the key of a sound in the stores.
pub fn hash(path: &str) -> u64 {
    path.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

/// Size and modification time (seconds) of a file.
pub fn stamp(path: &str) -> Option<(u64, i64)> {
    let m = std::fs::metadata(path).ok()?;
    let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    Some((m.len(), t))
}

/// A record of the peaks pack.
#[derive(Clone, Debug)]
pub struct PackRec {
    pub size: u64,
    pub mtime: i64,
    pub peaks: Peaks,
}

/// Read a peaks pack into `map` (a later record of the same path wins).
pub fn read_pack(path: &Path, map: &mut HashMap<u64, PackRec>) {
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
            map.insert(h, PackRec { size, mtime, peaks: p });
        }
        i += 28 + len;
    }
}

/// Append one record to a peaks pack (created with its header when missing).
pub fn append_pack(path: &Path, h: u64, size: u64, mtime: i64, p: &Peaks) -> std::io::Result<()> {
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

/// Sound effect (whole preview) or music (an excerpt).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreviewKind {
    Sfx,
    Music,
}

/// The part of a sound its preview covers: (start, length) in seconds.
pub fn preview_window(duration: f64, kind: PreviewKind) -> (f64, f64) {
    let d = duration.max(0.0);
    match kind {
        PreviewKind::Sfx => (0.0, d),
        PreviewKind::Music => {
            // from 30 %, 30 s; a short piece keeps 30 s by starting earlier
            let start = (d * MUSIC_START).min((d - MUSIC_SECONDS).max(0.0));
            (start, MUSIC_SECONDS.min(d - start))
        }
    }
}

/// One line of `previews-v1.jsonl`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PreviewRec {
    pub key: u64,
    pub size: u64,
    pub mtime: i64,
    /// Seconds of the original where the preview starts, and its length.
    pub start: f64,
    pub len: f64,
    /// Length of the original, seconds.
    pub duration: f64,
    /// Bytes of the preview file (0 when it failed).
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<PreviewKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
}

/// The previews of a cache folder: the index loaded in memory.
pub struct PreviewStore {
    pub dir: PathBuf,
    map: RwLock<HashMap<u64, PreviewRec>>,
}

impl PreviewStore {
    /// Open (and read the index of) `dir`. Nothing is created until the first preview.
    pub fn open(dir: &Path) -> PreviewStore {
        let mut map = HashMap::new();
        if let Ok(t) = std::fs::read_to_string(dir.join("previews-v1.jsonl")) {
            for l in t.lines() {
                if let Ok(r) = serde_json::from_str::<PreviewRec>(l) {
                    map.insert(r.key, r);
                }
            }
        }
        PreviewStore { dir: dir.to_path_buf(), map: RwLock::new(map) }
    }

    fn file_of(&self, r: &PreviewRec) -> PathBuf {
        self.dir.join(format!("{:016x}-{:x}-{:x}.m4a", r.key, r.size, r.mtime))
    }

    /// The record of `path` for this size and date (a failure included), if any.
    pub fn record(&self, path: &str, size: u64, mtime: i64) -> Option<PreviewRec> {
        let m = self.map.read().unwrap_or_else(|e| e.into_inner());
        m.get(&hash(path)).filter(|r| r.size == size && r.mtime == mtime).cloned()
    }

    /// The preview file of `path` and its record, when one exists for the file as it is now.
    pub fn get(&self, path: &str) -> Option<(PathBuf, PreviewRec)> {
        let (size, mtime) = stamp(path)?;
        let r = self.record(path, size, mtime).filter(|r| r.failed.is_none())?;
        let f = self.file_of(&r);
        f.exists().then_some((f, r))
    }

    /// Previews made of this kind (failures excluded) and their total size in bytes.
    pub fn stats(&self, kind: PreviewKind) -> (usize, u64) {
        let m = self.map.read().unwrap_or_else(|e| e.into_inner());
        m.values().filter(|r| r.failed.is_none() && r.kind == Some(kind)).fold((0, 0), |(n, b), r| (n + 1, b + r.bytes))
    }

    /// Whether the last reading of `path` failed (any version of the file).
    pub fn failed_before(&self, path: &str) -> bool {
        self.map.read().unwrap_or_else(|e| e.into_inner()).get(&hash(path)).is_some_and(|r| r.failed.is_some())
    }

    /// Whether the index knows `path` at all (made or failed, any version): a cheap test for the
    /// rows on screen, without reading the file's date on the slow disk.
    pub fn knows(&self, path: &str) -> bool {
        self.map.read().unwrap_or_else(|e| e.into_inner()).contains_key(&hash(path))
    }

    fn record_line(&self, r: PreviewRec) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(self.dir.join("previews-v1.jsonl")).map_err(|e| e.to_string())?;
        let line = serde_json::to_string(&r).map_err(|e| e.to_string())?;
        writeln!(f, "{line}").map_err(|e| e.to_string())?;
        self.map.write().unwrap_or_else(|e| e.into_inner()).insert(r.key, r);
        Ok(())
    }

    /// Note that `path` (at this size and date) cannot be read: not retried by a later run.
    pub fn record_failure(&self, path: &str, size: u64, mtime: i64, why: &str) -> Result<(), String> {
        self.record_line(PreviewRec { key: hash(path), size, mtime, start: 0.0, len: 0.0, duration: 0.0, bytes: 0, kind: None, failed: Some(why.to_string()) })
    }

    /// Delete every preview and the index (the peaks are kept). Returns the bytes freed.
    pub fn clear(&self) -> u64 {
        let mut freed = 0;
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                let ours = (n.ends_with(".m4a") || n.ends_with(".m4a.tmp")) && n.len() > 17 && n[..16].chars().all(|c| c.is_ascii_hexdigit());
                if ours || n == "previews-v1.jsonl" {
                    freed += e.metadata().map(|m| m.len()).unwrap_or(0);
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        self.map.write().unwrap_or_else(|e| e.into_inner()).clear();
        freed
    }

    /// Encode `mono` (samples at [`PREVIEW_RATE`]) as the preview of `path`: written to a
    /// temporary file, renamed, then recorded.
    fn write(&self, path: &str, size: u64, mtime: i64, mono: &[f32], (start, duration): (f64, f64), kind: PreviewKind) -> Result<PreviewRec, String> {
        let mut enc = filmcraft_aac::Encoder::new(filmcraft_aac::EncoderConfig::cbr(PREVIEW_RATE, 1, PREVIEW_BPS)).map_err(|e| e.to_string())?;
        let mut aus = enc.encode(&[mono]);
        aus.extend(enc.flush());
        let mut track = filmcraft_isobmff::TrackConfig::new(filmcraft_isobmff::SampleEntry::aac(enc.audio_specific_config(), 1, PREVIEW_RATE), PREVIEW_RATE);
        track.media_start = Some(enc.priming_samples() as i64);
        let rec = PreviewRec {
            key: hash(path),
            size,
            mtime,
            start,
            len: mono.len() as f64 / PREVIEW_RATE as f64,
            duration,
            bytes: 0,
            kind: Some(kind),
            failed: None,
        };
        let file = self.file_of(&rec);
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let tmp = file.with_extension("m4a.tmp");
        let w = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        let mut mux = filmcraft_isobmff::Mp4Writer::new(std::io::BufWriter::new(w), filmcraft_isobmff::WriterOptions::new(filmcraft_isobmff::Brand::Mp4))
            .map_err(|e| e.to_string())?;
        let t = mux.add_track(track).map_err(|e| e.to_string())?;
        for au in &aus {
            mux.write_sample(t, filmcraft_isobmff::WriteSample { data: au, duration: filmcraft_aac::FRAME_LEN as u32, composition_offset: 0, is_sync: true })
                .map_err(|e| e.to_string())?;
        }
        let mut w = mux.finish().map_err(|e| e.to_string())?;
        w.flush().map_err(|e| e.to_string())?;
        drop(w);
        std::fs::rename(&tmp, &file).map_err(|e| e.to_string())?;
        let bytes = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
        let rec = PreviewRec { bytes, ..rec };
        self.record_line(rec.clone())?;
        Ok(rec)
    }
}

/// What one reading of a sound produced.
#[derive(Clone, Debug)]
pub struct Analysis {
    pub peaks: Peaks,
    pub size: u64,
    pub mtime: i64,
    pub preview: Option<PreviewRec>,
}

/// Read `path` **once** through the media layer: its waveform thumbnail, and its preview in
/// `previews` when given. The thumbnail is computed from the same samples as the preview.
pub fn analyse(media: &MediaPool, services: &dyn Services, path: &str, previews: Option<(&PreviewStore, PreviewKind)>) -> Result<Analysis, String> {
    let (size, mtime) = stamp(path).unwrap_or((0, 0));
    let src = media.open_file(path, services).map_err(|e| e.to_string())?;
    let info = src.info().clone();
    let stream = info.audio_streams.first().ok_or_else(|| format!("{path}: no sound"))?;
    // without a preview, a reduced rate is enough for 1 000 columns
    let sr = if previews.is_some() { PREVIEW_RATE } else { stream.sample_rate.clamp(8_000, 22_050) };
    let duration = info.duration.seconds();
    let total = (duration * sr as f64).round().max(1.0) as u64;
    let (w0, wn) = match previews {
        Some((_, kind)) => {
            let (s, l) = preview_window(duration, kind);
            let a = (s * sr as f64).round() as u64;
            (a, ((l * sr as f64).round() as u64).min(total.saturating_sub(a)))
        }
        None => (0, 0),
    };
    let mut mono: Vec<f32> = Vec::with_capacity(wn as usize);
    let mut cols = vec![(0f32, 0f32); COLUMNS];
    let step = sr as usize * 4;
    let mut at = 0u64;
    while at < total {
        let n = step.min((total - at) as usize);
        let buf = src.audio(at as i64, n, sr).map_err(|e| e.to_string())?;
        accumulate(&mut cols, &buf.channels, at, total);
        if previews.is_some() {
            let frames = buf.channels.iter().map(Vec::len).min().unwrap_or(0);
            let k = buf.channels.len().max(1) as f32;
            for i in 0..frames {
                let f = at + i as u64;
                if f >= w0 && f < w0 + wn {
                    mono.push(buf.channels.iter().map(|c| c[i]).sum::<f32>() / k);
                }
            }
        }
        at += n as u64;
    }
    let peaks = quantise(&cols, duration as f32);
    let preview = match previews {
        Some((store, kind)) if !mono.is_empty() => {
            if kind == PreviewKind::Music {
                // the excerpt is cut inside the music: short fades, no click
                let f = ((CUT_FADE * sr as f64) as usize).min(mono.len() / 2);
                let n = mono.len();
                for i in 0..f {
                    let g = i as f32 / f as f32;
                    mono[i] *= g;
                    mono[n - 1 - i] *= g;
                }
            }
            Some(store.write(path, size, mtime, &mono, (w0 as f64 / sr as f64, duration), kind)?)
        }
        _ => None,
    };
    Ok(Analysis { peaks, size, mtime, preview })
}

/// Options of a batch run (`filmcraft-cli assets.buildPreviews`).
#[derive(Clone, Debug)]
pub struct BuildOptions {
    pub kind: PreviewKind,
    /// The library folder to walk.
    pub root: PathBuf,
    /// The cache folder (peaks pack + previews).
    pub cache: PathBuf,
    /// At most this many sounds read in this run (0 = no limit).
    pub limit: usize,
    /// These files instead of walking `root` (a sample, a list prepared elsewhere).
    pub files: Option<Vec<String>>,
}

/// Progress of a batch run, after each sound.
#[derive(Clone, Debug, Default, Serialize)]
pub struct BuildProgress {
    /// Sounds in the library.
    pub total: usize,
    /// Already done before this run (skipped without reading).
    pub skipped: usize,
    pub made: usize,
    pub failed: usize,
    /// Bytes of the previews made in this run.
    pub bytes: u64,
    /// Seconds spent reading and encoding in this run.
    pub seconds: f64,
    pub last: Option<String>,
    pub last_ms: u64,
}

/// The sound files under `root` (sorted; hidden folders and files, `_` folders and macOS `._`
/// files skipped).
pub fn list_sounds(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries.into_iter().rev() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with('.') {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if !n.starts_with('_') && n != "__MACOSX" {
                    stack.push(e.path());
                }
            } else if let Some(ext) = n.rsplit_once('.').map(|(_, x)| x.to_ascii_lowercase())
                && filmcraft_media::AUDIO_EXTENSIONS.contains(&ext.as_str())
            {
                out.push(e.path().to_string_lossy().into_owned());
            }
        }
    }
    out.sort();
    out
}

/// Make the missing previews (and peaks) of a library, one file at a time, resumable: what is
/// already made for the file as it is now is skipped without reading it. `progress` is called
/// after each sound; returning `false` stops the run cleanly.
pub fn build(media: &MediaPool, services: &dyn Services, o: &BuildOptions, mut progress: impl FnMut(&BuildProgress) -> bool) -> BuildProgress {
    let store = PreviewStore::open(&o.cache.join("previews"));
    let pack = o.cache.join("peaks").join("peaks-v1.bin");
    let mut known = HashMap::new();
    read_pack(&pack, &mut known);
    let sounds = o.files.clone().unwrap_or_else(|| list_sounds(&o.root));
    let mut p = BuildProgress { total: sounds.len(), ..Default::default() };
    let t0 = web_time::Instant::now();
    for path in sounds {
        if o.limit > 0 && p.made + p.failed >= o.limit {
            break;
        }
        let Some((size, mtime)) = stamp(&path) else { continue };
        if store.record(&path, size, mtime).is_some() {
            p.skipped += 1;
            continue;
        }
        let t = web_time::Instant::now();
        match analyse(media, services, &path, Some((&store, o.kind))) {
            Ok(a) => {
                let _ = append_pack(&pack, hash(&path), a.size, a.mtime, &a.peaks);
                p.made += 1;
                p.bytes += a.preview.map(|r| r.bytes).unwrap_or(0);
            }
            Err(e) => {
                let _ = store.record_failure(&path, size, mtime, &e);
                p.failed += 1;
            }
        }
        p.last_ms = t.elapsed().as_millis() as u64;
        p.seconds = t0.elapsed().as_secs_f64();
        p.last = Some(path);
        if !progress(&p) {
            break;
        }
    }
    p.seconds = t0.elapsed().as_secs_f64();
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fc-sndprev-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A 16-bit PCM stereo WAV: a 440 Hz tone, silent for its first `silent` seconds.
    fn wav(path: &Path, seconds: f64, silent: f64) {
        let sr = 44_100u32;
        let n = (seconds * sr as f64) as u32;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + n * 4).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&sr.to_le_bytes());
        b.extend_from_slice(&(sr * 4).to_le_bytes());
        b.extend_from_slice(&4u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(n * 4).to_le_bytes());
        for i in 0..n {
            let t = i as f64 / sr as f64;
            let v = if t < silent { 0.0 } else { (2.0 * std::f64::consts::PI * 440.0 * t).sin() * 0.5 };
            let s = (v * 32767.0) as i16;
            b.extend_from_slice(&s.to_le_bytes());
            b.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(path, b).unwrap();
    }

    #[test]
    fn music_previews_start_at_thirty_percent_and_last_thirty_seconds() {
        assert_eq!(preview_window(100.0, PreviewKind::Music), (30.0, 30.0));
        assert_eq!(preview_window(40.0, PreviewKind::Music), (10.0, 30.0));
        assert_eq!(preview_window(20.0, PreviewKind::Music), (0.0, 20.0));
        assert_eq!(preview_window(3.5, PreviewKind::Sfx), (0.0, 3.5));
    }

    #[test]
    fn one_reading_makes_the_peaks_and_a_preview_the_media_layer_plays() {
        let d = tmp("one");
        let src = d.join("tone.wav");
        wav(&src, 2.0, 1.0);
        let s = crate::Session::default();
        let store = PreviewStore::open(&d.join("previews"));
        let a = analyse(&s.media, &*s.services, &src.to_string_lossy(), Some((&store, PreviewKind::Sfx))).unwrap();
        // the thumbnail: silent first half, loud second half
        assert_eq!(a.peaks.cols.len(), COLUMNS);
        assert_eq!(a.peaks.cols[100], (0, 0));
        assert!(a.peaks.cols[900].1 > 55, "{:?}", a.peaks.cols[900]);
        let rec = a.preview.expect("a preview");
        assert!((rec.len - 2.0).abs() < 0.01 && rec.start == 0.0 && rec.kind == Some(PreviewKind::Sfx));
        // ~96 kbps mono: about 24 KB for 2 s (plus the container)
        assert!(rec.bytes > 15_000 && rec.bytes < 40_000, "{} bytes", rec.bytes);
        let (file, _) = store.get(&src.to_string_lossy()).expect("found by path, size and date");
        // the preview is read by FilmCraft's own media layer, at the right length and level
        let p = s.media.open_file(&file.to_string_lossy(), &*s.services).unwrap();
        assert!((p.info().duration.seconds() - 2.0).abs() < 0.05, "{}", p.info().duration.seconds());
        let b = p.audio(0, 48_000 * 2, 48_000).unwrap();
        let ch = &b.channels[0];
        let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt();
        assert!(rms(&ch[2_000..40_000]) < 0.01, "silent part");
        assert!((rms(&ch[56_000..90_000]) - 0.354).abs() < 0.05, "tone part {}", rms(&ch[56_000..90_000]));
        // the index survives a restart; a changed file is not served its old preview
        let again = PreviewStore::open(&d.join("previews"));
        assert_eq!(again.stats(PreviewKind::Sfx).0, 1);
        wav(&src, 3.0, 0.0);
        assert!(again.get(&src.to_string_lossy()).is_none());
        // clearing removes the previews and the index, nothing else
        std::fs::write(d.join("previews").join("keep.txt"), b"x").unwrap();
        assert!(again.clear() > 15_000);
        assert_eq!(again.stats(PreviewKind::Sfx), (0, 0));
        assert!(d.join("previews").join("keep.txt").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_batch_run_resumes_skips_failures_and_respects_its_limit() {
        let d = tmp("batch");
        let lib = d.join("lib");
        std::fs::create_dir_all(lib.join("A")).unwrap();
        std::fs::create_dir_all(lib.join("_skip")).unwrap();
        wav(&lib.join("A/a.wav"), 0.5, 0.0);
        wav(&lib.join("A/b.wav"), 0.5, 0.0);
        wav(&lib.join("_skip/c.wav"), 0.5, 0.0);
        std::fs::write(lib.join("A/._a.wav"), b"Mac OS X junk").unwrap();
        std::fs::write(lib.join("A/broken.wav"), b"not a wav").unwrap();
        let s = crate::Session::default();
        let o = BuildOptions { kind: PreviewKind::Sfx, root: lib.clone(), cache: d.join("cache"), limit: 1, files: None };
        let p = build(&s.media, &*s.services, &o, |_| true);
        assert_eq!((p.total, p.made + p.failed), (3, 1), "{p:?}");
        // the next run goes on where the first stopped; the broken file fails once
        let o = BuildOptions { limit: 0, ..o };
        let p = build(&s.media, &*s.services, &o, |_| true);
        assert_eq!((p.skipped, p.made, p.failed), (1, 1, 1), "{p:?}");
        let p = build(&s.media, &*s.services, &o, |_| true);
        assert_eq!((p.skipped, p.made, p.failed), (3, 0, 0), "nothing read again: {p:?}");
        // the peaks were written in the same pass
        let mut m = HashMap::new();
        read_pack(&d.join("cache/peaks/peaks-v1.bin"), &mut m);
        assert_eq!(m.len(), 2);
        // a stop request ends the run after the current file
        let o2 = BuildOptions { cache: d.join("cache2"), ..o };
        let p = build(&s.media, &*s.services, &o2, |_| false);
        assert_eq!(p.made + p.failed, 1);
        let _ = std::fs::remove_dir_all(&d);
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
        let d = tmp("pack");
        let pack = d.join("peaks-v1.bin");
        append_pack(&pack, 1, 10, 20, &Peaks { duration: 1.0, cols: vec![(0, 1)] }).unwrap();
        append_pack(&pack, 2, 10, 20, &Peaks { duration: 2.0, cols: vec![(0, 2)] }).unwrap();
        append_pack(&pack, 1, 11, 21, &Peaks { duration: 3.0, cols: vec![(0, 3)] }).unwrap();
        let mut m = HashMap::new();
        read_pack(&pack, &mut m);
        assert_eq!(m.len(), 2);
        assert_eq!(m[&1].peaks.duration, 3.0);
        assert_eq!((m[&1].size, m[&1].mtime), (11, 21));
        let _ = std::fs::remove_dir_all(&d);
    }
}
