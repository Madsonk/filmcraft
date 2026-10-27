//! Voice / music separation with HT Demucs (build feature `ml`, model `htdemucs`).
//!
//! | command | what it does |
//! |---|---|
//! | `audio.separate` | splits the audio of a clip (or media item) into stems and imports each as a new 32-bit float WAV: `vocals`, `drums`, `bass`, `other`, and `rest` (everything except the voice: the music without the voice) |
//!
//! Parameters: `clip` (timeline clip: the range it uses) or `item`; `stems` (default `["vocals","rest"]`);
//! `out` (a folder; default `<project folder>/ML`); `wait`. The network is the released HT Demucs; its output
//! was compared with the reference PyTorch implementation (see `docs/extensions.md`).

use serde_json::Value;

use crate::commands::{CommandSpec, bad};
use crate::{EngineError, Result, Session};

#[cfg(not(feature = "ml"))]
fn no_ml(cmd: &str) -> EngineError {
    bad(cmd, "this build has no neural tools (build with --features ml)")
}

#[cfg(feature = "ml")]
mod on {
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::Ordering;

    use filmcraft_export::Report;
    use filmcraft_ml::catalogue;
    use filmcraft_project::ItemId;
    use filmcraft_time::{Tick, TimeRange};
    use serde_json::{Value, json};

    use super::*;
    use crate::commands::{bool_p, clip_p, str_p, u64_p};
    use crate::ml_video::on::{spawn, unique_dir_file};

    /// 32-bit float stereo WAV.
    fn write_wav(path: &Path, ch: &[Vec<f32>; 2], rate: u32) -> std::io::Result<()> {
        let n = ch[0].len();
        let data = (n * 8) as u32;
        let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
        f.write_all(b"RIFF")?;
        f.write_all(&(36 + data).to_le_bytes())?;
        f.write_all(b"WAVEfmt ")?;
        f.write_all(&16u32.to_le_bytes())?;
        f.write_all(&3u16.to_le_bytes())?;
        f.write_all(&2u16.to_le_bytes())?;
        f.write_all(&rate.to_le_bytes())?;
        f.write_all(&(rate * 8).to_le_bytes())?;
        f.write_all(&8u16.to_le_bytes())?;
        f.write_all(&32u16.to_le_bytes())?;
        f.write_all(b"data")?;
        f.write_all(&data.to_le_bytes())?;
        for i in 0..n {
            f.write_all(&ch[0][i].to_le_bytes())?;
            f.write_all(&ch[1][i].to_le_bytes())?;
        }
        f.flush()
    }

    pub fn separate(s: &mut Session, p: &Value) -> Result<Value> {
        const CMD: &str = "audio.separate";
        let (item, range) = if let Some(c) = clip_p(p, "clip").or_else(|| s.state.selection.first().copied().filter(|_| p.get("item").is_none())) {
            let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
            let (_, it) = q.find_item(c).ok_or_else(|| bad(CMD, "no such clip"))?;
            if (it.speed - 1.0).abs() > 1e-9 || it.reverse {
                return Err(bad(CMD, "the clip is retimed (speed or reverse): process the media item instead (`item`)"));
            }
            (it.item, Some(TimeRange { start: it.source_in, duration: it.duration }))
        } else if let Some(i) = u64_p(p, "item") {
            (ItemId(i), None)
        } else {
            return Err(bad(CMD, "select a clip, or pass `clip` or `item`"));
        };
        let (mid, m, sub) = s.project.resolve_media(item).ok_or_else(|| bad(CMD, "this item is not file media"))?;
        if !m.info.has_audio() {
            return Err(bad(CMD, "this media has no audio"));
        }
        let name = s.project.item(item).map(|i| i.name.clone()).unwrap_or_else(|| "clip".into());
        let range = range.or(sub).unwrap_or(TimeRange { start: Tick::ZERO, duration: m.info.duration });
        if s.media.offline_status(mid).is_some() {
            return Err(bad(CMD, format!("{name} is offline; link it first")));
        }
        let src = s.media.full_res_source(&s.project, mid, &*s.services).ok_or_else(|| bad(CMD, format!("{name}: no source")))?;
        let dir = crate::ml_tools::require(&catalogue::HTDEMUCS, CMD)?;
        let stems: Vec<String> = match p.get("stems") {
            Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
            Some(Value::String(v)) => v.split(',').map(|x| x.trim().to_string()).collect(),
            _ => vec!["vocals".into(), "rest".into()],
        };
        if stems.is_empty() || stems.iter().any(|x| !["vocals", "drums", "bass", "other", "rest"].contains(&x.as_str())) {
            return Err(bad(CMD, "`stems` is a list of vocals, drums, bass, other, rest"));
        }
        let folder = match str_p(p, "out") {
            Some(o) => PathBuf::from(o),
            None => s
                .path
                .as_deref()
                .and_then(|f| Path::new(f).parent())
                .map(|d| d.join("ML"))
                .or_else(|| crate::ml_tools::models_dir().and_then(|d| d.parent().map(|x| x.join("out"))))
                .unwrap_or_else(std::env::temp_dir),
        };
        let stem_name: String = name
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { ' ' })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let label = |x: &str| match x {
            "vocals" => "voix",
            "rest" => "sans voix",
            "drums" => "batterie",
            "bass" => "basse",
            _ => "autre",
        };
        let outs: Vec<PathBuf> = stems.iter().map(|x| unique_dir_file(&folder, &stem_name, &format!(" {}", label(x)), "wav")).collect();
        let wait = bool_p(p, "wait").unwrap_or(false);
        let secs = range.duration.seconds();
        let (start, frames) = (range.start.to_units_floor(44_100), (secs * 44_100.0).round() as usize);
        let stems2 = stems.clone();
        let mut res = spawn(s, format!("Separate {name}"), (secs / 5.85).ceil().max(1.0) as u64, mid, outs, wait, move |prog, o| {
            let buf = src.audio(start, frames, 44_100).map_err(|e| format!("audio read: {e}"))?;
            let n = buf.channels.iter().map(Vec::len).min().unwrap_or(0);
            if n < 4410 {
                return Err("the audio is shorter than 0.1 s".into());
            }
            let left = buf.channels[0][..n].to_vec();
            let right = buf.channels.get(1).map_or_else(|| left.clone(), |c| c[..n].to_vec());
            let model = filmcraft_ml::htdemucs::Htdemucs::load(&dir).map_err(|e| e.to_string())?;
            let out = model
                .separate(&[left, right], &mut |d, t| {
                    prog.total.store(t as u64, Ordering::Relaxed);
                    prog.done.store(d as u64, Ordering::Relaxed);
                    !prog.cancel.load(Ordering::Relaxed)
                })
                .map_err(|e| e.to_string())?;
            let names = filmcraft_ml::htdemucs::SOURCES;
            for (k, st) in stems2.iter().enumerate() {
                let wav: [Vec<f32>; 2] = if st == "rest" {
                    // everything except the voice
                    let mut r = [vec![0f32; n], vec![0f32; n]];
                    for (si, nm) in names.iter().enumerate() {
                        if *nm != "vocals" {
                            for c in 0..2 {
                                for i in 0..n {
                                    r[c][i] += out[si][c][i];
                                }
                            }
                        }
                    }
                    r
                } else {
                    let si = names.iter().position(|x| x == st).ok_or("unknown stem")?;
                    [out[si][0].clone(), out[si][1].clone()]
                };
                write_wav(&o[k], &wav, 44_100).map_err(|e| format!("{}: {e}", o[k].display()))?;
            }
            Ok(Report { path: String::new(), frames: n as u64, seconds: 0.0, bytes: 0, render_fps: 0.0, extra_files: Vec::new() })
        })?;
        res["stems"] = json!(stems);
        res["summary"] = json!(format!("{} séparé en {} (HT Demucs)", name, stems.iter().map(|x| label(x)).collect::<Vec<_>>().join(" + ")));
        Ok(res)
    }
}

fn separate(s: &mut Session, p: &Value) -> Result<Value> {
    #[cfg(feature = "ml")]
    return on::separate(s, p);
    #[cfg(not(feature = "ml"))]
    {
        let _ = (s, p);
        Err(no_ml("audio.separate"))
    }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "audio.separate",
        label: "Separate Voice and Music…",
        menu: &[],
        shortcut: None,
        params: r#"{"clip":id? (timeline clip: the range it uses; default the selected one),"item":id? (a media item: all of it),"stems":["vocals","rest"] (any of vocals, rest = everything but the voice, drums, bass, other),"out":folder?,"wait":bool=false}"#,
        enabled: crate::commands::always,
        run: separate,
        journal: true,
    }]
}
