//! Sequence / Markers menu long tail (M3.11).
//!
//! | Id | Menu | Notes |
//! |---|---|---|
//! | `sequence.applyDefaultTransitionsToSelection` | Sequence ▸ Apply Default Transitions to Selection (⇧D) | selected clips' edges; selected edit points |
//! | `sequence.normalizeMixTrack` | Sequence ▸ Normalize Mix Track… | Mix fader so the mix peaks at the target |
//! | `sequence.removeSilence` | (none; Extensions ▸ Silence Remover) | ripple-delete the quiet stretches of the mix, no transcript needed |
//! | `sequence.markBeats` / `sequence.cutOnBeats` | (none; Extensions ▸ Beat Edit) | markers / cuts on the beats of an audio clip, see `beats` |
//! | `sequence.transcribe` | Sequence ▸ Transcribe Sequence… | `transcript.generate` on the sequence's audio clips |
//! | `sequence.simplify` | Sequence ▸ Simplify Sequence… | a simplified copy of the sequence |
//! | `captions.showActiveOnly` | Sequence ▸ Captions ▸ Show Active Caption Tracks Only | |
//! | `markers.addFlashCue` | Markers ▸ Add Flash Cue Marker… | `MarkerKind::FlashCue` |
//!
//! **Simplify Sequence** (Premiere 26): makes a copy named "<name> (Simplified)" and opens it;
//! the original is untouched. Options: remove disabled clips, remove empty tracks, close gaps (spans
//! where no track has a clip), move video clips down to the lowest free track, remove video / audio
//! effects (standard effects; Motion, Opacity, Volume… are kept), remove text (graphic clips and
//! caption tracks), keep only video or only audio.

use filmcraft_edit as edit;
use filmcraft_project::{ClipId, ItemKind, Label, Marker, MarkerId, MarkerKind, Sequence, TrackId, TrackKind};
use filmcraft_time::{Tick, TimeRange};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, bool_p, f64_p, has_seq, str_p, time_p, u64_p};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

fn spec(
    id: &'static str,
    label: &'static str,
    menu: &'static [&'static str],
    shortcut: Option<&'static str>,
    params: &'static str,
    enabled: Enabled,
    run: Run,
) -> CommandSpec {
    CommandSpec { id, label, menu, shortcut, params, enabled, run, journal: true }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    let mut v = base_commands();
    v.extend(crate::beats::commands());
    v.extend(crate::word_captions::commands());
    v.extend(crate::captions_translate::commands());
    v.extend(crate::ml_tools::commands());
    v.extend(crate::ml_video::commands());
    v.extend(crate::ml_rush::commands());
    v.extend(crate::ml_audio::commands());
    v.extend(crate::dub::commands());
    v
}

fn base_commands() -> Vec<CommandSpec> {
    vec![
        spec(
            "sequence.applyDefaultTransitionsToSelection",
            "Apply Default Transitions to Selection",
            &["Sequence"],
            // ⇧D comes from the Premiere shortcut audit (`shortcut_presets::PREMIERE`)
            None,
            r#"{"clips":[id]?}"#,
            has_clips_or_edit_points,
            apply_default_transitions,
        ),
        spec("sequence.normalizeMixTrack", "Normalize Mix Track…", &["Sequence"], None, r#"{"db":f64=0}"#, has_audio_clips, normalize_mix),
        // No menu entry: it is the engine behind the Silence Remover extension (docs/extensions.md)
        spec(
            "sequence.removeSilence",
            "Remove Silences…",
            &[],
            None,
            r#"{"thresholdDb":db? (default: from the mix's noise floor),"minSeconds":f64=0.5,"keepSeconds":f64=0.1,"apply":bool=true}"#,
            has_audio_clips,
            remove_silence,
        ),
        spec(
            "sequence.transcribe",
            "Transcribe Sequence…",
            &["Sequence"],
            None,
            r#"{"track":"mix"|"A1"|id?,"language":"en|auto"?,"diarize":bool?,"maxSpeakers":n?,"model":str?}"#,
            can_transcribe_sequence,
            transcribe,
        ),
        spec(
            "sequence.simplify",
            "Simplify Sequence…",
            &["Sequence"],
            None,
            r#"{"name":str?,"removeDisabled":bool=true,"removeEmptyTracks":bool=true,"closeGaps":bool=false,"moveClipsDown":bool=false,"removeVideoEffects":bool=false,"removeAudioEffects":bool=false,"removeText":bool=false,"keep":"both|video|audio"}"#,
            has_seq,
            simplify,
        ),
        spec(
            "captions.showActiveOnly",
            "Show Active Caption Tracks Only",
            &["Sequence", "Captions"],
            None,
            r#"{"track":id|"C1"?}"#,
            has_caption_track,
            show_active_only,
        ),
        spec(
            "markers.addFlashCue",
            "Add Flash Cue Marker…",
            &["Markers"],
            None,
            r#"{"time":ticks?,"name":str?,"comment":str?,"color":label?}"#,
            has_seq,
            add_flash_cue,
        ),
    ]
}

// ---------------------------------------------------------------------------------------------
// enablement
// ---------------------------------------------------------------------------------------------

fn has_clips_or_edit_points(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if s.state.selection.is_empty() && s.state.edit_points.is_empty() { Err("select clips or edit points".into()) } else { Ok(()) }
}

fn has_audio_clips(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    let q = s.active_sequence().ok_or("no sequence is open")?;
    if q.audio_tracks.iter().any(|t| !t.items.is_empty()) { Ok(()) } else { Err("the sequence has no audio clips".into()) }
}

/// Transcribe Sequence runs `transcript.generate`: disabled like it in a build without
/// speech-to-text (#97), and when the sequence has no sound.
fn can_transcribe_sequence(s: &Session) -> std::result::Result<(), String> {
    crate::transcript::can_transcribe(s)?;
    has_audio_clips(s)
}

fn has_caption_track(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if s.active_sequence().is_some_and(|q| !q.caption_tracks.is_empty()) { Ok(()) } else { Err("the sequence has no caption track".into()) }
}

// ---------------------------------------------------------------------------------------------
// Apply Default Transitions to Selection
// ---------------------------------------------------------------------------------------------

/// Premiere applies the default video / audio transition at each edge of the selected clips
/// (centred on cuts between two clips, starting / ending at a clip's free edge). Edges that
/// already have a transition keep it. With edit points selected (trim mode) and no clips, the
/// transitions go at the edit points.
fn apply_default_transitions(s: &mut Session, p: &Value) -> Result<Value> {
    let clips: Vec<ClipId> = match p.get("clips").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|v| v.as_u64().map(ClipId)).collect(),
        None => s.state.selection.clone(),
    };
    if clips.is_empty() {
        return crate::trim::apply_default_transitions(s);
    }
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    // (track, clip, edge) per distinct cut
    let mut edges: Vec<(TrackKind, ClipId, &'static str, Tick, TrackId)> = Vec::new();
    for c in &clips {
        let Some((tid, it)) = q.find_item(*c) else { continue };
        let Some(tr) = q.track(tid) else { continue };
        for (edge, at) in [("in", it.start), ("out", it.end())] {
            if edges.iter().any(|e| e.4 == tid && e.3 == at) {
                continue;
            }
            // an existing transition at this cut stays
            if tr.transitions.iter().any(|x| x.start <= at && x.end() >= at && (x.from == Some(*c) || x.to == Some(*c))) {
                continue;
            }
            edges.push((tr.kind, *c, edge, at, tid));
        }
    }
    let n0 = s.history.undo.len();
    let mut ids = Vec::new();
    let mut errors = Vec::new();
    for (kind, clip, edge, _, _) in edges {
        let cmd = if kind == TrackKind::Video { "sequence.applyVideoTransition" } else { "sequence.applyAudioTransition" };
        match s.execute(cmd, json!({"clip": clip.0, "edge": edge})) {
            Ok(r) => ids.push(r["transition"].clone()),
            Err(e) => errors.push(e.to_string()),
        }
    }
    crate::clip_ops::collapse_history(s, n0, "Apply Default Transitions");
    if ids.is_empty() {
        return Err(EngineError::Other(errors.into_iter().next().unwrap_or_else(|| "no transition could be applied to the selection".into())));
    }
    Ok(json!({"transitions": ids}))
}

// ---------------------------------------------------------------------------------------------
// Normalize Mix Track
// ---------------------------------------------------------------------------------------------

/// Peak (dBFS) of the active sequence's mix over its whole duration, rendered through the mixer
/// graph exactly like playback and export.
pub fn mix_peak_db(s: &Session) -> Result<f64> {
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let q = s.project.sequence(seq_id).ok_or(EngineError::NoSequence)?;
    let sr = q.settings.sample_rate.max(1);
    let total = q.duration().to_units_floor(sr as i64);
    let provider = s.media.full_res_provider(s.project.clone(), s.services.clone());
    let mut peak = 0f32;
    let mut pos = 0i64;
    while pos < total {
        let n = (total - pos).min(sr as i64) as usize;
        let b = filmcraft_render::audio::mix_sequence(&s.project, q, pos, n, &provider);
        peak = b.peaks().into_iter().fold(peak, f32::max);
        pos += n as i64;
    }
    Ok(if peak > 0.0 { 20.0 * (peak as f64).log10() } else { f64::NEG_INFINITY })
}

/// Sequence ▸ Normalize Mix Track…: set the Mix fader so the mix peaks at `db` (default 0 dBFS).
/// The mix is measured, the fader moved by the difference, and measured again (post-fader
/// effects such as a limiter are not linear), up to three times. One undo step.
fn normalize_mix(s: &mut Session, p: &Value) -> Result<Value> {
    let target = f64_p(p, "db").unwrap_or(0.0);
    if !(-96.0..=24.0).contains(&target) {
        return Err(bad("sequence.normalizeMixTrack", "the target must be between -96 and +24 dB"));
    }
    let before = s.active_sequence().map(|q| q.master_volume_db).unwrap_or(0.0);
    let first = mix_peak_db(s)?;
    if !first.is_finite() {
        return Err(EngineError::Other("the mix is silent: nothing to normalize".into()));
    }
    let n0 = s.history.undo.len();
    let mut peak = first;
    for _ in 0..3 {
        let delta = target - peak;
        if delta.abs() < 0.01 {
            break;
        }
        s.edit_sequence("Normalize Mix Track", |q, _, _| {
            q.master_volume_db = (q.master_volume_db + delta).clamp(-96.0, 48.0);
            Ok(())
        })?;
        peak = mix_peak_db(s)?;
    }
    crate::clip_ops::collapse_history(s, n0, "Normalize Mix Track");
    let after = s.active_sequence().map(|q| q.master_volume_db).unwrap_or(0.0);
    Ok(json!({"peakBeforeDb": first, "peakDb": peak, "gainDb": after - before, "masterVolumeDb": after}))
}

// ---------------------------------------------------------------------------------------------
// Remove Silences
// ---------------------------------------------------------------------------------------------

/// Length of the level windows the mix is measured in.
pub(crate) const SILENCE_WINDOW_SECONDS: f64 = 0.01;

/// The level of the whole mix of the active sequence in dBFS RMS over all channels, one value per
/// [`SILENCE_WINDOW_SECONDS`] window (the mixer graph renders it, as for playback and export).
pub(crate) fn mix_levels(s: &Session) -> Result<Vec<f32>> {
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let q = s.project.sequence(seq_id).ok_or(EngineError::NoSequence)?;
    let sr = q.settings.sample_rate.max(1);
    let total = q.duration().to_units_floor(sr as i64).max(0);
    let window = ((sr as f64 * SILENCE_WINDOW_SECONDS).round() as usize).max(1);
    let provider = s.media.full_res_provider(s.project.clone(), s.services.clone());
    let mut db: Vec<f32> = Vec::new();
    let mut pos = 0i64;
    let block = (window * 100) as i64;
    while pos < total {
        let n = (total - pos).min(block) as usize;
        let b = filmcraft_render::audio::mix_sequence(&s.project, q, pos, n, &provider);
        let channels = b.channels.len().max(1);
        for w in 0..n.div_ceil(window) {
            let (from, to) = (w * window, ((w + 1) * window).min(n));
            let mut e = 0f64;
            for ch in &b.channels {
                e += ch.get(from..to).unwrap_or_default().iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>();
            }
            let count = ((to - from) * channels).max(1) as f64;
            db.push((10.0 * (e / count + 1e-12).log10()) as f32);
        }
        pos += n as i64;
    }
    Ok(db)
}

/// Find the stretches of the sequence mix that stay quiet for `minSeconds` or more and, unless
/// `apply` is false, ripple-delete them on every unlocked track (one undo step), keeping
/// `keepSeconds` of air next to the speech on both sides. A silence at the very start or end of the
/// sequence is removed up to the sound (no air is kept against the edge).
///
/// The mix is rendered through the mixer graph, like playback and export, and measured in 10 ms RMS
/// windows. The quiet/loud threshold is `thresholdDb`, or by default the one speech tightening uses
/// (`filmcraft_speech::vad::threshold`: 30 % of the way from the recording's noise floor to its
/// speech level, at least 6 dB above the floor). No transcript or speech model is needed.
fn remove_silence(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "sequence.removeSilence";
    let min = f64_p(p, "minSeconds").unwrap_or(0.5);
    let keep = f64_p(p, "keepSeconds").unwrap_or(0.1);
    let apply = bool_p(p, "apply").unwrap_or(true);
    let manual = f64_p(p, "thresholdDb");
    if !(0.05..=3600.0).contains(&min) {
        return Err(bad(CMD, "`minSeconds` must be between 0.05 and 3600"));
    }
    if !(0.0..=min / 2.0).contains(&keep) {
        return Err(bad(CMD, "`keepSeconds` must be between 0 and half of `minSeconds`"));
    }
    if manual.is_some_and(|t| !(-100.0..=0.0).contains(&t)) {
        return Err(bad(CMD, "`thresholdDb` must be between -100 and 0"));
    }
    let db = mix_levels(s)?;
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let q = s.project.sequence(seq_id).ok_or(EngineError::NoSequence)?;
    let threshold = manual.map(|t| t as f32).unwrap_or_else(|| filmcraft_speech::vad::threshold(&db));
    let rate = q.settings.frame_rate;
    let end_of_sequence = q.duration();
    let at = |frames: usize| Tick::from_seconds_f64(frames as f64 * SILENCE_WINDOW_SECONDS);
    let mut ranges: Vec<TimeRange> = Vec::new();
    let mut i = 0usize;
    while i < db.len() {
        if db.get(i).copied().unwrap_or(0.0) >= threshold {
            i += 1;
            continue;
        }
        let a = i;
        while db.get(i).is_some_and(|v| *v < threshold) {
            i += 1;
        }
        let b = i;
        if (b - a) as f64 * SILENCE_WINDOW_SECONDS < min {
            continue;
        }
        let start = if a == 0 { Tick::ZERO } else { at(a) + Tick::from_seconds_f64(keep) };
        let end = if b >= db.len() { end_of_sequence } else { at(b) - Tick::from_seconds_f64(keep) };
        // snap inward to frames, as pauses are
        let mut s2 = rate.snap(start);
        if s2 < start {
            s2 += rate.frame_duration();
        }
        let e2 = rate.snap(end);
        if e2 > s2 {
            ranges.push(TimeRange::from_bounds(s2, e2));
        }
    }
    let silences: Vec<Value> = ranges.iter().map(|r| json!({"start": r.start.seconds(), "end": r.end().seconds()})).collect();
    let mut out = json!({"silences": silences.len(), "thresholdDb": threshold, "ranges": silences, "removed": 0, "seconds": 0.0});
    let found = ranges.len();
    let planned: f64 = ranges.iter().map(|r| r.duration.seconds()).sum();
    let thr = format!("{threshold:.0}").replace('-', "−");
    let min_text = format!("{min}").replace('.', ",");
    if apply && !ranges.is_empty() {
        let r = crate::transcript::remove_ranges(s, "Remove Silences", ranges)?;
        out["removed"] = r["removed"].clone();
        out["seconds"] = r["seconds"].clone();
    }
    out["summary"] = json!(if found == 0 {
        format!(
            "Aucun silence de {min_text} s ou plus (seuil {thr} dB) : rien à supprimer. Essayez une durée plus courte (minSeconds) ou un seuil plus haut (thresholdDb)."
        )
    } else if !apply {
        format!(
            "{found} silence{} de {min_text} s ou plus trouvé{}, {planned:.1} s à retirer (seuil {thr} dB), rien supprimé",
            if found == 1 { "" } else { "s" },
            if found == 1 { "" } else { "s" }
        )
        .replace('.', ",")
    } else {
        format!("{found} silence{} supprimé{}, {planned:.1} s retirées (seuil {thr} dB)", if found == 1 { "" } else { "s" }, if found == 1 { "" } else { "s" })
            .replace('.', ",")
    });
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Transcribe Sequence
// ---------------------------------------------------------------------------------------------

/// Sequence ▸ Transcribe Sequence…: transcribe the media of the sequence's audio clips (all audio
/// tracks — "Mix" — or one track) with `transcript.generate`.
fn transcribe(s: &mut Session, p: &Value) -> Result<Value> {
    let params = transcribe_params(s, p)?;
    s.execute("transcript.generate", params)
}

/// The `transcript.generate` parameters of `sequence.transcribe`: the media of the audio clips of the
/// sequence (all tracks, or one) and the options passed on.
pub(crate) fn transcribe_params(s: &Session, p: &Value) -> Result<Value> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let track = match p.get("track") {
        None => None,
        Some(Value::String(t)) if t.eq_ignore_ascii_case("mix") => None,
        Some(_) => Some(crate::commands::track_p(s, p, "track", "sequence.transcribe")?.ok_or_else(|| bad("sequence.transcribe", "unknown `track`"))?),
    };
    let mut items = Vec::new();
    for t in q.audio_tracks.iter().filter(|t| track.is_none_or(|x| x == t.id)) {
        for it in t.items.iter().filter(|i| i.enabled) {
            if !items.contains(&it.item.0) {
                items.push(it.item.0);
            }
        }
    }
    if items.is_empty() {
        return Err(EngineError::Other("there are no audio clips to transcribe".into()));
    }
    let mut params = json!({"items": items});
    for k in ["language", "diarize", "maxSpeakers", "model", "prompt", "hesitations", "hesitationBias"] {
        if let Some(v) = p.get(k) {
            params[k] = v.clone();
        }
    }
    Ok(params)
}

// ---------------------------------------------------------------------------------------------
// Simplify Sequence
// ---------------------------------------------------------------------------------------------

/// What Simplify Sequence does to the copy.
#[derive(Clone, Debug)]
pub struct SimplifyOptions {
    pub remove_disabled: bool,
    pub remove_empty_tracks: bool,
    pub close_gaps: bool,
    pub move_clips_down: bool,
    pub remove_video_effects: bool,
    pub remove_audio_effects: bool,
    pub remove_text: bool,
    /// `both`, `video` or `audio`.
    pub keep: String,
}

impl SimplifyOptions {
    pub fn from_params(p: &Value) -> Result<Self> {
        let keep = str_p(p, "keep").unwrap_or("both").to_ascii_lowercase();
        if !matches!(keep.as_str(), "both" | "video" | "audio") {
            return Err(bad("sequence.simplify", "`keep` is both, video or audio"));
        }
        Ok(SimplifyOptions {
            remove_disabled: bool_p(p, "removeDisabled").unwrap_or(true),
            remove_empty_tracks: bool_p(p, "removeEmptyTracks").unwrap_or(true),
            close_gaps: bool_p(p, "closeGaps").unwrap_or(false),
            move_clips_down: bool_p(p, "moveClipsDown").unwrap_or(false),
            remove_video_effects: bool_p(p, "removeVideoEffects").unwrap_or(false),
            remove_audio_effects: bool_p(p, "removeAudioEffects").unwrap_or(false),
            remove_text: bool_p(p, "removeText").unwrap_or(false),
            keep,
        })
    }
}

fn is_text(p: &filmcraft_project::Project, it: &filmcraft_project::TrackItem) -> bool {
    p.item(it.item).is_some_and(|x| matches!(x.kind, ItemKind::Graphic { .. }))
}

/// Apply Simplify Sequence to `q` (a copy). Returns what changed.
pub fn simplify_sequence(proj: &filmcraft_project::Project, q: &mut Sequence, o: &SimplifyOptions) -> Value {
    let mut removed_clips = 0usize;
    // keep only video / audio
    if o.keep == "video" {
        q.audio_tracks.clear();
    } else if o.keep == "audio" {
        q.video_tracks.clear();
    }
    let mut drop: Vec<ClipId> = Vec::new();
    for t in q.video_tracks.iter().chain(q.audio_tracks.iter()) {
        for it in &t.items {
            if (o.remove_disabled && !it.enabled) || (o.remove_text && is_text(proj, it)) {
                drop.push(it.id);
            }
        }
    }
    removed_clips += edit::delete_items(q, &drop);
    // linked partners removed with "keep" lose their link
    let mut links: std::collections::BTreeMap<u64, usize> = Default::default();
    for t in q.all_tracks() {
        for it in &t.items {
            if let Some(l) = it.link {
                *links.entry(l).or_default() += 1;
            }
        }
    }
    for t in q.video_tracks.iter_mut().chain(q.audio_tracks.iter_mut()) {
        for it in &mut t.items {
            if it.link.is_some_and(|l| links.get(&l).copied().unwrap_or(0) < 2) {
                it.link = None;
            }
        }
        edit::remove_orphan_transitions(t);
    }
    if o.remove_text {
        q.caption_tracks.clear();
    }
    // effects (standard ones; intrinsic Motion / Opacity / Volume… and Essential Sound stay)
    let mut removed_effects = 0usize;
    for (video, tracks) in [(true, &mut q.video_tracks), (false, &mut q.audio_tracks)] {
        if (video && !o.remove_video_effects) || (!video && !o.remove_audio_effects) {
            continue;
        }
        for t in tracks.iter_mut() {
            for it in &mut t.items {
                let n = it.effects.len();
                it.effects.retain(|e| e.def().is_none_or(|d| d.intrinsic || filmcraft_project::graphic::is_layer_id(d.id)) || e.essential);
                removed_effects += n - it.effects.len();
            }
            if !video {
                let n = t.effects.len();
                t.effects.clear();
                removed_effects += n;
            }
        }
    }
    // move video clips down to the lowest track where they fit (clips with transitions stay)
    let mut moved = 0usize;
    if o.move_clips_down {
        for ti in 1..q.video_tracks.len() {
            let ids: Vec<ClipId> = q.video_tracks[ti].items.iter().map(|i| i.id).collect();
            for id in ids {
                let Some(pos) = q.video_tracks[ti].items.iter().position(|i| i.id == id) else { continue };
                let it = q.video_tracks[ti].items[pos].clone();
                if q.video_tracks[ti].transitions.iter().any(|x| x.from == Some(id) || x.to == Some(id)) {
                    continue;
                }
                let Some(dest) = (0..ti).find(|&d| !q.video_tracks[d].locked && edit::track_range_empty(&q.video_tracks[d], it.range())) else { continue };
                q.video_tracks[ti].items.remove(pos);
                q.video_tracks[dest].items.push(it);
                q.video_tracks[dest].sort();
                moved += 1;
            }
        }
    }
    // close gaps: spans where no track has anything
    let mut closed = Tick::ZERO;
    if o.close_gaps {
        let mut spans: Vec<TimeRange> = q.all_tracks().flat_map(|t| t.items.iter().map(|i| i.range())).collect();
        spans.extend(q.caption_tracks.iter().flat_map(|t| t.captions.iter().map(|c| c.range())));
        spans.sort_by_key(|r| r.start);
        let mut gaps: Vec<TimeRange> = Vec::new();
        let mut cursor = Tick::ZERO;
        for r in &spans {
            if r.start > cursor {
                gaps.push(TimeRange::from_bounds(cursor, r.start));
            }
            cursor = cursor.max(r.end());
        }
        for g in gaps.iter().rev() {
            for t in q.video_tracks.iter_mut().chain(q.audio_tracks.iter_mut()) {
                edit::shift_track_from(t, g.end(), Tick::ZERO - g.duration);
            }
            for t in &mut q.caption_tracks {
                for c in t.captions.iter_mut().filter(|c| c.start >= g.end()) {
                    c.start -= g.duration;
                }
            }
            crate::sequence_tools::ripple_markers(&mut q.markers, g.end(), Tick::ZERO - g.duration);
            closed += g.duration;
        }
    }
    // empty tracks (one video and one audio track always stay unless "keep" dropped the kind)
    let mut removed_tracks = 0usize;
    if o.remove_empty_tracks {
        for tracks in [&mut q.video_tracks, &mut q.audio_tracks] {
            // all empty: the first track stays so the sequence remains editable
            let all_empty = tracks.iter().all(|t| t.items.is_empty());
            let n = tracks.len();
            let mut first = true;
            tracks.retain(|t| {
                let keep = !t.items.is_empty() || (all_empty && first);
                first = false;
                keep
            });
            removed_tracks += n - tracks.len();
        }
    }
    json!({"removedClips": removed_clips, "removedEffects": removed_effects, "movedClips": moved, "closedGaps": closed.0, "removedTracks": removed_tracks})
}

fn simplify(s: &mut Session, p: &Value) -> Result<Value> {
    let o = SimplifyOptions::from_params(p)?;
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let src = s.project.item(seq_id).cloned().ok_or(EngineError::NoSequence)?;
    let name = str_p(p, "name").map(str::to_string).unwrap_or_else(|| format!("{} (Simplified)", src.name));
    let (id, report) = s.edit("Simplify Sequence", |pr, st| {
        let ItemKind::Sequence(q) = &src.kind else { return Err(EngineError::NoSequence) };
        let mut q = (**q).clone();
        let report = simplify_sequence(pr, &mut q, &o);
        q.check().map_err(EngineError::Other)?;
        let bin = pr.root.parent_of(seq_id).filter(|b| b.0 != 0);
        let id = pr.add_item(&name, src.label, ItemKind::Sequence(Box::new(q)), bin);
        st.active_sequence = Some(id);
        if !st.open_sequences.contains(&id) {
            st.open_sequences.push(id);
        }
        st.selection.clear();
        st.project_selection = vec![id];
        Ok((id, report))
    })?;
    s.events.push(crate::Event::OpenSequence(id));
    let mut out = report;
    out["sequence"] = json!(id.0);
    out["name"] = json!(name);
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Captions ▸ Show Active Caption Tracks Only
// ---------------------------------------------------------------------------------------------

/// The active caption track: the one named by `track`, else the track of the selected caption,
/// else a targeted caption track, else the top one.
fn active_caption_track(s: &Session, p: &Value) -> Option<TrackId> {
    let q = s.active_sequence()?;
    if let Some(v) = p.get("track") {
        if let Some(id) = v.as_u64() {
            return q.caption_track(TrackId(id)).map(|t| t.id);
        }
        let n: usize = v.as_str()?.trim_start_matches(['C', 'c']).parse().ok()?;
        return q.caption_tracks.get(n.checked_sub(1)?).map(|t| t.id);
    }
    if let Some(t) = s.state.caption_selection.iter().find_map(|c| q.find_caption(*c).map(|x| x.0)) {
        return Some(t);
    }
    let tg = s.targeting().targeted;
    q.caption_tracks.iter().find(|t| tg.contains(&t.id)).or(q.caption_tracks.first()).map(|t| t.id)
}

fn show_active_only(s: &mut Session, p: &Value) -> Result<Value> {
    let active = active_caption_track(s, p).ok_or_else(|| bad("captions.showActiveOnly", "no such caption track"))?;
    s.edit("Show Active Caption Tracks Only", |pr, st| {
        let seq = st.active_sequence.ok_or(EngineError::NoSequence)?;
        for t in &mut pr.sequence_mut(seq).ok_or(EngineError::NoSequence)?.caption_tracks {
            t.enabled = t.id == active;
        }
        Ok(())
    })?;
    Ok(json!({"track": active.0}))
}

// ---------------------------------------------------------------------------------------------
// Markers ▸ Add Flash Cue Marker…
// ---------------------------------------------------------------------------------------------

fn add_flash_cue(s: &mut Session, p: &Value) -> Result<Value> {
    let t = s.sequence_rate().snap(time_p(s, p, "").unwrap_or(s.playhead()));
    let n = s.active_sequence().map(|q| q.markers.iter().filter(|m| m.kind == MarkerKind::FlashCue).count()).unwrap_or(0) + 1;
    let name = str_p(p, "name").map(str::to_string).unwrap_or_else(|| format!("Cue Point {n}"));
    let comment = str_p(p, "comment").unwrap_or("").to_string();
    let color = str_p(p, "color").and_then(Label::from_name).unwrap_or(Label::Green);
    let dur = u64_p(p, "durationFrames").map(|f| s.sequence_rate().tick_of(f as i64)).unwrap_or(Tick::ZERO);
    let id = s.edit_sequence("Add Flash Cue Marker", |q, ctx, _| {
        let id = MarkerId(ctx.alloc());
        q.markers.push(Marker { id, start: t, duration: dur, name, comment, kind: MarkerKind::FlashCue, color });
        q.markers.sort_by_key(|m| m.start);
        Ok(id)
    })?;
    Ok(json!({"marker": id.0}))
}

#[cfg(test)]
#[path = "sequence_extras_tests.rs"]
mod tests;
