//! Auto-reframe: `sequence.autoReframe`.
//!
//! Takes the selected video clips (or `clips`), finds the main subject in each (people with YOLOX,
//! faces with YuNet when installed), follows it, and writes the crop as animated Motion `Position`
//! keyframes with a fixed `Scale`, so a 16:9 shot fills a 9:16, 1:1 or 4:5 frame with the subject
//! kept in it. One undo step. The models are optional downloads (`models.list`, `models.install`).
//!
//! **The crop.** For a picture `W × H` and a target ratio, the crop window is the largest
//! rectangle of that ratio inside the picture (divided by `zoom`). It is placed by the Motion
//! transform: `Scale` = output width / crop width (in %, Scale to Frame Size is turned off) and
//! `Position` moves the window's centre to the middle of the frame. The window never leaves the
//! picture. Keyframes sit on media time (source in + clip time × speed), as every Motion keyframe does.
//!
//! **The path.** The clip is sampled `fps` times a second (default 5); each sample picks the
//! subject (see `filmcraft_reframe::track`); the samples go through `filmcraft_reframe::path`: a
//! dead zone (`deadZone`, a fraction of the crop: the camera holds still while the subject only
//! fidgets), then a smoothing spline (`smooth` seconds: no jerks). A scene cut (the picture changes
//! between two samples: 24 x 14 luma thumbnails differ by more than 0.2 on average) splits the path
//! into pieces planned separately, and the frame cuts too (a Hold keyframe) instead of panning across the cut. Keyframes are Linear (Bézier
//! interpolation in this engine eases to a stop at every keyframe, which would stutter a pan): one per
//! frame at most, minus the ones the straight line between their neighbours reproduces within 0.15
//! source pixel, so a straight pan costs two keyframes and a curve a few per second.
//!
//! **Subtitles.** `safeBottom` reserves that fraction of the output height under the subject: when
//! the window is shorter than the picture (`zoom` > 1) the subject sits in the free part above it.
//! The result reports `captionOverlapPercent`: how often the subject's box reaches into the reserved band.
//!
//! The work runs on the calling thread: about 0.1 s per sampled frame with YOLOX-Nano.

use std::sync::{Arc, Mutex, OnceLock};

use filmcraft_media::FrameRequest;
use filmcraft_project::{ClipId, Interpolation, Keyframe, ParamValue, TrackKind};
use filmcraft_reframe::detect::{Yolox, Yunet};
use filmcraft_reframe::path::{self, Options as PathOptions, Sample};
use filmcraft_reframe::track::{self, Prefer, Subject};
use filmcraft_reframe::{Detection, Image};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, bool_p, f64_p, has_seq, str_p};
use crate::{EngineError, Result, Session};

type Cache<T> = Mutex<Option<(std::path::PathBuf, String, Arc<T>)>>;

fn yolox_for(root: &std::path::Path, id: &str, cmd: &str) -> Result<Arc<Yolox>> {
    static C: OnceLock<Cache<Yolox>> = OnceLock::new();
    let mut c = C.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some((r, i, m)) = c.as_ref()
        && r == root
        && i == id
    {
        return Ok(m.clone());
    }
    let info = filmcraft_modelstore::find(id)
        .filter(|_| id.starts_with("yolox"))
        .ok_or_else(|| bad(cmd, format!("unknown detector `{id}` (yolox-nano, yolox-tiny, yolox-s)")))?;
    let name = info.files.first().map(|f| f.name).unwrap_or_default();
    let bytes = filmcraft_modelstore::read(root, info, name).map_err(|_| bad(cmd, crate::voice_clean::consent_text(info)))?;
    let size = if id == "yolox-s" { 640 } else { 416 };
    let m = Arc::new(Yolox::from_bytes(&bytes).map_err(|e| bad(cmd, format!("model unreadable: {e}")))?.with_size(size));
    *c = Some((root.to_path_buf(), id.to_string(), m.clone()));
    Ok(m)
}

fn yunet_for(root: &std::path::Path) -> Option<Arc<Yunet>> {
    static C: OnceLock<Cache<Yunet>> = OnceLock::new();
    let mut c = C.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some((r, _, m)) = c.as_ref()
        && r == root
    {
        return Some(m.clone());
    }
    let info = filmcraft_modelstore::find("yunet")?;
    let name = info.files.first()?.name;
    let bytes = filmcraft_modelstore::read(root, info, name).ok()?;
    let m = Arc::new(Yunet::from_bytes(&bytes).ok()?);
    *c = Some((root.to_path_buf(), "yunet".into(), m.clone()));
    Some(m)
}

/// `"9:16"`, `"1:1"`, `"4:5"`, `"1080x1920"` or a number (width / height) → width / height.
pub(crate) fn ratio_p(p: &Value, cmd: &str) -> Result<(f64, f64)> {
    let v = p.get("ratio").cloned().unwrap_or(json!("9:16"));
    let parse = |a: &str, b: &str| -> Option<(f64, f64)> {
        let (a, b) = (a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?);
        (a > 0.0 && b > 0.0).then_some((a, b))
    };
    let r = match &v {
        Value::Number(n) => n.as_f64().filter(|x| *x > 0.0).map(|x| (x, 1.0)),
        Value::String(s) => s.split_once(':').or_else(|| s.split_once('x')).and_then(|(a, b)| parse(a, b)),
        _ => None,
    };
    let (w, h) = r.ok_or_else(|| bad(cmd, "`ratio` must look like 9:16, 1:1, 4:5 or 1080x1920"))?;
    let q = w / h;
    if !(0.2..=5.0).contains(&q) {
        return Err(bad(cmd, "`ratio` is out of range"));
    }
    Ok((w, h))
}

/// Output frame size for a ratio: 1080 on the short side (the sequence's own size when it already has that ratio).
pub(crate) fn output_size(ratio: (f64, f64), seq: (u32, u32)) -> (u32, u32) {
    let q = ratio.0 / ratio.1;
    if ((seq.0 as f64 / seq.1.max(1) as f64) / q - 1.0).abs() < 0.01 {
        return seq;
    }
    let even = |v: f64| ((v / 2.0).round() as u32 * 2).max(2);
    if q < 1.0 { (even(1080.0), even(1080.0 / q)) } else { (even(1080.0 * q), even(1080.0)) }
}

struct Job {
    clip: ClipId,
    item: filmcraft_project::ItemId,
    duration: Tick,
    source_in: Tick,
    speed: f64,
}

fn collect_jobs(s: &Session, p: &Value, cmd: &str) -> Result<Vec<Job>> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let ids: Vec<ClipId> = match p.get("clips").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|v| v.as_u64().map(ClipId)).collect(),
        None => s.state.selection.to_vec(),
    };
    let mut jobs = Vec::new();
    for c in ids {
        let Some((t, it)) = q.find_item(c) else { continue };
        if q.track(t).is_none_or(|t| t.kind != TrackKind::Video) {
            continue;
        }
        if it.reverse || it.speed <= 0.0 {
            return Err(bad(cmd, "a clip is played in reverse: re-frame it forwards"));
        }
        if s.project.source_size(it.item).is_none() {
            continue;
        }
        jobs.push(Job { clip: c, item: it.item, duration: it.duration, source_in: it.source_in, speed: it.speed });
    }
    if jobs.is_empty() {
        return Err(bad(cmd, "select the video clip(s) to re-frame (or pass `clips`)"));
    }
    Ok(jobs)
}

/// 24 x 14 luma thumbnail in 0 … 1 (box average).
fn thumbnail(img: &Image) -> Vec<f32> {
    let (tw, th) = (24usize, 14usize);
    let mut out = vec![0f32; tw * th];
    for ty in 0..th {
        for tx in 0..tw {
            let (x0, x1) = (tx * img.width / tw, ((tx + 1) * img.width / tw).max(tx * img.width / tw + 1));
            let (y0, y1) = (ty * img.height / th, ((ty + 1) * img.height / th).max(ty * img.height / th + 1));
            let mut acc = 0f32;
            let mut n = 0f32;
            for y in (y0..y1).step_by(2) {
                for x in (x0..x1).step_by(2) {
                    let i = (y * img.width + x) * 4;
                    if let Some(px) = img.rgba.get(i..i + 3) {
                        acc += 0.299 * f32::from(px[0]) + 0.587 * f32::from(px[1]) + 0.114 * f32::from(px[2]);
                        n += 1.0;
                    }
                }
            }
            if let Some(o) = out.get_mut(ty * tw + tx) {
                *o = acc / (n.max(1.0) * 255.0);
            }
        }
    }
    out
}

fn pct(x: f64) -> String {
    format!("{x:.1}").replace('.', ",")
}

/// The planned crop of one clip, in source pixels, before it is written.
struct Planned {
    clip: ClipId,
    /// (clip time, centre x, centre y, hold-after) keyframes
    keys: Vec<(Tick, f64, f64, bool)>,
    scale_pct: f64,
    src: (f64, f64),
    json: Value,
}

fn plan_clip(s: &Session, job: &Job, p: &Value, out: (u32, u32), root: &std::path::Path, fps_seq: f64, cmd: &str) -> Result<Planned> {
    let src_size = s.project.source_size(job.item).ok_or_else(|| bad(cmd, "no picture size for the clip"))?;
    let (sw, sh) = (f64::from(src_size.0), f64::from(src_size.1));
    let zoom = f64_p(p, "zoom").unwrap_or(1.0);
    if !(1.0..=4.0).contains(&zoom) {
        return Err(bad(cmd, "`zoom` must be between 1 and 4"));
    }
    let q_out = f64::from(out.0) / f64::from(out.1);
    let (cw0, ch0) = if q_out <= sw / sh { (sh * q_out, sh) } else { (sw, sw / q_out) };
    let (cw, ch) = (cw0 / zoom, ch0 / zoom);
    let scale_pct = f64::from(out.0) / cw * 100.0;
    let model = str_p(p, "model").unwrap_or("yolox-nano");
    let yolox = yolox_for(root, model, cmd)?;
    let use_faces = bool_p(p, "faces").unwrap_or(true);
    let yunet = if use_faces { yunet_for(root) } else { None };
    let prefer = match str_p(p, "subject").unwrap_or("auto") {
        "auto" => Prefer::Auto,
        "face" => Prefer::Face,
        other => return Err(bad(cmd, format!("unknown subject `{other}` (auto, face)"))),
    };
    if prefer == Prefer::Face && yunet.is_none() {
        return Err(bad(cmd, "subject:face needs the YuNet model: `models.install {\"model\":\"yunet\",\"accept\":true}`"));
    }
    let det_fps = f64_p(p, "fps").unwrap_or(5.0).clamp(0.5, 15.0);
    let smooth = f64_p(p, "smooth").unwrap_or(0.3).clamp(0.0, 5.0);
    let dead = f64_p(p, "deadZone").unwrap_or(0.03).clamp(0.0, 0.5);
    let safe = f64_p(p, "safeBottom").unwrap_or(0.0).clamp(0.0, 0.6);
    let dur = job.duration.seconds();
    let n = ((dur * det_fps).ceil() as usize).max(1);
    if n > 6000 {
        return Err(bad(cmd, "the clip is too long for this sampling rate: lower `fps`"));
    }
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let src = filmcraft_render::SourceProvider::source(&provider, job.item).ok_or_else(|| bad(cmd, "le média du clip est hors ligne"))?;
    let want = (960.0 / sw).min(1.0) as f32;
    let mut prev: Option<Subject> = None;
    let mut samples: Vec<(f64, Option<Subject>, bool)> = Vec::with_capacity(n);
    let mut prev_thumb: Option<Vec<f32>> = None;
    let t_all = web_time::Instant::now();
    for k in 0..n {
        let rel = (k as f64 / det_fps).min((dur - 0.04).max(0.0));
        let media_t = job.source_in + Tick::from_seconds_f64(rel * job.speed);
        let frame = src.video_frame(FrameRequest { time: media_t, scale: want }).map_err(|e| bad(cmd, format!("lecture de l'image impossible : {e}")))?;
        let rgba = frame.to_rgba8();
        let img = Image { width: frame.width as usize, height: frame.height as usize, rgba: &rgba };
        let (kx, ky) = (sw / img.width.max(1) as f64, sh / img.height.max(1) as f64);
        let scale_box = |d: &Detection| Detection {
            x0: (f64::from(d.x0) * kx) as f32,
            y0: (f64::from(d.y0) * ky) as f32,
            x1: (f64::from(d.x1) * kx) as f32,
            y1: (f64::from(d.y1) * ky) as f32,
            score: d.score,
        };
        let persons: Vec<Detection> = yolox.detect(&img, 0.3).map_err(|e| bad(cmd, format!("détection impossible : {e}")))?.iter().map(scale_box).collect();
        let faces: Vec<Detection> = match &yunet {
            Some(y) => y.detect(&img, 0.7).map_err(|e| bad(cmd, format!("détection des visages impossible : {e}")))?.iter().map(scale_box).collect(),
            None => Vec::new(),
        };
        let subj = track::pick(prev.as_ref(), &persons, &faces, prefer);
        if subj.is_some() {
            prev = subj;
        }
        // a scene cut: the picture itself changes between two samples (mean luma difference of 24 x 14 thumbnails)
        let th = thumbnail(&img);
        let cut = prev_thumb.as_ref().is_some_and(|p| p.iter().zip(&th).map(|(a, b)| (a - b).abs()).sum::<f32>() / th.len().max(1) as f32 > 0.2);
        prev_thumb = Some(th);
        samples.push((rel, subj, cut));
    }
    let detected = samples.iter().filter(|(_, s, _)| s.is_some()).count();
    let coverage = detected as f64 / samples.len().max(1) as f64;
    // pieces between scene cuts: each is planned on its own, and the frame cuts with the picture
    let mut pieces: Vec<Vec<Sample>> = vec![Vec::new()];
    let mut cuts = 0usize;
    let mut cut_times: Vec<f64> = Vec::new();
    for (t, s, cut) in &samples {
        if *cut {
            cuts += 1;
            cut_times.push(*t);
            pieces.push(Vec::new());
        }
        if let (Some(s), Some(piece)) = (s, pieces.last_mut()) {
            piece.push(Sample { t: *t, x: s.x, y: s.y });
        }
    }
    let fps_path = fps_seq.clamp(10.0, 60.0);
    let aim_y = 0.5 - safe / 2.0;
    let opts = PathOptions { fps: fps_path, smooth, dead_zone: dead, aim_y };
    let mut keys: Vec<(Tick, f64, f64, bool)> = Vec::new();
    let mut raw_x: Vec<f64> = Vec::new();
    let mut shift_max = 0f64;
    for (i, piece) in pieces.iter().enumerate() {
        let t0 = if i == 0 { 0.0 } else { cut_times.get(i - 1).copied().unwrap_or(0.0) };
        let t1 = cut_times.get(i).copied().unwrap_or(dur);
        let shifted: Vec<Sample> = piece.iter().map(|s| Sample { t: s.t - t0, ..*s }).collect();
        let plan = path::plan(&shifted, t1 - t0, (sw, sh), (cw, ch), &opts);
        let kx = path::knots(&plan.cx, 1, 0.15);
        let ky = path::knots(&plan.cy, 1, 0.15);
        let mut idx: Vec<usize> = kx.into_iter().chain(ky).collect();
        idx.sort_unstable();
        idx.dedup();
        // keyframes stay before the next piece starts (sorted, one per instant)
        idx.retain(|f| i + 1 == pieces.len() || t0 + *f as f64 / plan.fps < t1);
        for (j, &f) in idx.iter().enumerate() {
            let t = t0 + f as f64 / plan.fps;
            let hold = j + 1 == idx.len() && i + 1 < pieces.len();
            keys.push((Tick::from_seconds_f64(t.min(dur)), plan.cx.get(f).copied().unwrap_or(sw / 2.0), plan.cy.get(f).copied().unwrap_or(sh / 2.0), hold));
        }
        // how far the window strays from the raw samples' own line (for the report)
        for sm in &shifted {
            let fi = ((sm.t * plan.fps).round() as usize).min(plan.cx.len().saturating_sub(1));
            let c = plan.cx.get(fi).copied().unwrap_or(sw / 2.0);
            shift_max = shift_max.max((sm.x - c).abs());
        }
        raw_x.extend(path::plan(&shifted, t1 - t0, (sw, sh), (cw, ch), &PathOptions { smooth: 0.0, dead_zone: 0.0, ..opts }).cx);
    }
    // overlap of the subject box with the caption band
    // share of the samples whose subject box reaches into the caption band of the output
    let overlap = if safe > 0.0 {
        let hit = samples
            .iter()
            .filter_map(|(_, s, _)| s.as_ref())
            .filter(|s| {
                // window top so the subject point sits at `aim_y`, clamped to the picture
                let top = (s.y - aim_y * ch).clamp(0.0, (sh - ch).max(0.0));
                (f64::from(s.bbox.y1) - top) / ch > 1.0 - safe
            })
            .count();
        100.0 * hit as f64 / detected.max(1) as f64
    } else {
        0.0
    };
    // what the Motion effect will really play: the keyframes evaluated at every frame, in output pixels
    let f = scale_pct / 100.0;
    let mut param = filmcraft_project::Param::new(ParamValue::Vec2(filmcraft_geom::Vec2::new(f64::from(out.0) / 2.0, f64::from(out.1) / 2.0)));
    param.keyframes = keys
        .iter()
        .map(|(t, cx, cy, hold)| {
            let v = filmcraft_geom::Vec2::new(f64::from(out.0) / 2.0 - (cx - sw / 2.0) * f, f64::from(out.1) / 2.0 - (cy - sh / 2.0) * f);
            let mut k = Keyframe::new(*t, ParamValue::Vec2(v));
            k.interp = if *hold { Interpolation::Hold } else { Interpolation::Linear };
            k
        })
        .collect();
    let frames = (dur * fps_path).floor() as usize;
    let played: Vec<f64> = (0..=frames).map(|i| param.value_at(Tick::from_seconds_f64(i as f64 / fps_path)).as_vec2().map_or(0.0, |v| v.x)).collect();
    let (jm, jx) = path::jerk(&played, fps_path);
    let raw_px: Vec<f64> = raw_x.iter().map(|x| -x * f).collect();
    let (rm, _) = path::jerk(&raw_px, fps_path);
    let max_speed = played.windows(2).map(|w| (w[1] - w[0]).abs() * fps_path).fold(0.0, f64::max);
    let debug = bool_p(p, "debug").unwrap_or(false);
    let mut report = json!({
        "clip": job.clip.0, "samples": samples.len(), "detected": detected, "coveragePercent": (coverage * 1000.0).round() / 10.0,
        "faces": samples.iter().filter(|(_, s, _)| s.as_ref().is_some_and(|s| s.has_face)).count(), "cuts": cuts,
        "crop": {"width": cw, "height": ch}, "scalePercent": scale_pct, "keyframes": keys.len(),
        "jerkPxPerS3": {"mean": jm, "max": jx, "unsmoothedMean": rm}, "maxSpeedPxPerS": max_speed, "maxDistanceToSamplesPx": shift_max,
        "captionOverlapPercent": overlap, "detectSeconds": t_all.elapsed().as_secs_f64(),
        "model": model, "faceModel": yunet.is_some(),
    });
    if debug {
        report["debug"] = json!({"fps": fps_path, "keys": keys.iter().map(|k| json!([k.0.seconds(), k.1, k.3])).collect::<Vec<_>>(), "playedX": played, "scale": f, "src": [sw, sh], "out": [out.0, out.1]});
    }
    Ok(Planned { clip: job.clip, keys, scale_pct, src: (sw, sh), json: report })
}

fn auto_reframe(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "sequence.autoReframe";
    let ratio = ratio_p(p, CMD)?;
    let jobs = collect_jobs(s, p, CMD)?;
    let root = match str_p(p, "modelsDir") {
        Some(d) => std::path::PathBuf::from(d),
        None => crate::transcript::models_dir().ok_or_else(|| EngineError::Other("no data directory for models".into()))?,
    };
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let seq_size = (q.settings.width, q.settings.height);
    let fps_seq = q.settings.frame_rate.as_f64();
    let out = output_size(ratio, seq_size);
    let resize = out != seq_size;
    if resize {
        let others = q.video_tracks.iter().flat_map(|t| t.items.iter()).filter(|i| !jobs.iter().any(|j| j.clip == i.id)).count();
        if others > 0 && !bool_p(p, "resizeSequence").unwrap_or(false) {
            return Err(bad(
                CMD,
                format!(
                    "la séquence fait {}×{} et le format {}:{} demande {}×{} : {others} autre(s) clip(s) vidéo seraient recadrés eux aussi. Dupliquez la séquence ou créez-en une au bon format (File ▸ New Sequence), ou passez `resizeSequence:true`",
                    seq_size.0, seq_size.1, ratio.0, ratio.1, out.0, out.1
                ),
            ));
        }
        if bool_p(p, "resizeSequence") == Some(false) {
            return Err(bad(CMD, "resizeSequence:false but the sequence does not have the target ratio"));
        }
    }
    let mut planned = Vec::new();
    for j in &jobs {
        planned.push(plan_clip(s, j, p, out, &root, fps_seq, CMD)?);
    }
    let apply = bool_p(p, "apply").unwrap_or(true);
    let n_keys: usize = planned.iter().map(|x| x.keys.len()).sum();
    let reports: Vec<Value> = planned.iter().map(|x| x.json.clone()).collect();
    let cov: f64 = reports.iter().filter_map(|r| r["coveragePercent"].as_f64()).sum::<f64>() / reports.len().max(1) as f64;
    if apply {
        let plans = planned;
        s.edit_sequence("Auto Reframe", move |q, _, _| {
            if resize {
                q.settings.width = out.0;
                q.settings.height = out.1;
            }
            for pl in plans {
                let (_, it) = q.find_item_mut(pl.clip).ok_or_else(|| bad(CMD, "a clip disappeared"))?;
                it.scale_to_frame = false;
                let (sw, sh) = pl.src;
                let f = pl.scale_pct / 100.0;
                // keyframes live on media time (source in + clip time x speed)
                let times: Vec<Tick> = pl.keys.iter().map(|k| it.source_time_at(it.start + k.0)).collect();
                let Some(m) = it.effect_mut("motion") else { continue };
                if let Some(sc) = m.param_mut("scale") {
                    sc.keyframes.clear();
                    sc.value = ParamValue::Float(pl.scale_pct);
                }
                if let Some(un) = m.param_mut("uniform_scale") {
                    un.value = ParamValue::Bool(true);
                }
                if let Some(pos) = m.param_mut("position") {
                    pos.keyframes = pl
                        .keys
                        .iter()
                        .zip(times)
                        .map(|((_, cx, cy, hold), t)| {
                            let v = filmcraft_geom::Vec2::new(f64::from(out.0) / 2.0 - (cx - sw / 2.0) * f, f64::from(out.1) / 2.0 - (cy - sh / 2.0) * f);
                            let mut k = Keyframe::new(t, ParamValue::Vec2(v));
                            k.interp = if *hold { Interpolation::Hold } else { Interpolation::Linear };
                            k
                        })
                        .collect();
                    if let Some(first) = pos.keyframes.first() {
                        pos.value = first.value.clone();
                    }
                }
            }
            Ok(())
        })?;
    }
    let clips = reports.len();
    Ok(json!({
        "ratio": format!("{}:{}", ratio.0, ratio.1), "output": {"width": out.0, "height": out.1}, "sequenceResized": resize && apply, "clips": reports, "applied": apply,
        "summary": format!(
            "{clips} clip{} recadré{} en {}×{} ({}:{}) : sujet trouvé sur {} % des images, {n_keys} keyframes de position{}",
            if clips > 1 { "s" } else { "" },
            if clips > 1 { "s" } else { "" },
            out.0,
            out.1,
            ratio.0,
            ratio.1,
            pct(cov),
            if apply { "" } else { " (rien appliqué)" }
        ),
    }))
}

fn has_video_clip(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    let q = s.active_sequence().ok_or("no sequence is open")?;
    if q.video_tracks.iter().any(|t| !t.items.is_empty()) { Ok(()) } else { Err("the sequence has no video clips".into()) }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "sequence.autoReframe",
        label: "Auto Reframe…",
        menu: &[],
        shortcut: None,
        params: r#"{"clips":[id]? (default: the selected video clips),"ratio":"9:16|1:1|4:5|w:h"="9:16","model":"yolox-nano|yolox-tiny|yolox-s"="yolox-nano","faces":bool=true (YuNet when installed),"subject":"auto|face"="auto","fps":n=5 (detections per second),"smooth":seconds=0.3,"deadZone":0..0.5=0.03,"safeBottom":0..0.6=0 (share of the output height kept free for subtitles),"zoom":1..4=1,"resizeSequence":bool? (set the sequence to the target size; refused when other video clips would be re-framed),"apply":bool=true,"modelsDir":str?}"#,
        enabled: has_video_clip,
        run: auto_reframe,
        journal: true,
        named: crate::commands::NamedKeys::NONE,
    }]
}
