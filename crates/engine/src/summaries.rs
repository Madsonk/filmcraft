//! One readable line per command result, for the status bar and the log (French, like the
//! Extensions menu that runs these commands).
//!
//! Commands that know more than their result says (`sequence.markBeats`, `sequence.removeSilence`,
//! `transcript.removeFillers`…) write their own `summary`. For the others, [`add`] builds it from the
//! result when the command is run on its own (not from inside another command): a command result
//! gets a `summary` string, and the line is also written to the log (Info), so what an extension or
//! a script just did is readable without reading JSON.

use serde_json::Value;

fn n(v: &Value, k: &str) -> Option<u64> {
    v.get(k).and_then(Value::as_u64)
}

fn count(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The line for a result of command `id`, if the command has one.
fn text(id: &str, v: &Value) -> Option<String> {
    match id {
        "transcript.generate" | "sequence.transcribe" => {
            let items = v.get("items")?.as_array()?;
            let words: u64 = items.iter().filter_map(|i| n(i, "words")).sum();
            let lang = items.iter().find_map(|i| i.get("language").and_then(Value::as_str)).filter(|l| !l.is_empty());
            Some(if words == 0 {
                "Transcription vide : aucun mot reconnu (audio sans parole, trop bas, ou mauvaise langue)".to_string()
            } else {
                format!(
                    "{} transcrits{}{}",
                    count(words, "mot", "mots"),
                    if items.len() > 1 { format!(" dans {}", count(items.len() as u64, "média", "médias")) } else { String::new() },
                    lang.map(|l| format!(" (langue : {l})")).unwrap_or_default()
                )
            })
        }
        "transcript.createCaptions" => {
            let c = n(v, "captions")?;
            Some(if c == 0 {
                "Aucun sous-titre créé : la transcription est vide".to_string()
            } else {
                format!("{} créé{}", count(c, "sous-titre", "sous-titres"), if c == 1 { "" } else { "s" })
            })
        }
        "captions.export" => {
            let c = n(v, "captions")?;
            Some(format!(
                "{} exporté{} en {} : {}",
                count(c, "sous-titre", "sous-titres"),
                if c == 1 { "" } else { "s" },
                v.get("format").and_then(Value::as_str).unwrap_or("?"),
                v.get("path").and_then(Value::as_str).unwrap_or("?")
            ))
        }
        "export.quick" => Some(format!(
            "Export « {} » lancé : {}",
            v.get("preset").and_then(Value::as_str).unwrap_or("?"),
            v.get("path").and_then(Value::as_str).unwrap_or("?")
        )),
        "edit.pasteAttributes" => {
            let c = n(v, "clips")?;
            Some(if c == 0 {
                "Aucun attribut collé : les clips ciblés étaient déjà identiques à la source".to_string()
            } else {
                format!("Attributs collés sur {}", count(c, "clip", "clips"))
            })
        }
        "edit.copy" => n(v, "copied").map(|c| format!("{} copié{}", count(c, "élément", "éléments"), if c == 1 { "" } else { "s" })),
        "timeline.select" => {
            let c = v.get("selection")?.as_array()?.len() as u64;
            Some(format!("{} sélectionné{}", count(c, "clip", "clips"), if c == 1 { "" } else { "s" }))
        }
        _ => None,
    }
}

/// Commands whose summary is also written to the log (the ones that do or report a piece of work).
pub(crate) fn logged(id: &str) -> bool {
    matches!(
        id,
        "sequence.markBeats"
            | "sequence.cutOnBeats"
            | "sequence.removeSilence"
            | "transcript.removePauses"
            | "transcript.removeFillers"
            | "transcript.createCaptions"
            | "transcript.generate"
            | "sequence.transcribe"
            | "captions.export"
            | "export.quick"
            | "edit.pasteAttributes"
    )
}

/// Give `v` its `summary` (when the command has none and one can be built) and return the line.
pub(crate) fn add(id: &str, v: &mut Value) -> Option<String> {
    if let Some(s) = v.get("summary").and_then(Value::as_str) {
        return Some(s.to_string());
    }
    let line = text(id, v)?;
    if let Value::Object(m) = v {
        m.insert("summary".into(), Value::String(line.clone()));
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lines_are_built_from_results() {
        let mut v = json!({"items": [{"words": 28, "language": "fr"}], "skipped": []});
        assert_eq!(add("sequence.transcribe", &mut v).as_deref(), Some("28 mots transcrits (langue : fr)"));
        assert_eq!(v["summary"], "28 mots transcrits (langue : fr)");
        let mut e = json!({"items": [{"words": 0, "language": "fr"}]});
        assert!(add("transcript.generate", &mut e).unwrap().starts_with("Transcription vide"));
        let mut c = json!({"captions": 1, "path": "/a.srt", "format": "SRT"});
        assert_eq!(add("captions.export", &mut c).as_deref(), Some("1 sous-titre exporté en SRT : /a.srt"));
        let mut own = json!({"summary": "déjà là", "captions": 3});
        assert_eq!(add("transcript.createCaptions", &mut own).as_deref(), Some("déjà là"));
        assert!(add("file.save", &mut json!({})).is_none());
    }
}
