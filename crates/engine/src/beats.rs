//! Beat editing: markers and cuts on the beats of an audio clip.
//!
//! | command | menu | what it does |
//! |---|---|---|
//! | `sequence.markBeats` | (none; Extensions ▸ Beat Edit) | analyses an audio clip (the beat tracker of Clip ▸ Remix) and puts a marker on its beats, one or every N-th, as one undo step |
//! | `sequence.cutOnBeats` | (none; Extensions ▸ Beat Edit) | cuts the selected clips (or `clips`) at the beats of the audio clip, one or every N-th, as one undo step |
//!
//! The beats are those [`filmcraft_audio_dsp::beatgrid::analyze_grid`] finds in the part of the
//! clip's media the clip plays, snapped to the nearest frame unless `snap` is false. The tracker
//! follows one steady tempo and has no sensitivity setting: a clip without a clear pulse (ambient
//! music, speech, noise, a few seconds) is refused with a message saying so. Only clips played at
//! 100 % forwards are analysed (a speed change or a remixed clip is refused, not guessed at).
//!
//! **Tempo octave.** The grid weighs the tempo found, half of it and double it (65–172 BPM without a
//! hint) and keeps the one whose beats explain the most onsets without leaving strong onsets between
//! them: drums with hi-hats on the eighths or with a kick stronger than the snare are no longer read at
//! double or half tempo. A slow tune with eighth hats can still be read at double (80 → 160): give the
//! tempo with `bpm` (any octave, 30–300; the nearest tempo in the audio is used) or thin the beats with
//! `every`. `subdivide: 2` adds a beat between each pair of detected beats (derived, not detected).
//!
//! **Bars.** `downbeat: true` also looks for the first beat of the bars (accent, kick weight and
//! chord change, four phases) and starts there: markers and cuts then fall on bar lines when `every`
//! is 4. With chord changes it is reliable; on bare drums the first and third beats of a bar look alike
//! and the answer can be half a bar off: the result says how sure it is (`barConfidence`).
//!
//! Beat numbers count from 1 at the first beat found (or at the bar start); `offset` skips that many
//! beats and `every` keeps one beat in N from there; the name's number counts the kept beats (bar
//! numbers for `every: 4`).
//!
//! Every command returns `summary`, a line for the status bar, and refuses with a clear message when it
//! finds nothing to do.

use std::sync::{Arc, Mutex, OnceLock};

use filmcraft_audio_dsp::beatgrid::{self, Grid, GridOptions};
use filmcraft_edit as edit;
use filmcraft_project::{ClipId, ItemId, Label, Marker, MarkerId, MarkerKind, TrackKind};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, bool_p, has_seq, str_p, u64_p};
use crate::{EngineError, Result, Session};

/// Beat times found in a clip.
struct Beats {
    clip: ClipId,
    bpm: f64,
    /// (1-based number among the kept beats, sequence time) of every beat inside the clip.
    at: Vec<(usize, Tick)>,
    /// The bars were looked for: (beat that starts them, how sure 0 … 1).
    bars: Option<(usize, f64)>,
    /// Beats the analysis found in the clip, before `offset` / `every` / the clip's edges.
    detected: usize,
}

type GridKey = (u64, i64, i64, u32, u64, bool, bool);

fn grid_cache() -> &'static Mutex<Vec<(GridKey, Arc<Grid>)>> {
    static C: OnceLock<Mutex<Vec<(GridKey, Arc<Grid>)>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// The beat grid of `[source_in, source_in + dur)` of the item's media (cached per range, rate and options).
fn grid(s: &Session, item: ItemId, source_in: Tick, dur: Tick, opts: GridOptions, neural: bool, cmd: &str) -> Result<Arc<Grid>> {
    let sr = s.active_sequence().map(|q| q.settings.sample_rate).unwrap_or(48_000).max(1);
    let m0 = source_in.to_units_floor(sr as i64);
    let len = (source_in + dur).to_units_floor(sr as i64) - m0;
    let key: GridKey = (item.0, m0, len, sr, opts.tempo_hint.map_or(0, f64::to_bits), opts.downbeat, neural);
    if let Some((_, g)) = grid_cache().lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(k, _)| *k == key) {
        return Ok(g.clone());
    }
    if neural {
        // Beat This! (optional model, build feature `ml`): see `ml_tools`
        let g = Arc::new(crate::ml_tools::neural_grid(s, item, source_in, dur, opts, cmd)?);
        let mut c = grid_cache().lock().unwrap_or_else(|e| e.into_inner());
        c.push((key, g.clone()));
        if c.len() > 8 {
            c.remove(0);
        }
        return Ok(g);
    }
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let src = filmcraft_render::SourceProvider::source(&provider, item).ok_or_else(|| bad(cmd, "le média du clip est hors ligne"))?;
    let buf = src.audio(m0, len.max(0) as usize, sr).map_err(|e| bad(cmd, format!("lecture de l'audio du clip impossible : {e}")))?;
    let refs: Vec<&[f32]> = buf.channels.iter().map(Vec::as_slice).collect();
    let g = Arc::new(beatgrid::analyze_grid(&refs, sr, opts).map_err(|_| {
        bad(
            cmd,
            "0 beat détecté dans ce clip : pas de pulsation régulière (musique d'ambiance, voix, bruit, ou clip trop court). Choisissez un clip de musique rythmée, ou donnez le tempo avec `bpm`.",
        )
    })?);
    let mut c = grid_cache().lock().unwrap_or_else(|e| e.into_inner());
    c.push((key, g.clone()));
    if c.len() > 8 {
        c.remove(0);
    }
    Ok(g)
}

fn is_audio_clip(s: &Session, c: ClipId) -> bool {
    s.active_sequence().and_then(|q| q.find_item(c).and_then(|(t, _)| q.track(t))).is_some_and(|t| t.kind == TrackKind::Audio)
}

/// The clip that gives the beats: `beatClip` / `clip`, else the first selected audio clip, else
/// the first audio clip of the sequence.
fn beat_clip(s: &Session, p: &Value, cmd: &str) -> Result<ClipId> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    if let Some(c) = u64_p(p, "beatClip").or_else(|| u64_p(p, "clip")).map(ClipId) {
        if q.find_item(c).is_none() {
            return Err(bad(cmd, "no such clip"));
        }
        if !is_audio_clip(s, c) {
            return Err(bad(cmd, "the beats come from an audio clip"));
        }
        return Ok(c);
    }
    if let Some(c) = s.state.selection.iter().copied().find(|c| is_audio_clip(s, *c)) {
        return Ok(c);
    }
    q.audio_tracks
        .iter()
        .flat_map(|t| t.items.iter())
        .filter(|i| i.enabled)
        .min_by_key(|i| i.start)
        .map(|i| i.id)
        .ok_or_else(|| bad(cmd, "the sequence has no audio clip"))
}

fn count_p(p: &Value, key: &str, default: u64, cmd: &str) -> Result<usize> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default as usize),
        Some(v) => v
            .as_u64()
            .or_else(|| v.as_f64().filter(|f| f.fract() == 0.0 && *f >= 0.0).map(|f| f as u64))
            .map(|n| n as usize)
            .ok_or_else(|| bad(cmd, format!("`{key}` must be a whole number"))),
    }
}

fn find_beats(s: &Session, p: &Value, cmd: &str) -> Result<Beats> {
    let c = beat_clip(s, p, cmd)?;
    let every = count_p(p, "every", 1, cmd)?;
    let offset = count_p(p, "offset", 0, cmd)?;
    if !(1..=64).contains(&every) {
        return Err(bad(cmd, "`every` must be between 1 and 64"));
    }
    let subdivide = count_p(p, "subdivide", 1, cmd)?;
    if !(1..=8).contains(&subdivide) {
        return Err(bad(cmd, "`subdivide` must be between 1 and 8"));
    }
    let snap = bool_p(p, "snap").unwrap_or(true);
    let hint = match p.get("bpm").or_else(|| p.get("tempoHint")) {
        None | Some(Value::Null) => None,
        Some(v) => match v.as_f64() {
            Some(0.0) => None,
            Some(h) if (beatgrid::HINT_MIN..=beatgrid::HINT_MAX).contains(&h) => Some(h),
            _ => return Err(bad(cmd, format!("`bpm` must be 0 (automatic) or between {} and {}", beatgrid::HINT_MIN, beatgrid::HINT_MAX))),
        },
    };
    let downbeat = bool_p(p, "downbeat").unwrap_or(false);
    let neural = match str_p(p, "model") {
        None | Some("") | Some("dsp") => false,
        Some("beat-this") => true,
        Some(other) => return Err(bad(cmd, format!("unknown beat model `{other}` (`dsp`, the default, or `beat-this`)"))),
    };
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, it) = q.find_item(c).ok_or_else(|| bad(cmd, "no such clip"))?;
    if filmcraft_render::remix::is_remixed(it) {
        return Err(bad(cmd, "the clip is a remix clip: revert the remix first"));
    }
    if it.reverse || (it.speed - 1.0).abs() > 1e-9 {
        return Err(bad(cmd, "the clip is retimed (speed or reverse): beats are only read from clips played at 100 %"));
    }
    let sr = q.settings.sample_rate.max(1);
    let rate = q.settings.frame_rate;
    let (start, end, dur) = (it.start, it.end(), it.duration);
    let g = grid(s, it.item, it.source_in, dur, GridOptions { tempo_hint: hint, downbeat }, neural, cmd)?;
    // `subdivide` n puts n - 1 evenly spaced beats between the detected ones, for a tracker that settled on
    // half the real tempo
    let mut line: Vec<i64> = Vec::with_capacity(g.beats.len() * subdivide);
    for (k, b) in g.beats.iter().enumerate() {
        line.push(*b);
        if let Some(next) = g.beats.get(k + 1) {
            line.extend((1..subdivide).map(|j| *b + (*next - *b) * j as i64 / subdivide as i64));
        }
    }
    let bars = g.bar_start.map(|b| (b, g.bar_confidence));
    let skip = bars.map_or(0, |(b, _)| b * subdivide);
    let detected = line.len().saturating_sub(skip);
    let mut at = Vec::new();
    for (i, b) in line.iter().enumerate().skip(skip) {
        let j = i - skip;
        if j < offset || (j - offset) % every != 0 {
            continue;
        }
        let mut t = start + Tick::from_units(*b, sr as i64);
        if snap {
            t = rate.snap_nearest(t);
        }
        if t <= start || t >= end {
            continue;
        }
        if at.last().is_some_and(|(_, last)| *last == t) {
            continue;
        }
        at.push((j / every + 1, t));
    }
    Ok(Beats { clip: c, bpm: g.bpm, at, bars, detected })
}

fn times(b: &Beats) -> Value {
    Value::Array(b.at.iter().take(500).map(|(n, t)| json!({"beat": n, "seconds": t.seconds()})).collect())
}

/// 120 → "120", 127.96 → "128", 93.4 → "93,4".
fn bpm_text(bpm: f64) -> String {
    if (bpm - bpm.round()).abs() < 0.05 { format!("{}", bpm.round()) } else { format!("{bpm:.1}").replace('.', ",") }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// What the analysis found, for a status line: ", mesures dès le beat 3 (sûr à 80 %)".
fn bars_text(b: &Beats) -> String {
    match b.bars {
        Some((start, conf)) if conf >= 0.5 => format!(", mesure dès le beat {}", start + 1),
        Some((start, conf)) => format!(", mesure dès le beat {} (incertain : {:.0} %, à vérifier)", start + 1, conf * 100.0),
        None => String::new(),
    }
}

fn mark_beats(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "sequence.markBeats";
    let found = find_beats(s, p, CMD)?;
    let apply = bool_p(p, "apply").unwrap_or(true);
    let name = str_p(p, "name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Beat").to_string();
    let color = match str_p(p, "color") {
        Some(c) => Label::from_name(c).ok_or_else(|| bad(CMD, format!("unknown colour {c}")))?,
        None => Label::Green,
    };
    if found.at.is_empty() {
        return Err(bad(
            CMD,
            format!(
                "{} détectés à {} BPM mais aucun à marquer : `offset`/`every` ou les bords du clip les écartent (on ne marque pas le tout début ni la toute fin du clip)",
                plural(found.detected, "beat", "beats"),
                bpm_text(found.bpm)
            ),
        ));
    }
    let mut out = json!({
        "clip": found.clip.0, "bpm": found.bpm, "beats": found.at.len(), "detected": found.detected, "markers": 0, "times": times(&found),
        "barStart": found.bars.map(|b| b.0 + 1), "barConfidence": found.bars.map(|b| b.1),
    });
    if apply {
        let at = found.at.clone();
        let added = s.edit_sequence("Mark Beats", move |q, ctx, _| {
            let mut n = 0;
            for (num, t) in at {
                let label = format!("{name} {num}");
                if q.markers.iter().any(|m| m.start == t && m.name == label) {
                    continue;
                }
                let id = MarkerId(ctx.alloc());
                q.markers.push(Marker { id, start: t, duration: Tick::ZERO, name: label, comment: String::new(), kind: MarkerKind::Comment, color });
                n += 1;
            }
            q.markers.sort_by_key(|m| m.start);
            Ok(n)
        })?;
        out["markers"] = json!(added);
        let already = found.at.len() - added.min(found.at.len());
        let mut line =
            format!("{} posé{}, {} BPM{}", plural(added, "marqueur", "marqueurs"), if added == 1 { "" } else { "s" }, bpm_text(found.bpm), bars_text(&found));
        if added == 0 {
            line = format!("0 marqueur posé : les {} étaient déjà là, {} BPM", found.at.len(), bpm_text(found.bpm));
        } else if already > 0 {
            line.push_str(&format!(" ({already} déjà présent{})", if already == 1 { "" } else { "s" }));
        }
        out["summary"] = json!(line);
    } else {
        out["summary"] = json!(format!("{} à marquer, {} BPM (rien posé)", plural(found.at.len(), "beat", "beats"), bpm_text(found.bpm)));
    }
    Ok(out)
}

fn cut_targets(s: &Session, p: &Value, beat: ClipId) -> Result<Vec<ClipId>> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let ids: Vec<ClipId> = match p.get("clips").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|v| v.as_u64().map(ClipId)).collect(),
        None => s.state.selection.iter().copied().filter(|c| *c != beat).collect(),
    };
    let mut ids: Vec<ClipId> = ids.into_iter().filter(|c| q.find_item(*c).is_some()).collect();
    if ids.is_empty() && p.get("clips").is_none() {
        // nothing else selected: the music itself is cut
        ids.push(beat);
    }
    if ids.is_empty() {
        return Err(bad("sequence.cutOnBeats", "select the clips to cut (or pass `clips`)"));
    }
    Ok(ids)
}

fn cut_on_beats(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "sequence.cutOnBeats";
    let found = find_beats(s, p, CMD)?;
    let targets = cut_targets(s, p, found.clip)?;
    let apply = bool_p(p, "apply").unwrap_or(true);
    if found.at.is_empty() {
        return Err(bad(
            CMD,
            format!(
                "{} détectés à {} BPM mais aucune coupe possible : `offset`/`every` ou les bords du clip les écartent",
                plural(found.detected, "beat", "beats"),
                bpm_text(found.bpm)
            ),
        ));
    }
    let n_targets = targets.len();
    let mut out = json!({
        "clip": found.clip.0, "bpm": found.bpm, "beats": found.at.len(), "detected": found.detected, "cuts": 0, "times": times(&found),
        "barStart": found.bars.map(|b| b.0 + 1), "barConfidence": found.bars.map(|b| b.1),
    });
    if apply {
        let at = found.at.clone();
        let cuts = s.edit_sequence("Cut on Beats", move |q, ctx, _| {
            let mut items = targets;
            let mut cuts = 0;
            for (_, t) in at {
                let made = edit::razor_items(q, &items, t, ctx);
                if !made.is_empty() {
                    cuts += 1;
                    items.extend(made);
                }
            }
            Ok(cuts)
        })?;
        out["cuts"] = json!(cuts);
        if cuts == 0 {
            return Err(bad(
                CMD,
                format!(
                    "0 coupe : les {} (sur {} BPM) ne tombent sur aucun des {} sélectionné{}",
                    plural(found.at.len(), "beat", "beats"),
                    bpm_text(found.bpm),
                    plural(n_targets, "clip", "clips"),
                    if n_targets == 1 { "" } else { "s" }
                ),
            ));
        }
        out["summary"] =
            json!(format!("{} sur {}, {} BPM{}", plural(cuts, "coupe", "coupes"), plural(n_targets, "clip", "clips"), bpm_text(found.bpm), bars_text(&found)));
    } else {
        out["summary"] = json!(format!("{} possibles, {} BPM (rien coupé)", plural(found.at.len(), "coupe", "coupes"), bpm_text(found.bpm)));
    }
    Ok(out)
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "sequence.markBeats",
            label: "Mark Beats…",
            menu: &[],
            shortcut: None,
            params: r#"{"clip":id? (audio clip; default: the selected one, else the first of the sequence),"every":n=1,"offset":n=0,"subdivide":n=1,"bpm":n? (30-300, 0 = automatic: the tempo octave to use),"downbeat":bool=false (start at the first beat of a bar),"model":"dsp"|"beat-this"="dsp" (beat-this = neural tracker, needs ml.install),"name":str="Beat","color":label=Green,"snap":bool=true,"apply":bool=true}"#,
            enabled: has_beat_source,
            run: mark_beats,
            journal: true,
        },
        CommandSpec {
            id: "sequence.cutOnBeats",
            label: "Cut on Beats…",
            menu: &[],
            shortcut: None,
            params: r#"{"beatClip":id? (audio clip that gives the beats),"clips":[id]? (default: the selection),"every":n=1,"offset":n=0,"subdivide":n=1,"bpm":n? (30-300, 0 = automatic),"downbeat":bool=false,"model":"dsp"|"beat-this"="dsp","snap":bool=true,"apply":bool=true}"#,
            enabled: has_beat_source,
            run: cut_on_beats,
            journal: true,
        },
    ]
}

fn has_beat_source(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    let q = s.active_sequence().ok_or("no sequence is open")?;
    if q.audio_tracks.iter().any(|t| !t.items.is_empty()) { Ok(()) } else { Err("the sequence has no audio clips".into()) }
}
