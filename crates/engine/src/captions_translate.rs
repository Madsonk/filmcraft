//! Offline subtitle translation (`captions.translate`): a caption track becomes a second track in
//! another language, same timings, translated with an Opus-MT model (French ↔ English) running on
//! the CPU (`filmcraft_speech::mt`). The model is a download the user confirms
//! (`transcript.downloadModel {model:"opus-mt-tc-big-fr-en"}`, size, source and licence in
//! `transcript.models`); nothing is sent anywhere when translating.
//!
//! The translation is done caption by caption (so timings are untouched); a caption of two lines is
//! translated as one sentence and wrapped again in two balanced lines. The command runs on the
//! calling thread: allow about a second per caption with the default 4 beams.

use serde_json::{Value, json};

use filmcraft_project::{ClipId, TrackId, plain_text};

use crate::commands::{CommandSpec, bad, str_p};
use crate::{EngineError, Result, Session};

pub fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "captions.translate",
        label: "Translate Captions…",
        menu: &[],
        shortcut: None,
        params: r#"{"track":id|"C1"?,"to":"en|fr","from":"fr|en"? (default: the track's language, else the transcript's),"name":str? (default "<track> (EN)"),"model":str? (default opus-mt-tc-big-<from>-<to>),"beams":1-8=4,"maxChars":n=42 (line length when wrapping),"apply":bool=true (false: only reports what would be translated)}"#,
        enabled: has_captions,
        run: translate,
        journal: true,
    }]
}

fn has_captions(s: &Session) -> std::result::Result<(), String> {
    crate::commands::has_seq(s)?;
    let any = s.active_sequence().is_some_and(|q| q.caption_tracks.iter().any(|t| !t.captions.is_empty()));
    if any { Ok(()) } else { Err("the sequence has no captions (create them from the transcript first)".into()) }
}

#[cfg(feature = "whisper")]
/// Split `text` into one line, or two balanced lines when it is longer than `max`.
pub(crate) fn wrap(text: &str, max: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let n = text.chars().count();
    if n <= max {
        return text;
    }
    let chars: Vec<char> = text.chars().collect();
    let mid = n / 2;
    let split = (0..n).filter(|&i| chars[i] == ' ').min_by_key(|&i| i.abs_diff(mid));
    match split {
        Some(i) => format!("{}\n{}", chars[..i].iter().collect::<String>(), chars[i + 1..].iter().collect::<String>()),
        None => text,
    }
}

/// Language code of a BCP-47 tag (`fr-CA` → `fr`).
fn lang_of(tag: &str) -> String {
    tag.split(['-', '_']).next().unwrap_or("").to_lowercase()
}

#[cfg(feature = "whisper")]
fn translator(model: &'static filmcraft_speech::models::ModelInfo, dir: &std::path::Path) -> Result<std::sync::Arc<filmcraft_speech::mt::Translator>> {
    use std::sync::{Arc, Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<Option<(String, Arc<filmcraft_speech::mt::Translator>)>>> = OnceLock::new();
    let cell = CACHE.get_or_init(|| Mutex::new(None));
    let mut g = cell.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((id, t)) = g.as_ref()
        && id == model.id
    {
        return Ok(t.clone());
    }
    let t = Arc::new(filmcraft_speech::mt::Translator::load(&filmcraft_speech::models::model_dir(dir, model)).map_err(|e| EngineError::Other(e.to_string()))?);
    *g = Some((model.id.to_string(), t.clone()));
    Ok(t)
}

fn translate(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "captions.translate";
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let tid: TrackId = if p.get("track").is_some() {
        crate::captions::track_param(s, p).ok_or_else(|| bad(CMD, "no such caption track"))?
    } else {
        q.caption_tracks.iter().find(|t| !t.captions.is_empty()).map(|t| t.id).ok_or_else(|| bad(CMD, "no caption track with captions"))?
    };
    let src = q.caption_tracks.iter().find(|t| t.id == tid).ok_or_else(|| bad(CMD, "no such caption track"))?.clone();
    let to = str_p(p, "to").map(lang_of).ok_or_else(|| bad(CMD, "`to` (en or fr) is required"))?;
    let from = str_p(p, "from")
        .filter(|f| *f != "auto")
        .map(lang_of)
        .or_else(|| Some(lang_of(&src.language)).filter(|l| !l.is_empty() && *l != to))
        .or_else(|| s.project.transcripts.values().map(|t| lang_of(&t.language)).find(|l| !l.is_empty()))
        .unwrap_or_else(|| if to == "en" { "fr".into() } else { "en".into() });
    if from == to {
        return Err(bad(CMD, format!("the captions are already in `{to}` (pass `from` to say otherwise)")));
    }
    let model = match str_p(p, "model") {
        Some(id) => filmcraft_speech::models::find_mt(id).ok_or_else(|| bad(CMD, format!("unknown translation model `{id}`")))?,
        None => filmcraft_speech::models::mt_for(&from, &to)
            .ok_or_else(|| bad(CMD, format!("no translation model for {from} → {to} (available: fr → en, en → fr)")))?,
    };
    let todo: Vec<(ClipId, String)> = src.captions.iter().map(|c| (c.id, plain_text(&c.text).replace('\n', " "))).collect();
    let apply = p.get("apply").and_then(Value::as_bool).unwrap_or(true);
    if !apply {
        return Ok(
            json!({"model": model.id, "from": from, "to": to, "captions": todo.len(), "summary": format!("{} sous-titres à traduire ({from} → {to}) avec {}", todo.len(), model.id)}),
        );
    }
    #[cfg(not(feature = "whisper"))]
    {
        let _ = (model, todo);
        Err(EngineError::Other("offline translation is not available in this build (built without the `whisper` feature)".into()))
    }
    #[cfg(feature = "whisper")]
    {
        use crate::commands::u64_p;
        use filmcraft_project::{Caption, CaptionTrack};
        let dir = crate::transcript::models_dir().ok_or_else(|| EngineError::Other("no data directory for models".into()))?;
        if !filmcraft_speech::models::installed(&dir, model) {
            return Err(EngineError::Other(format!(
                "the translation model `{}` is not downloaded ({} MB, {}): run transcript.downloadModel {{\"model\":\"{}\"}} first",
                model.id,
                model.size() / 1_000_000,
                model.license,
                model.id
            )));
        }
        let tr = translator(model, &dir)?;
        let beams = u64_p(p, "beams").map(|n| n.clamp(1, 8) as usize).unwrap_or(4);
        let max_chars = u64_p(p, "maxChars").map(|n| n.clamp(10, 100) as usize).unwrap_or(42);
        let clock = web_time::Instant::now();
        let mut out: Vec<(ClipId, String)> = Vec::with_capacity(todo.len());
        for (id, text) in &todo {
            let t = tr.translate(text, beams).map_err(|e| EngineError::Other(format!("translation failed: {e}")))?;
            out.push((*id, wrap(&t, max_chars)));
        }
        let secs = clock.elapsed().as_secs_f64();
        let name = str_p(p, "name").map(str::to_string).unwrap_or_else(|| format!("{} ({})", src.name, to.to_uppercase()));
        let n = out.len();
        let new_track = s.edit_sequence("Translate Captions", |q, ctx, st| {
            let id = TrackId(ctx.alloc());
            let mut t = CaptionTrack::new(id, name, src.format);
            t.language = to.clone();
            t.style = src.style.clone();
            for (c, (_, text)) in src.captions.iter().zip(&out) {
                t.captions.push(Caption {
                    id: ClipId(ctx.alloc()),
                    start: c.start,
                    duration: c.duration,
                    text: text.clone(),
                    speaker: c.speaker.clone(),
                    cue_id: None,
                    settings: c.settings.clone(),
                });
            }
            let at = q.caption_tracks.iter().position(|x| x.id == src.id).map_or(0, |i| i + 1);
            q.caption_tracks.insert(at, t);
            st.caption_selection.clear();
            Ok(id)
        })?;
        Ok(json!({
            "track": new_track.0, "captions": n, "from": from, "to": to, "model": model.id, "seconds": secs,
            "summary": format!("{n} sous-titres traduits ({from} → {to}) en {secs:.0} s, nouvelle piste de sous-titres"),
            "preview": todo.iter().zip(&out).take(3).map(|((_, a), (_, b))| json!({"from": a, "to": b})).collect::<Vec<_>>(),
        }))
    }
}

#[cfg(all(test, feature = "whisper"))]
mod tests {
    use super::*;

    #[test]
    fn wrap_balances_two_lines() {
        assert_eq!(wrap("Short one", 42), "Short one");
        let w = wrap("This is a rather long sentence that has to be split in two lines", 42);
        let lines: Vec<&str> = w.lines().collect();
        assert_eq!(lines.len(), 2, "{w}");
        assert!(lines.iter().all(|l| l.chars().count() <= 42), "{w}");
        assert!((lines[0].len() as i64 - lines[1].len() as i64).abs() < 12, "{w}");
        assert_eq!(wrap("  a   b ", 42), "a b");
    }

    #[test]
    fn language_tags() {
        assert_eq!(lang_of("fr-CA"), "fr");
        assert_eq!(lang_of("EN"), "en");
    }
}
