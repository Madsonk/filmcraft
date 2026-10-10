//! Animated word-by-word captions (`transcript.createWordCaptions`): the words of the sequence
//! transcript become graphic clips on a new video track, drawn by the native text layers and
//! animated with their keyframes. No model, no script.
//!
//! * The words are grouped (1-3 words, never across a sentence end, a pause, a speaker change or
//!   past `maxChars`); each group is one graphic clip lasting from its first word to its last
//!   (plus a short hold), so the clips of the track never overlap.
//! * Styles:
//!   - `karaoke`: the group is on screen and the word being spoken takes the accent colour. The
//!     colour is a second text layer with the same text whose characters, except the active word's,
//!     have a transparent fill (so both layers lay out identically with no text measuring), and
//!     whose fill alpha is keyframed with *hold* interpolation.
//!   - `pop`: one word at a time (a group is one word by default), springing in from 70 % to 112 %
//!     to 100 % in the accent colour.
//!
//!   The group itself pops in (scale and opacity keyframes) in all styles.
//! * Everything is ordinary project data: the layers, keyframes and clips can be edited like any
//!   graphic, and the whole command is one undo step.

use filmcraft_edit::transcript::SeqWord;
use filmcraft_geom::Vec2;
use filmcraft_project::graphic::{TEXT_LAYER, new_text_layer};
use filmcraft_project::{CharStyle, EffectInstance, Interpolation, ItemId, Keyframe, LayerExtra, Param, ParamValue, StyleRun, Track, TrackId, TrackKind};
use filmcraft_time::{FrameRate, Tick, TimeRange};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, bool_p, f64_p, str_p, u64_p};
use crate::{EngineError, Result, Session};

pub fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "transcript.createWordCaptions",
        label: "Create Animated Word Captions…",
        menu: &["Sequence", "Transcript"],
        shortcut: None,
        params: r##"{"style":"karaoke|pop"="karaoke","wordsPerGroup":n? (1-6; default 3, pop: 1),"maxChars":n=18,"color":"#rrggbb"="#ffffff","accent":"#rrggbb"="#ffd400","outline":"#rrggbb"="#000000","font":str="Inter","fontStyle":str="Bold","size":px?,"y":0..1=0.74 (baseline height as a share of the frame),"caps":bool=true,"pauseSeconds":f=0.6,"holdSeconds":f=0.12,"name":str="Sous-titres animés"}"##,
        enabled: crate::transcript::has_transcript,
        run: create,
        journal: true,
    }]
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Style {
    Karaoke,
    Pop,
}

/// `#rrggbb[aa]` → straight sRGB RGBA.
pub(crate) fn parse_hex(s: &str) -> Option<[f32; 4]> {
    let h = s.trim().trim_start_matches('#');
    if (h.len() != 6 && h.len() != 8) || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok().map(|x| f32::from(x) / 255.0);
    Some([v(0)?, v(2)?, v(4)?, if h.len() == 8 { v(6)? } else { 1.0 }])
}

/// Split the words into groups of at most `per` words that fit `max_chars`, cut at sentence ends,
/// pauses and speaker changes.
fn groups(words: &[SeqWord], per: usize, max_chars: usize, pause: Tick) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut a = 0;
    let mut chars = 0;
    for i in 0..words.len() {
        let len = words[i].text.chars().count();
        if i > a {
            let prev = &words[i - 1];
            let brk = i - a >= per
                || chars + 1 + len > max_chars
                || prev.text.ends_with(['.', '?', '!', '…'])
                || words[i].start - prev.end >= pause
                || words[i].speaker != prev.speaker;
            if brk {
                out.push(a..i);
                a = i;
                chars = len;
                continue;
            }
            chars += 1 + len;
        } else {
            chars = len;
        }
    }
    if a < words.len() {
        out.push(a..words.len());
    }
    out
}

struct Look {
    font: String,
    font_style: String,
    size: f64,
    color: [f32; 4],
    accent: [f32; 4],
    outline: [f32; 4],
    caps: bool,
    pos: Vec2,
}

fn put(e: &mut EffectInstance, id: &str, v: ParamValue) {
    e.params.insert(id.to_string(), Param::new(v));
}

/// A text layer showing `text` with the look; `fill` is the fill colour, `outlined` draws the stroke
/// and shadow (the base layer) or not (the overlays).
fn layer(text: &str, look: &Look, fill: [f32; 4], outlined: bool, name: &str) -> EffectInstance {
    let mut e = new_text_layer(text, look.pos, look.size);
    debug_assert_eq!(e.effect, TEXT_LAYER);
    put(&mut e, "name", ParamValue::Text(name.into()));
    put(&mut e, "font", ParamValue::Text(look.font.clone()));
    put(&mut e, "font_style", ParamValue::Text(look.font_style.clone()));
    put(&mut e, "align", ParamValue::Choice(1));
    put(&mut e, "caps", ParamValue::Choice(u32::from(look.caps)));
    put(&mut e, "fill_color", ParamValue::Color(fill));
    if outlined {
        put(&mut e, "stroke", ParamValue::Bool(true));
        put(&mut e, "stroke_color", ParamValue::Color(look.outline));
        put(&mut e, "stroke_width", ParamValue::Float((look.size * 0.09).round()));
        put(&mut e, "stroke_type", ParamValue::Choice(0));
        put(&mut e, "shadow", ParamValue::Bool(true));
        put(&mut e, "shadow_color", ParamValue::Color([0.0, 0.0, 0.0, 1.0]));
        put(&mut e, "shadow_opacity", ParamValue::Float(55.0));
        put(&mut e, "shadow_distance", ParamValue::Float((look.size * 0.06).round()));
        put(&mut e, "shadow_blur", ParamValue::Float((look.size * 0.18).round()));
    }
    e
}

fn kf(t: Tick, v: ParamValue, interp: Interpolation) -> Keyframe {
    let mut k = Keyframe::new(t, v);
    k.interp = interp;
    k
}

fn animate(e: &mut EffectInstance, id: &str, keys: Vec<Keyframe>) {
    let first = keys.first().map(|k| k.value.clone());
    if let Some(v) = first {
        e.params.insert(id.to_string(), Param { value: v, keyframes: keys });
    }
}

/// Characters (not bytes) of word `k` of `words` within their space-joined text.
fn word_span(words: &[&str], k: usize) -> (usize, usize) {
    let start: usize = words[..k].iter().map(|w| w.chars().count() + 1).sum();
    (start, start + words[k].chars().count())
}

/// Per-character runs that make everything but word `k` transparent (`only`), or only word `k`'s
/// colour (`!only`).
fn visibility_runs(words: &[&str], k: usize) -> Vec<StyleRun> {
    let total: usize = words.iter().map(|w| w.chars().count()).sum::<usize>() + words.len().saturating_sub(1);
    let (a, b) = word_span(words, k);
    let hide = CharStyle { fill: Some([1.0, 1.0, 1.0, 0.0]), ..Default::default() };
    let mut runs = Vec::new();
    if a > 0 {
        runs.push(StyleRun { start: 0, end: a, style: hide.clone() });
    }
    if b < total {
        runs.push(StyleRun { start: b, end: total, style: hide });
    }
    runs
}

fn with_runs(mut e: EffectInstance, runs: Vec<StyleRun>) -> EffectInstance {
    e.layer = Some(Box::new(LayerExtra { uid: 0, runs, pin: None }));
    e
}

fn alpha(c: [f32; 4], a: f32) -> ParamValue {
    ParamValue::Color([c[0], c[1], c[2], c[3] * a])
}

/// The layers of one group clip. `times[k]` = (start, end) of word k relative to the clip start,
/// `dur` = clip length.
fn build_layers(style: Style, ws: &[&str], times: &[(Tick, Tick)], dur: Tick, fd: Tick, look: &Look) -> Vec<EffectInstance> {
    let text = ws.join(" ");
    let n = ws.len();
    let mut layers: Vec<EffectInstance> = Vec::new();
    let hold = Interpolation::Hold;
    match style {
        Style::Karaoke => {
            layers.push(layer(&text, look, look.color, true, "Texte"));
            for k in 0..n {
                let mut e = with_runs(layer(&text, look, look.accent, false, &format!("Mot {}", k + 1)), visibility_runs(ws, k));
                let start = times[k].0;
                let end = if k + 1 < n { times[k + 1].0 } else { dur };
                let mut keys = Vec::new();
                if start > Tick::ZERO {
                    keys.push(kf(Tick::ZERO, alpha(look.accent, 0.0), hold));
                }
                keys.push(kf(start, alpha(look.accent, 1.0), hold));
                if end < dur {
                    keys.push(kf(end, alpha(look.accent, 0.0), hold));
                }
                animate(&mut e, "fill_color", keys);
                layers.push(e);
            }
        }
        Style::Pop => {
            // one word (or a short group) in the accent colour, no overlay
            layers.push(layer(&text, look, look.accent, true, "Texte"));
        }
    }
    // the pop-in: scale 70 → 112 → 100 % and a quick fade, on every layer of the clip
    let (t1, t2, t3) = (Tick(fd.0 * 3), Tick(fd.0 * 6), Tick(fd.0 * 2));
    let (t1, t2) = (t1.min(dur), t2.min(dur));
    for e in &mut layers {
        if style == Style::Pop {
            animate(
                e,
                "scale",
                vec![
                    kf(Tick::ZERO, ParamValue::Float(70.0), Interpolation::EaseOut),
                    kf(t1, ParamValue::Float(112.0), Interpolation::EaseOut),
                    kf(t2.max(Tick(t1.0 + 1)), ParamValue::Float(100.0), Interpolation::Linear),
                ],
            );
        } else {
            animate(
                e,
                "scale",
                vec![
                    kf(Tick::ZERO, ParamValue::Float(80.0), Interpolation::EaseOut),
                    kf(t1, ParamValue::Float(106.0), Interpolation::EaseOut),
                    kf(t2.max(Tick(t1.0 + 1)), ParamValue::Float(100.0), Interpolation::Linear),
                ],
            );
        }
        animate(
            e,
            "opacity",
            vec![kf(Tick::ZERO, ParamValue::Float(0.0), Interpolation::Linear), kf(t3.min(dur).max(Tick(1)), ParamValue::Float(100.0), Interpolation::Linear)],
        );
    }
    layers
}

fn snap(rate: FrameRate, t: Tick) -> Tick {
    rate.snap_nearest(t)
}

fn create(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "transcript.createWordCaptions";
    let words = crate::transcript::sequence_words(s);
    if words.is_empty() {
        return Err(bad(CMD, "the sequence has no transcript (Transcribe first)"));
    }
    let style = match str_p(p, "style").unwrap_or("karaoke").to_ascii_lowercase().as_str() {
        "karaoke" => Style::Karaoke,
        "pop" => Style::Pop,
        other => return Err(bad(CMD, format!("unknown style `{other}` (karaoke, pop)"))),
    };
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let (w, h, rate) = {
        let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
        (q.settings.width, q.settings.height, q.settings.frame_rate)
    };
    let per = u64_p(p, "wordsPerGroup").map(|n| n.clamp(1, 6) as usize).unwrap_or(if style == Style::Pop { 1 } else { 3 });
    let max_chars = u64_p(p, "maxChars").map(|n| n.clamp(6, 60) as usize).unwrap_or(18);
    let color = |k: &str, d: &str| -> Result<[f32; 4]> {
        let v = str_p(p, k).unwrap_or(d);
        parse_hex(v).ok_or_else(|| bad(CMD, format!("`{k}` is not a #rrggbb colour: {v}")))
    };
    let size = f64_p(p, "size").unwrap_or_else(|| (0.095 * f64::from(h)).min(0.9 * f64::from(w) / (0.58 * max_chars as f64))).clamp(8.0, 600.0);
    let y = f64_p(p, "y").unwrap_or(0.74).clamp(0.05, 0.98);
    let look = Look {
        font: str_p(p, "font").unwrap_or("Inter").to_string(),
        font_style: str_p(p, "fontStyle").unwrap_or("Bold").to_string(),
        size,
        color: color("color", "#ffffff")?,
        accent: color("accent", "#ffd400")?,
        outline: color("outline", "#000000")?,
        caps: bool_p(p, "caps").unwrap_or(true),
        pos: Vec2::new(f64::from(w) / 2.0, f64::from(h) * y),
    };
    let pause = Tick::from_seconds_f64(f64_p(p, "pauseSeconds").unwrap_or(0.6));
    let hold = Tick::from_seconds_f64(f64_p(p, "holdSeconds").unwrap_or(0.12));
    let name = str_p(p, "name").unwrap_or("Sous-titres animés").to_string();
    let gs = groups(&words, per, max_chars, pause);
    let fd = rate.frame_duration();

    // frame-snapped, non-overlapping clip ranges
    let mut plan: Vec<(std::ops::Range<usize>, Tick, Tick)> = Vec::new();
    for (gi, g) in gs.iter().enumerate() {
        let mut start = snap(rate, words[g.start].start);
        if let Some((_, _, prev_end)) = plan.last() {
            start = start.max(*prev_end);
        }
        let mut end = snap(rate, words[g.end - 1].end + hold);
        if let Some(next) = gs.get(gi + 1) {
            end = end.min(snap(rate, words[next.start].start)).max(start);
        }
        end = end.max(start + fd);
        if end - start < fd {
            continue;
        }
        plan.push((g.clone(), start, end));
    }
    if plan.is_empty() {
        return Err(bad(CMD, "no word is long enough to caption"));
    }
    let n_clips = plan.len();
    let n_words = words.len();
    let track = s.edit("Create Animated Word Captions", move |pr, _st| {
        let src: ItemId = crate::graphics::graphic_source(pr, w, h, rate);
        let tid = TrackId(pr.alloc_id());
        let mut items = Vec::with_capacity(plan.len());
        for (g, start, end) in &plan {
            let dur = *end - *start;
            let ws: Vec<&str> = words[g.clone()].iter().map(|x| x.text.as_str()).collect();
            let times: Vec<(Tick, Tick)> =
                words[g.clone()].iter().map(|x| (snap(rate, x.start).max(*start) - *start, (snap(rate, x.end).max(*start) - *start).min(dur))).collect();
            let mut ti = pr
                .make_track_item(src, TrackKind::Video, *start, TimeRange::new(Tick::ZERO, dur), rate)
                .ok_or_else(|| bad(CMD, "cannot make a graphic clip"))?;
            ti.name = ws.join(" ");
            ti.effects.extend(build_layers(style, &ws, &times, dur, fd, &look));
            for e in &mut ti.effects {
                filmcraft_project::resolve_auto_points(e, (w, h), (w, h));
            }
            items.push(ti);
        }
        let q = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
        let mut t = Track::new(tid, TrackKind::Video, name);
        t.items = items;
        t.sort();
        q.video_tracks.push(t);
        q.check().map_err(EngineError::Other)?;
        Ok(tid)
    })?;
    let label = match style {
        Style::Karaoke => "karaoké",
        Style::Pop => "pop",
    };
    Ok(json!({
        "track": track.0,
        "clips": n_clips,
        "words": n_words,
        "style": label,
        "summary": format!("{n_clips} sous-titres animés posés sur une nouvelle piste vidéo (style {label}, {} mots)", n_words),
    }))
}

#[cfg(test)]
#[path = "word_captions_tests.rs"]
mod tests;
