//! Dubbing: `sequence.placeDub` (Extensions ▸ Doublage vocal).
//!
//! The extension program renders the dubbed voice off line (one audio file aligned on the
//! sequence, silence between the phrases) and plans the ducking of the original voice; this command
//! puts the result in the sequence as **one undo step**:
//!
//! 1. imports `path` into the project,
//! 2. adds an audio track (or uses `track`) and names it,
//! 3. places the file on it at `time` / `seconds` (default: the playhead),
//! 4. writes the original track's volume automation (`duck`): the voice is lowered under the dub and
//!    comes back between the phrases, or is silenced.
//!
//! A command run from an `exec` extension cannot read the ids the earlier commands returned (the
//! program has exited by then), so the import, the track and the clip have to be made by one command.
//! If any step fails, everything done so far is undone and nothing is left behind.

use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, has_seq, str_p};
use crate::{Result, Session};

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "sequence.placeDub",
        label: "Place Dubbed Audio",
        menu: &[],
        shortcut: None,
        params: r#"{"path":str (audio file),"time":ticks|"seconds":f64 (default: playhead),"name":str="Doublage","track":"A3"|id? (default: a new audio track),"duck":{"strip":"A1"|id,"points":[[ticks,dB]]}? (volume automation of the original track),"note":str? (the status line)}"#,
        enabled: has_seq,
        run: place_dub,
        journal: true,
    }]
}

fn place_dub(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "sequence.placeDub";
    let path = str_p(p, "path").ok_or_else(|| bad(CMD, "need `path` (the dubbed audio file)"))?.to_string();
    if !std::path::Path::new(&path).is_file() {
        return Err(bad(CMD, format!("le fichier de doublage est introuvable : {path}")));
    }
    let undo_before = s.history.undo.len();
    match place_steps(s, p, &path) {
        Ok(mut v) => {
            // everything above is one undo step
            if s.history.undo.len() > undo_before + 1 {
                let (_, base, extra) = s.history.undo[undo_before].clone();
                s.history.undo.truncate(undo_before);
                s.history.undo.push(("Place Dubbed Audio".to_string(), base, extra));
                s.history.merge_key = None;
            }
            if let Some(n) = str_p(p, "note").filter(|n| !n.trim().is_empty()) {
                v["summary"] = json!(n);
            }
            Ok(v)
        }
        Err(e) => {
            while s.history.undo.len() > undo_before {
                s.undo();
            }
            s.history.redo.clear();
            Err(e)
        }
    }
}

fn place_steps(s: &mut Session, p: &Value, path: &str) -> Result<Value> {
    const CMD: &str = "sequence.placeDub";
    let imported = s.execute("file.import", json!({"paths": [path]}))?;
    let item = imported
        .get("items")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_u64)
        .ok_or_else(|| bad(CMD, format!("FilmCraft n'a pas pu importer {path} : {}", imported.get("errors").map(Value::to_string).unwrap_or_default())))?;

    let track: Value = match p.get("track") {
        Some(t) if !t.is_null() => t.clone(),
        _ => {
            let r = s.execute("sequence.addTracks", json!({"video": 0, "audio": 1}))?;
            r.get("audio").and_then(Value::as_array).and_then(|a| a.first()).cloned().ok_or_else(|| bad(CMD, "la piste audio n'a pas pu être ajoutée"))?
        }
    };
    let name = str_p(p, "name").unwrap_or("Doublage").to_string();
    s.execute("timeline.setTrack", json!({"track": track, "name": name}))?;

    let mut place = json!({"item": item, "audioTrack": track});
    for k in ["time", "seconds", "frame"] {
        if let Some(v) = p.get(k) {
            place[k] = v.clone();
        }
    }
    let placed = s.execute("timeline.place", place)?;
    let clips = placed.get("clips").cloned().unwrap_or(json!([]));

    let mut keyframes = 0u64;
    if let Some(d) = p.get("duck").filter(|d| d.get("points").and_then(Value::as_array).is_some_and(|a| !a.is_empty())) {
        let strip = d.get("strip").cloned().ok_or_else(|| bad(CMD, "`duck` needs `strip` (the track of the original voice)"))?;
        let r = s.execute("mixer.writeAutomation", json!({"strip": strip, "lane": "volume", "points": d["points"]}))?;
        keyframes = r.get("keyframes").and_then(Value::as_u64).unwrap_or(0);
    }
    Ok(json!({
        "item": item,
        "track": track,
        "clips": clips,
        "duckKeyframes": keyframes,
        "summary": format!("Doublage posé sur la piste « {name} »{}", if keyframes > 0 { format!(", voix d'origine atténuée ({keyframes} points)") } else { String::new() }),
    }))
}
