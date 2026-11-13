//! Video tools built on optional neural models (build feature `ml`). Each one reads a clip's media,
//! processes it frame by frame and writes a **new media file** (ProRes 422 LT by default) that is
//! imported into the project: the original is never touched.
//!
//! | command | what it does | model |
//! |---|---|---|
//! | `clip.interpolate` | RIFE v4 frame interpolation: `fps` converts the frame rate (24 to 60 fps), `slow` stretches time (4 = four times slower at the same rate); the midpoint network is applied on a 1/8 grid | `rife-v4` |
//! | `clip.upscale` | SPAN 2x super-resolution of every frame (clips generated at 1K, old rushes) | `span-2x` |
//!
//! Parameters shared by the video tools: `clip` (timeline clip: its used range is processed) or `item`
//! (a media item: all of it); `out` (output file; default `<project folder>/ML/<name> <tag>.mov`);
//! `preset` (a transcode preset of `media.createProxies`, default `prores_lt`); `wait` (run to the end
//! before returning: the extensions pass `${blocking}`; otherwise a background job, see `job.list`).

use serde_json::Value;

use crate::commands::{CommandSpec, bad};
#[allow(unused_imports)]
use crate::{EngineError, Result, Session};

#[cfg(not(feature = "ml"))]
fn no_ml(cmd: &str) -> EngineError {
    bad(cmd, "this build has no neural tools (build with --features ml)")
}

#[cfg(feature = "ml")]
pub(crate) mod on {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};

    use filmcraft_export::{Progress, Report};
    use filmcraft_frame::{AudioBuffer, VideoFrame};
    use filmcraft_media::{FrameRequest, MediaError, MediaInfo, MediaSource, SharedSource};
    use filmcraft_ml::Image;
    use filmcraft_ml::catalogue::{self, Model};
    use filmcraft_project::{ItemId, MediaClip};
    use filmcraft_time::{FrameRate, Tick, TimeRange};
    use serde_json::{Value, json};

    use super::*;
    use crate::commands::{bool_p, clip_p, f64_p, str_p, u64_p};
    use crate::ml_tools::{models_dir, require};
    use crate::proxies::{OnDone, PendingJob, preset, transcode, unique_path};

    type MResult<T> = std::result::Result<T, MediaError>;

    /// A source made of another one: frames come from `make` (given the virtual time), audio is the
    /// inner one shifted by `base` (or silence).
    pub struct Virtual {
        inner: SharedSource,
        info: MediaInfo,
        base: Tick,
        keep_audio: bool,
        make: Box<dyn Fn(&SharedSource, Tick) -> MResult<VideoFrame> + Send + Sync>,
    }

    impl MediaSource for Virtual {
        fn info(&self) -> &MediaInfo {
            &self.info
        }
        fn video_frame(&self, req: FrameRequest) -> MResult<Arc<VideoFrame>> {
            let t = req.time.max(Tick::ZERO);
            (self.make)(&self.inner, t).map(|f| Arc::new(f.with_pts(req.time)))
        }
        fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> MResult<AudioBuffer> {
            if self.keep_audio {
                let off = self.base.to_units_floor(sample_rate as i64);
                self.inner.audio(start + off, frames, sample_rate)
            } else {
                self.inner.audio(i64::MAX / 4, frames, sample_rate)
            }
        }
    }

    /// A thread of its own that makes the frames, one at a time and with its own state.
    ///
    /// The export pipeline asks for frames from several rayon workers at once, and a rayon worker that waits
    /// on a lock can pick up another frame request while it waits (work stealing) and wait for itself: a
    /// network that needs the frames in order (RVM keeps a recurrent state), or one that must not run twice at
    /// once on the GPU (candle's Metal backend gave corrupted SPAN tiles from two threads, measured), would
    /// hang the export. So a frame request only sends its time to this thread and waits on a channel, which
    /// does not steal work, and the export itself runs in a pool of two threads (see [`run_transcode`]) so
    /// the global pool stays free for the decoders and the tiles this thread uses.
    pub struct Worker {
        tx: Mutex<std::sync::mpsc::Sender<(Tick, std::sync::mpsc::Sender<MResult<VideoFrame>>)>>,
    }

    impl Worker {
        pub fn spawn(mut f: impl FnMut(Tick) -> MResult<VideoFrame> + Send + 'static) -> Self {
            let (tx, rx) = std::sync::mpsc::channel::<(Tick, std::sync::mpsc::Sender<MResult<VideoFrame>>)>();
            let _ = std::thread::Builder::new().name("filmcraft-ml-frames".into()).spawn(move || {
                while let Ok((t, reply)) = rx.recv() {
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(t)))
                        .unwrap_or_else(|_| Err(MediaError::Decode("internal error in the frame worker".into())));
                    let _ = reply.send(r);
                }
            });
            Self { tx: Mutex::new(tx) }
        }

        pub fn call(&self, t: Tick) -> MResult<VideoFrame> {
            let (rtx, rrx) = std::sync::mpsc::channel();
            self.tx.lock().unwrap_or_else(|e| e.into_inner()).send((t, rtx)).map_err(|_| MediaError::Decode("the frame worker stopped".into()))?;
            rrx.recv().map_err(|_| MediaError::Decode("the frame worker stopped".into()))?
        }
    }

    /// A virtual source whose frames come from `worker`.
    pub fn from_worker(inner: SharedSource, info: MediaInfo, base: Tick, keep_audio: bool, worker: Worker) -> SharedSource {
        Arc::new(Virtual { inner, info, base, keep_audio, make: Box::new(move |_, t| worker.call(t)) })
    }

    /// `transcode`, with the frame requests of the export confined to two threads.
    pub fn run_transcode(
        clip: &MediaClip,
        name: &str,
        src: SharedSource,
        range: TimeRange,
        out: &Path,
        preset: &crate::proxies::Preset,
        prog: &Arc<Progress>,
    ) -> std::result::Result<Report, String> {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build().map_err(|e| e.to_string())?;
        pool.install(|| transcode(clip, name, src, Some(range), &out.to_string_lossy(), preset, prog))
    }

    pub fn to_image(f: &VideoFrame) -> Image {
        let px = f.to_rgba8();
        let mut data = Vec::with_capacity(px.len() / 4 * 3);
        for p in px.as_chunks::<4>().0 {
            data.extend([p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0]);
        }
        Image { width: f.width as usize, height: f.height as usize, data }
    }

    pub fn to_frame(img: &Image) -> VideoFrame {
        let mut px = Vec::with_capacity(img.width * img.height * 4);
        for p in img.data.as_chunks::<3>().0 {
            px.extend([
                (p[0].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
                (p[1].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
                (p[2].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
                255,
            ]);
        }
        VideoFrame::rgba8(img.width as u32, img.height as u32, px)
    }

    /// What a video tool works on.
    pub struct Target {
        pub item: ItemId,
        pub clip: MediaClip,
        pub name: String,
        pub src: SharedSource,
        /// The part of the media to process (media time).
        pub range: TimeRange,
        pub rate: FrameRate,
    }

    pub fn target(s: &Session, p: &Value, cmd: &str) -> Result<Target> {
        let (item, range) = if let Some(c) = clip_p(p, "clip").or_else(|| s.state.selection.first().copied().filter(|_| p.get("item").is_none())) {
            let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
            let (_, it) = q.find_item(c).ok_or_else(|| bad(cmd, "no such clip"))?;
            if (it.speed - 1.0).abs() > 1e-9 || it.reverse {
                return Err(bad(cmd, "the clip is retimed (speed or reverse): process the media item instead (`item`)"));
            }
            (it.item, Some(TimeRange { start: it.source_in, duration: it.duration }))
        } else if let Some(i) = u64_p(p, "item") {
            (ItemId(i), None)
        } else {
            return Err(bad(cmd, "select a clip, or pass `clip` or `item`"));
        };
        let (mid, m, sub) = s.project.resolve_media(item).ok_or_else(|| bad(cmd, "this item is not file media"))?;
        let name = s.project.item(item).map(|i| i.name.clone()).unwrap_or_else(|| "clip".into());
        let v = m.info.video.as_ref().ok_or_else(|| bad(cmd, "this media has no video"))?;
        if s.media.offline_status(mid).is_some() {
            return Err(bad(cmd, format!("{name} is offline; link it first")));
        }
        let src = s.media.full_res_source(&s.project, mid, &*s.services).ok_or_else(|| bad(cmd, format!("{name}: no source")))?;
        let range = range.or(sub).unwrap_or(TimeRange { start: Tick::ZERO, duration: m.info.duration });
        Ok(Target { item: mid, clip: m.clone(), name, src, range, rate: v.frame_rate })
    }

    pub fn require_model(m: &Model, cmd: &str) -> Result<PathBuf> {
        require(m, cmd)
    }

    /// `<dir>/<stem><suffix>.<ext>`, numbered if it exists.
    pub fn unique_dir_file(dir: &Path, stem: &str, suffix: &str, ext: &str) -> PathBuf {
        unique_path(dir, stem, suffix, ext, &[])
    }

    pub fn out_path(s: &Session, p: &Value, name: &str, tag: &str, ext: &str) -> PathBuf {
        if let Some(o) = str_p(p, "out") {
            return PathBuf::from(o);
        }
        let dir = s
            .path
            .as_deref()
            .and_then(|f| Path::new(f).parent())
            .map(|d| d.join("ML"))
            .or_else(|| models_dir().and_then(|d| d.parent().map(|x| x.join("out"))))
            .unwrap_or_else(std::env::temp_dir);
        let stem: String = name
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { ' ' })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        unique_path(&dir, &stem, &format!(" {tag}"), ext, &[])
    }

    /// Run `work` (frames or samples in, files out) as a job; the outputs are imported when it finishes.
    pub fn spawn(
        s: &mut Session,
        label: String,
        total: u64,
        item: ItemId,
        outs: Vec<PathBuf>,
        wait: bool,
        work: impl FnOnce(&Arc<Progress>, &[PathBuf]) -> std::result::Result<Report, String> + Send + 'static,
    ) -> Result<Value> {
        let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
        let job = crate::Job { id, label, progress: Default::default(), result: Default::default() };
        job.progress.total.store(total, Ordering::Relaxed);
        let outputs: Arc<Mutex<Vec<(ItemId, String)>>> = Arc::default();
        let (prog, res, outs_done) = (job.progress.clone(), job.result.clone(), outputs.clone());
        let out_s: Vec<String> = outs.iter().map(|o| o.to_string_lossy().into_owned()).collect();
        let run = move || {
            for o in &outs {
                if let Some(d) = o.parent() {
                    let _ = std::fs::create_dir_all(d);
                }
            }
            let t0 = web_time::Instant::now();
            let r = work(&prog, &outs);
            let secs = t0.elapsed().as_secs_f64();
            let r = match r {
                Ok(mut rep) => {
                    rep.seconds = secs;
                    rep.path = outs.first().map(|o| o.to_string_lossy().into_owned()).unwrap_or_default();
                    let mut g = outs_done.lock().unwrap_or_else(|e| e.into_inner());
                    for o in &outs {
                        g.push((item, o.to_string_lossy().into_owned()));
                    }
                    Ok(rep)
                }
                Err(e) => {
                    for o in &outs {
                        let _ = std::fs::remove_file(o);
                    }
                    *prog.error.lock().unwrap_or_else(|x| x.into_inner()) = Some(e.clone());
                    Err(e)
                }
            };
            *prog.status.lock().unwrap_or_else(|e| e.into_inner()) = match &r {
                Ok(_) => format!("Done in {secs:.1}s"),
                Err(e) => e.clone(),
            };
            prog.finished.store(true, Ordering::Relaxed);
            *res.lock().unwrap_or_else(|x| x.into_inner()) = Some(r);
        };
        let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
        s.jobs.push(job);
        if wait {
            run();
            let result = s.jobs.iter().find(|j| j.id == id).map(|j| j.to_json()).unwrap_or(Value::Null);
            if let Some(e) = result["result"]["error"].as_str() {
                return Err(EngineError::Other(e.to_string()));
            }
            let mut items = Vec::new();
            for p in &out_s {
                items.push(crate::commands::import_streamed(s, p, None)?.0);
            }
            return Ok(json!({"job": id, "path": out_s.first(), "paths": out_s, "item": items.first(), "items": items, "result": result["result"]}));
        }
        s.media_jobs.push(PendingJob { job: id, on_done: OnDone::Import, outputs });
        std::thread::Builder::new().name("filmcraft-ml".into()).spawn(run).map_err(|e| EngineError::Other(e.to_string()))?;
        Ok(json!({"job": id, "path": out_s.first(), "paths": out_s}))
    }

    pub fn preset_p(p: &Value, cmd: &str) -> Result<&'static crate::proxies::Preset> {
        let id = str_p(p, "preset").unwrap_or("prores_lt");
        preset(id).filter(|x| x.scale == 1.0).ok_or_else(|| bad(cmd, format!("unknown preset `{id}` (e.g. prores_lt, prores_hq, h264)")))
    }

    // -------------------------------------------------------------------------------- RIFE

    pub fn interpolate(s: &mut Session, p: &Value) -> Result<Value> {
        const CMD: &str = "clip.interpolate";
        let t = target(s, p, CMD)?;
        let dir = require_model(&catalogue::RIFE, CMD)?;
        let slow = f64_p(p, "slow").unwrap_or(1.0);
        if !(1.0..=16.0).contains(&slow) {
            return Err(bad(CMD, "`slow` must be between 1 and 16"));
        }
        let src_fps = t.rate.as_f64();
        let out_rate = match f64_p(p, "fps") {
            Some(f) if (1.0..=240.0).contains(&f) => FrameRate::from_f64(f),
            Some(_) => return Err(bad(CMD, "`fps` must be between 1 and 240")),
            None => t.rate,
        };
        if slow == 1.0 && out_rate == t.rate {
            return Err(bad(CMD, "nothing to do: give `fps` (another frame rate) and/or `slow` (> 1)"));
        }
        let depth = u64_p(p, "depth").unwrap_or(3).clamp(1, 4) as u32;
        let mut rife = filmcraft_ml::rife::Rife::load(&dir).map_err(|e| bad(CMD, e.to_string()))?;
        let (vw, vh) = t.clip.info.video.as_ref().map_or((0, 0), |v| (v.width, v.height));
        // flow estimated on half the pixels per side above 720p (about 3x faster), full precision up to 720p
        rife.scale = f64_p(p, "scale").unwrap_or(if (vw as u64) * (vh as u64) > 640 * 360 { 0.5 } else { 1.0 }).clamp(0.25, 1.0);
        let rife = Arc::new(rife);
        let preset = preset_p(p, CMD)?;
        let out_dur = Tick::from_seconds_f64(t.range.duration.seconds() * slow);
        let out_frames = (out_dur.seconds() * out_rate.as_f64()).round().max(1.0) as u64;
        let mut clip = t.clip.clone();
        clip.offline = false;
        clip.proxy = None;
        clip.info.duration = out_dur;
        let keep_audio = slow == 1.0 && clip.info.has_audio();
        if !keep_audio {
            clip.info.audio_streams.clear();
        }
        if let Some(v) = clip.info.video.as_mut() {
            v.frame_rate = out_rate;
        }
        let (base, rate, last) = (t.range.start, t.rate, (t.range.duration.seconds() * src_fps).round() as i64 - 1);
        let (inner, rife2) = (t.src.clone(), rife.clone());
        let mut frames: Vec<(i64, Arc<Image>)> = Vec::new();
        let mut interval: Option<(i64, filmcraft_ml::rife::Interval)> = None;
        let worker = Worker::spawn(move |vt: Tick| -> MResult<VideoFrame> {
            let pos = vt.seconds() / slow * src_fps;
            let i = (pos + 1e-6).floor() as i64;
            let phase = (pos - i as f64).max(0.0);
            let mut load = |k: i64| -> MResult<Arc<Image>> {
                if let Some((_, f)) = frames.iter().find(|(x, _)| *x == k) {
                    return Ok(f.clone());
                }
                let f = Arc::new(to_image(&*inner.video_frame(FrameRequest::full(base + rate.tick_of(k.clamp(0, last.max(0)))))?));
                frames.push((k, f.clone()));
                if frames.len() > 4 {
                    frames.remove(0);
                }
                Ok(f)
            };
            let a = load(i)?;
            if phase < 1.0 / 16.0 || i >= last {
                return Ok(to_frame(&a));
            }
            let b = load(i + 1)?;
            if interval.as_ref().is_none_or(|(k, _)| *k != i) {
                interval = Some((i, rife2.interval(&a, &b).map_err(|e| MediaError::Decode(format!("RIFE: {e}")))?));
            }
            let iv = &mut interval.as_mut().ok_or_else(|| MediaError::Decode("RIFE: no interval".into()))?.1;
            let y = rife2.at_in(iv, phase, depth).map_err(|e| MediaError::Decode(format!("RIFE: {e}")))?;
            Ok(to_frame(&y))
        });
        let virt = from_worker(t.src.clone(), clip.info.clone(), base, keep_audio, worker);
        let name = format!("{} (RIFE)", t.name);
        let out = out_path(s, p, &t.name, if slow != 1.0 { "slow" } else { "interp" }, "mov");
        let wait = bool_p(p, "wait").unwrap_or(false);
        let label = format!("Interpolate {}", t.name);
        let range = TimeRange { start: Tick::ZERO, duration: out_dur };
        let (out_s, devname) = (out.to_string_lossy().into_owned(), format!("{:?}", rife.device()));
        let mut res = spawn(s, label, out_frames, t.item, vec![out], wait, move |prog, o| run_transcode(&clip, &name, virt, range, &o[0], preset, prog))?;
        res["frames"] = json!(out_frames);
        res["device"] = json!(devname);
        res["summary"] = json!(format!(
            "{} images à {} i/s générées par RIFE{} → {}",
            out_frames,
            out_rate.label(),
            if slow != 1.0 { format!(" (ralenti ×{slow})") } else { String::new() },
            Path::new(&out_s).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        ));
        Ok(res)
    }

    // -------------------------------------------------------------------------------- SPAN

    pub fn upscale(s: &mut Session, p: &Value) -> Result<Value> {
        const CMD: &str = "clip.upscale";
        let t = target(s, p, CMD)?;
        let dir = require_model(&catalogue::SPAN2X, CMD)?;
        let mut up = filmcraft_mlrestore::Upscaler::load(&dir.join(filmcraft_mlrestore::files::SPAN2X.file.name)).map_err(|e| bad(CMD, e.to_string()))?;
        // one tile per frame up to 1024 px: fewer, larger passes (measured 4.6 s instead of 9 s for a 960x540 frame)
        up.tiling.tile = 1024;
        let up = Arc::new(up);
        let keep_tone = bool_p(p, "keepTone").unwrap_or(true);
        let scale = up.scale() as u32;
        let (w, h) = t.clip.info.video.as_ref().map(|v| (v.width, v.height)).unwrap_or((0, 0));
        if w * scale > 7680 || h * scale > 4320 {
            return Err(bad(CMD, format!("the result would be {}x{} px: more than 8K", w * scale, h * scale)));
        }
        let preset = preset_p(p, CMD)?;
        let frames = (t.range.duration.seconds() * t.rate.as_f64()).round().max(1.0) as u64;
        let mut clip = t.clip.clone();
        clip.offline = false;
        clip.proxy = None;
        clip.info.duration = t.range.duration;
        if let Some(v) = clip.info.video.as_mut() {
            v.width *= scale;
            v.height *= scale;
        }
        let (base, up2, inner) = (t.range.start, up.clone(), t.src.clone());
        let worker = Worker::spawn(move |vt: Tick| -> MResult<VideoFrame> {
            let f = inner.video_frame(FrameRequest::full(base + vt))?;
            let src = to_image(&f);
            let mut y = up2.upscale(&src, &mut |_, _| true).map_err(|e| MediaError::Decode(format!("SPAN: {e}")))?;
            if keep_tone {
                // the network darkens the picture a little (about 6 of 255 on this footage): put each channel's
                // mean back to the input's
                let mean = |d: &[f32], c: usize| d.iter().skip(c).step_by(3).map(|v| *v as f64).sum::<f64>() / (d.len() / 3) as f64;
                let shift: [f32; 3] = [0, 1, 2].map(|c| (mean(&src.data, c) - mean(&y.data, c)) as f32);
                for px in y.data.as_chunks_mut::<3>().0 {
                    for c in 0..3 {
                        px[c] = (px[c] + shift[c]).clamp(0.0, 1.0);
                    }
                }
            }
            Ok(to_frame(&Image { width: y.width, height: y.height, data: y.data }))
        });
        let keep_audio = clip.info.has_audio();
        let virt = from_worker(t.src.clone(), clip.info.clone(), base, keep_audio, worker);
        let name = format!("{} (x{scale})", t.name);
        let out = out_path(s, p, &t.name, &format!("x{scale}"), "mov");
        let wait = bool_p(p, "wait").unwrap_or(false);
        let range = TimeRange { start: Tick::ZERO, duration: t.range.duration };
        let out_s = out.to_string_lossy().into_owned();
        let mut res = spawn(s, format!("Upscale {}", t.name), frames, t.item, vec![out], wait, move |prog, o| {
            run_transcode(&clip, &name, virt, range, &o[0], preset, prog)
        })?;
        res["frames"] = json!(frames);
        res["summary"] = json!(format!(
            "{frames} images agrandies ×{scale} ({w}×{h} → {}×{}) → {}",
            w * scale,
            h * scale,
            Path::new(&out_s).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        ));
        Ok(res)
    }
    // -------------------------------------------------------------------------------- RVM

    struct MatteState {
        last: i64,
        st: filmcraft_ml::rvm::State,
        done: Vec<(i64, VideoFrame)>,
    }

    fn hex_rgb(v: &str) -> Option<[f32; 3]> {
        let v = v.trim().trim_start_matches('#');
        if v.len() != 6 {
            return None;
        }
        let n = u32::from_str_radix(v, 16).ok()?;
        Some([(n >> 16 & 255) as f32 / 255.0, (n >> 8 & 255) as f32 / 255.0, (n & 255) as f32 / 255.0])
    }

    pub fn matte(s: &mut Session, p: &Value) -> Result<Value> {
        const CMD: &str = "clip.matte";
        let t = target(s, p, CMD)?;
        let dir = require_model(&catalogue::RVM, CMD)?;
        let mode = str_p(p, "mode").unwrap_or("matte").to_string();
        if !matches!(mode.as_str(), "matte" | "cutout") {
            return Err(bad(CMD, "`mode` is `matte` (white person on black) or `cutout` (person on `background`)"));
        }
        let bg = hex_rgb(str_p(p, "background").unwrap_or("00B140")).ok_or_else(|| bad(CMD, "`background` is a colour such as 00B140"))?;
        let (w, h) = t.clip.info.video.as_ref().map_or((0, 0), |v| (v.width as usize, v.height as usize));
        let ratio = f64_p(p, "ratio").unwrap_or_else(|| filmcraft_ml::rvm::Rvm::ratio(w, h)).clamp(0.05, 1.0);
        let rvm = Arc::new(filmcraft_ml::rvm::Rvm::load(&dir).map_err(|e| bad(CMD, e.to_string()))?);
        let preset = preset_p(p, CMD)?;
        let frames = (t.range.duration.seconds() * t.rate.as_f64()).round().max(1.0) as u64;
        let mut clip = t.clip.clone();
        clip.offline = false;
        clip.proxy = None;
        clip.info.duration = t.range.duration;
        let (base, rate) = (t.range.start, t.rate);
        let (rvm2, mode2, inner) = (rvm.clone(), mode.clone(), t.src.clone());
        let mut st = MatteState { last: -1, st: Default::default(), done: Vec::new() };
        let worker = Worker::spawn(move |vt: Tick| -> MResult<VideoFrame> {
            let idx = (vt.seconds() * rate.as_f64()).round() as i64;
            if let Some((_, f)) = st.done.iter().find(|(k, _)| *k == idx) {
                return Ok(f.clone());
            }
            if idx <= st.last || (st.last < 0 && idx > 8) {
                // a seek: the recurrent state starts again here
                st.st = Default::default();
                st.last = idx - 1;
                st.done.clear();
            }
            while st.last < idx {
                let k = st.last + 1;
                let f = inner.video_frame(FrameRequest::full(base + rate.tick_of(k)))?;
                let img = to_image(&f);
                let (fgr, pha) = rvm2.frame(&img.data, img.height, img.width, ratio, &mut st.st).map_err(|e| MediaError::Decode(format!("RVM: {e}")))?;
                let mut px = Vec::with_capacity(img.width * img.height * 4);
                for (i, a) in pha.iter().enumerate() {
                    let rgb: [f32; 3] = if mode2 == "matte" { [*a, *a, *a] } else { [0, 1, 2].map(|c| a * fgr[i * 3 + c] + (1.0 - a) * bg[c]) };
                    px.extend(rgb.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8));
                    px.push(255);
                }
                st.done.push((k, VideoFrame::rgba8(img.width as u32, img.height as u32, px)));
                if st.done.len() > 6 {
                    st.done.remove(0);
                }
                st.last = k;
            }
            st.done.iter().find(|(k, _)| *k == idx).map(|(_, f)| f.clone()).ok_or_else(|| MediaError::Decode("RVM: frame not produced".into()))
        });
        let keep_audio = clip.info.has_audio();
        let virt = from_worker(t.src.clone(), clip.info.clone(), base, keep_audio, worker);
        let name = format!("{} ({})", t.name, if mode == "matte" { "matte" } else { "détouré" });
        let out = out_path(s, p, &t.name, if mode == "matte" { "matte" } else { "cutout" }, "mov");
        let wait = bool_p(p, "wait").unwrap_or(false);
        let range = TimeRange { start: Tick::ZERO, duration: t.range.duration };
        let out_s = out.to_string_lossy().into_owned();
        let devname = format!("{:?}", rvm.device());
        let mut res = spawn(s, format!("Matte {}", t.name), frames, t.item, vec![out], wait, move |prog, o| {
            run_transcode(&clip, &name, virt, range, &o[0], preset, prog)
        })?;
        res["frames"] = json!(frames);
        res["ratio"] = json!(ratio);
        res["device"] = json!(devname);
        res["summary"] = json!(format!(
            "{frames} images détourées (RVM, réseau à {:.0} % de la taille) → {}",
            ratio * 100.0,
            Path::new(&out_s).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        ));
        Ok(res)
    }
}

fn interpolate(s: &mut Session, p: &Value) -> Result<Value> {
    #[cfg(feature = "ml")]
    return on::interpolate(s, p);
    #[cfg(not(feature = "ml"))]
    {
        let _ = (s, p);
        Err(no_ml("clip.interpolate"))
    }
}

fn matte(s: &mut Session, p: &Value) -> Result<Value> {
    #[cfg(feature = "ml")]
    return on::matte(s, p);
    #[cfg(not(feature = "ml"))]
    {
        let _ = (s, p);
        Err(no_ml("clip.matte"))
    }
}

fn upscale(s: &mut Session, p: &Value) -> Result<Value> {
    #[cfg(feature = "ml")]
    return on::upscale(s, p);
    #[cfg(not(feature = "ml"))]
    {
        let _ = (s, p);
        Err(no_ml("clip.upscale"))
    }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "clip.interpolate",
            label: "Interpolate Frames (RIFE)…",
            menu: &[],
            shortcut: None,
            params: INTERPOLATE_PARAMS,
            enabled: crate::commands::always,
            run: interpolate,
            journal: true,
            named: crate::commands::NamedKeys::NONE,
        },
        CommandSpec {
            id: "clip.matte",
            label: "Matte the Person (RVM)…",
            menu: &[],
            shortcut: None,
            params: r#"{"clip":id? (timeline clip: its used range; default the selected one),"item":id? (a media item: all of it),"mode":"matte"|"cutout"="matte" (white person on black / person over `background`),"background":"00B140","ratio":0.05-1? (network size; default: long side 512),"out":path?,"preset":"prores_lt"|"prores_hq"|"h264"="prores_lt","wait":bool=false}"#,
            enabled: crate::commands::always,
            run: matte,
            journal: true,
            named: crate::commands::NamedKeys::NONE,
        },
        CommandSpec {
            id: "clip.upscale",
            label: "Upscale Video 2x…",
            menu: &[],
            shortcut: None,
            params: r#"{"clip":id? (timeline clip: its used range; default the selected one),"item":id? (a media item: all of it),"out":path?,"keepTone":bool=true (put the mean level of each channel back: the network darkens a little),"preset":"prores_lt"|"prores_hq"|"h264"="prores_lt","wait":bool=false}"#,
            enabled: crate::commands::always,
            run: upscale,
            journal: true,
            named: crate::commands::NamedKeys::NONE,
        },
    ]
}

const INTERPOLATE_PARAMS: &str = r#"{"clip":id? (timeline clip: its used range; default the selected one),"item":id? (a media item: all of it),"fps":n? (new frame rate, e.g. 60),"slow":n=1 (1-16: time stretch, 4 = four times slower at the same frame rate),"scale":0.25-1 (flow estimation scale; default 1 up to 720p, 0.5 above: faster),"depth":1-4=3 (the midpoint network is applied down to 1/2^depth of a frame),"out":path?,"preset":"prores_lt"|"prores_hq"|"h264"="prores_lt","wait":bool=false}"#;
