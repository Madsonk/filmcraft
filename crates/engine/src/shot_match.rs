//! Clip ▸ Match Shot Colour… (`color.matchShots`): make a target clip look like a reference clip
//! (two cameras of the same scene) and keep the correction as an ordinary project LUT.
//!
//! 1. A few frames spread over each clip (`frames`, default 5, at the middles of equal slices) are
//!    rendered at a quarter of the sequence size. The **reference** is rendered as it plays (all
//!    its effects). The **target** is rendered as its Lumetri Creative Look slot will see it:
//!    its Input LUT and Basic Correction (the log / flat → Rec.709 conversion) stay, while the
//!    look itself (intensity 0), the wheels, curves and HSL Secondary are switched off, so a
//!    previous match does not skew the new one.
//! 2. The pixels go to [`filmcraft_color::shot_match`]: crushed blacks and clipped whites are left
//!    out, and the chosen method (Reinhard by default, MKL or per-channel histogram, all in
//!    Oklab) is baked into a 33³ `.cube`, mixed with the identity by `strength` (0–100 %).
//! 3. The LUT joins the project's LUT library (`lib:lutNN`, also written to `path` when given)
//!    and, with `apply` (default), becomes the target's Lumetri Creative Look at intensity 100
//!    (Lumetri is added when the clip has none). Library entry + look = **one undo step**. Media
//!    files are never written.
//!
//! The result reports the distance between the mean Oklab colours of the two shots (× 100, ≈ ΔE)
//! before, predicted after (the samples through the LUT) and, when applied, measured after (the
//! target re-rendered with its new look, on the same frames).

use filmcraft_color::shot_match::{self, LUT_SIZE, Method};
use filmcraft_color::{GradeSpace, WorkingSpace};
use filmcraft_project::{ClipId, ItemId, ParamValue, ProjectLut, TrackKind};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, bool_p, clip_p, f64_p, has_seq, str_p, u64_p};
use crate::{Result, Session};

const CMD: &str = "color.matchShots";
/// Frames sampled per clip: default and bounds.
const FRAMES: u64 = 5;
const MAX_FRAMES: u64 = 24;
/// Render scale of the sampled frames and the widest picture kept for the statistics.
const SCALE: f32 = 0.25;
const MAX_W: usize = 320;

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: CMD,
        label: "Match Shot Colour…",
        menu: &["Clip"],
        shortcut: None,
        params: r#"{"referenceClip":id,"targetClip":id,"method":"reinhard"|"mkl"|"histogram"="reinhard","strength":n=100,"apply":bool=true,"frames":n=5,"name":str?,"path":str?}"#,
        enabled: has_seq,
        run: match_shots,
        journal: true,
    }]
}

/// A video clip of the active sequence: its media item, start and end.
fn video_clip(s: &Session, c: ClipId, what: &str) -> Result<(ItemId, Tick, Tick)> {
    let q = s.active_sequence().ok_or_else(|| bad(CMD, "no active sequence"))?;
    let (t, it) = q.find_item(c).ok_or_else(|| bad(CMD, format!("no clip {} in the active sequence ({what})", c.0)))?;
    if q.track(t).is_none_or(|t| t.kind != TrackKind::Video) {
        return Err(bad(CMD, format!("clip {} is not a video clip ({what})", c.0)));
    }
    if it.duration <= Tick::ZERO {
        return Err(bad(CMD, format!("clip {} is empty ({what})", c.0)));
    }
    Ok((it.item, it.start, it.end()))
}

/// `n` times at the middles of `n` equal slices of `[start, end)`.
fn sample_times(start: Tick, end: Tick, n: u64) -> Vec<Tick> {
    let d = end.0.saturating_sub(start.0).max(1) as i128;
    let n = n.max(1) as i128;
    (0..n).map(|i| Tick(start.0.saturating_add((d * (2 * i + 1) / (2 * n)) as i64))).collect()
}

/// The display-signal pixels (unpremultiplied, sRGB-encoded Rec.709) of a clip at `times`.
fn signal_pixels(s: &Session, project: &filmcraft_project::Project, seq: ItemId, clip: ClipId, times: &[Tick]) -> Vec<[f32; 3]> {
    let provider = s.media.provider(std::sync::Arc::new(project.clone()), s.services.clone());
    let opts = filmcraft_render::RenderOptions { scale: SCALE, working_output: true, ..Default::default() };
    let gs = GradeSpace::new(WorkingSpace::Rec709, filmcraft_color::grade::DEFAULT_HDR_WHITE_NITS);
    let mut out = Vec::new();
    for &t in times {
        let Some(img) = filmcraft_render::render_clip(project, seq, clip, t, opts, &provider) else { continue };
        let img = filmcraft_render::color_match::shrink(&img, MAX_W);
        for p in img.px.as_chunks::<4>().0 {
            if p[3] > 0.5 {
                out.push(gs.encode([p[0] / p[3], p[1] / p[3], p[2] / p[3]]));
            }
        }
    }
    out
}

/// The project as the target clip's Creative Look slot sees it: the look, wheels, curves and
/// HSL Secondary of its (first) Lumetri switched off.
fn before_look(project: &filmcraft_project::Project, seq: ItemId, clip: ClipId) -> filmcraft_project::Project {
    let mut p = project.clone();
    if let Some((_, it)) = p.sequence_mut(seq).and_then(|q| q.find_item_mut(clip))
        && let Some(e) = it.effects.iter_mut().find(|e| e.effect == "lumetri")
    {
        for (k, v) in [
            ("look_intensity", ParamValue::Float(0.0)),
            ("wheels_on", ParamValue::Bool(false)),
            ("curves_on", ParamValue::Bool(false)),
            ("hsl_on", ParamValue::Bool(false)),
        ] {
            let prm = e.params.entry(k.to_string()).or_insert_with(|| filmcraft_project::Param::new(v.clone()));
            prm.keyframes.clear();
            prm.value = v;
        }
    }
    p
}

fn match_shots(s: &mut Session, p: &Value) -> Result<Value> {
    let reference = clip_p(p, "referenceClip").ok_or_else(|| bad(CMD, "need `referenceClip` (the shot to look like)"))?;
    let target = clip_p(p, "targetClip").ok_or_else(|| bad(CMD, "need `targetClip` (the shot to correct)"))?;
    if reference == target {
        return Err(bad(CMD, "the reference and the target are the same clip"));
    }
    let method = match str_p(p, "method") {
        None => Method::Reinhard,
        Some(m) => Method::parse(m).ok_or_else(|| bad(CMD, format!("unknown method `{m}` (reinhard, mkl, histogram)")))?,
    };
    let strength = f64_p(p, "strength").unwrap_or(100.0);
    if !strength.is_finite() || !(0.0..=100.0).contains(&strength) {
        return Err(bad(CMD, "`strength` must be between 0 and 100"));
    }
    let frames = u64_p(p, "frames").unwrap_or(FRAMES);
    if !(1..=MAX_FRAMES).contains(&frames) {
        return Err(bad(CMD, format!("`frames` must be between 1 and {MAX_FRAMES}")));
    }
    let apply = bool_p(p, "apply").unwrap_or(true);
    let seq = s.state.active_sequence.ok_or_else(|| bad(CMD, "no active sequence"))?;
    let working = s.active_sequence().map(|q| q.settings.color.working).unwrap_or(WorkingSpace::Rec709);
    if working != WorkingSpace::Rec709 {
        return Err(bad(CMD, "shot matching works in Rec.709 sequences only (Sequence ▸ Color Management)"));
    }
    let (ref_item, rs, re) = video_clip(s, reference, "reference")?;
    let (tgt_item, ts, te) = video_clip(s, target, "target")?;
    let (ref_times, tgt_times) = (sample_times(rs, re, frames), sample_times(ts, te, frames));

    let ref_px = signal_pixels(s, &s.project, seq, reference, &ref_times);
    let probe = before_look(&s.project, seq, target);
    let tgt_px = signal_pixels(s, &probe, seq, target, &tgt_times);
    let (ref_lab, tgt_lab) = (shot_match::samples(&ref_px), shot_match::samples(&tgt_px));
    let m = shot_match::solve(&tgt_lab, &ref_lab, method).map_err(|e| bad(CMD, e))?;

    let item_name = |i: ItemId| s.project.item(i).map(|x| x.name.clone()).unwrap_or_else(|| format!("{}", i.0));
    let name = match str_p(p, "name") {
        Some(n) if !n.trim().is_empty() => n.trim().to_string(),
        _ => format!("Match {} → {} ({}, {}%)", item_name(tgt_item), item_name(ref_item), method.id(), strength.round()),
    };
    let cube = m.lut(LUT_SIZE, (strength / 100.0) as f32, &name);
    let text = cube.to_cube();
    if let Some(path) = str_p(p, "path") {
        std::fs::write(path, &text).map_err(|e| bad(CMD, format!("{path}: {e}")))?;
    }
    let before = shot_match::mean_distance(&tgt_lab, &ref_lab);
    let mapped: Vec<[f32; 3]> = tgt_px.iter().filter(|v| shot_match::usable(**v)).map(|v| shot_match::signal_to_oklab(cube.apply(*v))).collect();
    let predicted = shot_match::mean_distance(&mapped, &ref_lab);

    let n0 = s.history.undo.len();
    let mut id = String::new();
    s.edit("Match Shot Colour", |pr, _| {
        id = format!("lut{}", pr.alloc_id());
        pr.luts.push(ProjectLut {
            id: id.clone(),
            name: name.clone(),
            source_path: str_p(p, "path").map(str::to_string),
            format: "cube".into(),
            text: text.as_str().into(),
        });
        Ok(())
    })?;
    let lut_ref = format!("lib:{id}");
    let mut measured = Value::Null;
    if apply {
        let applied = (|| -> Result<()> {
            let (clip, idx) = crate::color::lumetri_clip(s, &json!({"clip": target.0}), CMD)?;
            s.edit_sequence("Match Shot Colour", |q, _, _| {
                let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
                let e = it.effects.get_mut(idx).ok_or_else(|| bad(CMD, "no Lumetri"))?;
                for (k, v) in
                    [("look_lut", ParamValue::Text(lut_ref.clone())), ("look_intensity", ParamValue::Float(100.0)), ("creative_on", ParamValue::Bool(true))]
                {
                    let prm = e.params.entry(k.to_string()).or_insert_with(|| filmcraft_project::Param::new(v.clone()));
                    prm.keyframes.clear();
                    prm.value = v;
                }
                e.enabled = true;
                Ok(())
            })
        })();
        crate::clip_ops::collapse_history(s, n0, "Match Shot Colour");
        if let Err(e) = applied {
            // keep the project as it was: drop the half-done step
            let _ = s.execute("edit.undo", json!({}));
            return Err(e);
        }
        let after_px = signal_pixels(s, &s.project, seq, target, &tgt_times);
        measured = json!(shot_match::mean_distance(&shot_match::samples(&after_px), &ref_lab));
    }
    Ok(json!({
        "lut": lut_ref,
        "id": id,
        "name": name,
        "method": method.id(),
        "strength": strength,
        "frames": frames,
        "applied": apply,
        "referenceClip": reference.0,
        "targetClip": target.0,
        "samples": {"reference": m.reference_samples, "target": m.target_samples},
        "meanDistance": {"before": before, "predictedAfter": predicted, "measuredAfter": measured},
        "path": str_p(p, "path"),
    }))
}
