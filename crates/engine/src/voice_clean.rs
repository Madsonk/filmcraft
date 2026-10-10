//! Neural voice clean-up: `audio.denoiseVoice`, and the model commands `models.list` / `models.install`.
//!
//! | command | what it does |
//! |---|---|
//! | `audio.denoiseVoice` | removes the background noise from the selected audio clips (or `clips`) with DeepFilterNet3, as one undo step |
//! | `models.list` | the optional neural models (voice clean-up, subject detection): size, source, licence, installed or not |
//! | `models.install` | downloads one after the caller has accepted its licence (`accept: true`); SHA-256 checked |
//!
//! **How a clip is cleaned.** The audio the clip plays (`source_in` … `source_in + duration`, every
//! channel up to two) is read at the sequence's sample rate, each channel goes through the model,
//! and the result is written as a 32-bit float WAV next to the project (Project Settings ▸ Scratch
//! Disks ▸ Captured Audio, else the project folder, else the data directory), imported, and the
//! clip is pointed at it. Position, duration, keyframes, effects, gain and links are untouched; the
//! original media stays in the project, so Undo (one step) restores everything but the file on disk.
//! Clips played at another speed, reversed or remixed are refused rather than guessed at.
//!
//! **Strength.** `strength` is `light` (50 %: noise reduced by at most 6 dB), `standard` (80 %:
//! 14 dB) or `strong` (100 %: as much as the model finds); `amount` (0 … 100) sets it directly.
//! Below 100 % the original is mixed back into the spectrum, which keeps a natural room feel.
//!
//! **Measured before and after** (no reference needed): the noise floor is the 10th percentile and
//! the speech level the 90th percentile of the 20 ms frame levels; their difference is an estimate
//! of the signal-to-noise ratio. They are returned for each clip and shown in the summary.
//!
//! The work runs on the calling thread (about 30× faster than real time on an Apple-silicon CPU).

use std::sync::{Arc, Mutex, OnceLock};

use filmcraft_project::{ClipId, ItemId, TrackKind};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, bool_p, f64_p, has_seq, str_p};
use crate::{EngineError, Result, Session};

const MODEL_ID: &str = "deepfilternet3";

// ---------------------------------------------------------------------------------- the models

fn models_root(p: &Value) -> Result<std::path::PathBuf> {
    if let Some(d) = str_p(p, "modelsDir") {
        return Ok(d.into());
    }
    crate::transcript::models_dir().ok_or_else(|| EngineError::Other("no data directory for models".into()))
}

fn model_json(root: &std::path::Path, m: &filmcraft_modelstore::ModelInfo) -> Value {
    json!({
        "id": m.id, "name": m.name, "task": m.task, "description": m.description, "license": m.license, "licenseUrl": m.license_url,
        "author": m.author, "source": m.source, "size": m.size(), "installed": filmcraft_modelstore::installed(root, m),
        "files": m.files.iter().map(|f| json!({"name": f.name, "url": f.url, "sha256": f.sha256, "size": f.size})).collect::<Vec<_>>(),
    })
}

fn models_list(_: &mut Session, p: &Value) -> Result<Value> {
    let root = models_root(p)?;
    let all: Vec<Value> = filmcraft_modelstore::catalogue().iter().map(|m| model_json(&root, m)).collect();
    let n = all.iter().filter(|m| m["installed"] == json!(true)).count();
    Ok(json!({
        "dir": root.to_string_lossy(), "models": all, "download": cfg!(feature = "model-download"),
        "summary": format!("{n} modèle{} installé{} sur {}", if n > 1 { "s" } else { "" }, if n > 1 { "s" } else { "" }, filmcraft_modelstore::catalogue().len()),
    }))
}

/// The refusal that tells the caller exactly what would be downloaded.
pub(crate) fn consent_text(m: &filmcraft_modelstore::ModelInfo) -> String {
    format!(
        "{} ({} Mo, depuis {}, licence {}, {}) n'est pas installé : `models.install {{\"model\":\"{}\",\"accept\":true}}` le télécharge (SHA-256 vérifié) après acceptation de sa licence",
        m.name,
        (m.size() as f64 / 1e6 * 10.0).round() / 10.0,
        m.source,
        m.license,
        m.license_url,
        m.id
    )
}

fn models_install(_: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "models.install";
    let id = str_p(p, "model").ok_or_else(|| bad(CMD, "need `model` (see models.list)"))?;
    let m = filmcraft_modelstore::find(id).ok_or_else(|| bad(CMD, format!("unknown model `{id}`")))?;
    let root = models_root(p)?;
    if filmcraft_modelstore::installed(&root, m) {
        return Ok(json!({"model": m.id, "installed": true, "downloaded": 0, "summary": format!("{} est déjà installé", m.name)}));
    }
    if !bool_p(p, "accept").unwrap_or(false) {
        return Err(bad(CMD, consent_text(m)));
    }
    #[cfg(feature = "model-download")]
    {
        filmcraft_modelstore::download(&root, m, &mut |_, _, _| true).map_err(|e| bad(CMD, e.to_string()))?;
        Ok(json!({"model": m.id, "installed": true, "downloaded": m.size(), "dir": filmcraft_modelstore::model_dir(&root, m).to_string_lossy(),
            "summary": format!("{} installé ({} Mo, licence {})", m.name, (m.size() as f64 / 1e5).round() / 10.0, m.license)}))
    }
    #[cfg(not(feature = "model-download"))]
    {
        Err(bad(CMD, "model downloads are not available in this build (built without the `model-download` feature)"))
    }
}

/// For hosts that show the licence themselves (the window's consent dialog): downloads model `id`
/// into the models directory. `progress(done, total)` returns false to cancel.
pub fn install_model(id: &str, progress: &mut dyn FnMut(u64, u64) -> bool) -> std::result::Result<String, String> {
    let m = filmcraft_modelstore::find(id).ok_or_else(|| format!("unknown model `{id}`"))?;
    let root = crate::transcript::models_dir().ok_or("no data directory for models")?;
    #[cfg(feature = "model-download")]
    {
        filmcraft_modelstore::download(&root, m, &mut |d, t, _| progress(d, t)).map_err(|e| e.to_string())?;
        Ok(format!("{} installé", m.name))
    }
    #[cfg(not(feature = "model-download"))]
    {
        let _ = (m, root, progress);
        Err("model downloads are not available in this build (built without the `model-download` feature)".into())
    }
}

type Loaded = (std::path::PathBuf, Arc<filmcraft_voice::VoiceModel>);

fn voice_model(p: &Value, cmd: &str) -> Result<Arc<filmcraft_voice::VoiceModel>> {
    static CACHE: OnceLock<Mutex<Option<Loaded>>> = OnceLock::new();
    let root = models_root(p)?;
    let m = filmcraft_modelstore::find(MODEL_ID).ok_or_else(|| bad(cmd, "model catalogue error"))?;
    let mut c = CACHE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some((dir, model)) = c.as_ref()
        && *dir == root
        && filmcraft_modelstore::installed(&root, m)
    {
        return Ok(model.clone());
    }
    if !filmcraft_modelstore::installed(&root, m) {
        return Err(bad(cmd, consent_text(m)));
    }
    let rd = |n: &str| filmcraft_modelstore::read(&root, m, n).map_err(|e| bad(cmd, e.to_string()));
    let model = Arc::new(
        filmcraft_voice::VoiceModel::from_bytes(&rd("enc.onnx")?, &rd("erb_dec.onnx")?, &rd("df_dec.onnx")?)
            .map_err(|e| bad(cmd, format!("model unreadable: {e}")))?,
    );
    *c = Some((root, model.clone()));
    Ok(model)
}

// ------------------------------------------------------------------------------- measurements

/// `(noise floor, speech level)` in dBFS from 20 ms frame levels: 10th and 90th percentile of all
/// the frames, a digital-silence frame counting as -120 dBFS (the model zeroes the frames it takes
/// for noise only, so the floor of a cleaned clip is often -120).
pub fn floor_and_level(x: &[f32], sr: u32) -> (f64, f64) {
    let w = (sr as usize / 50).max(1);
    let mut db: Vec<f64> = x
        .chunks(w)
        .filter(|c| c.len() == w)
        .map(|c| (c.iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>() / w as f64).sqrt())
        .map(|r| 20.0 * r.max(1e-6).log10())
        .collect();
    if db.is_empty() {
        return (-120.0, -120.0);
    }
    db.sort_by(|a, b| a.total_cmp(b));
    let at = |q: f64| db.get(((db.len() - 1) as f64 * q).round() as usize).copied().unwrap_or(-120.0);
    (at(0.10), at(0.90))
}

fn fr(v: f64) -> String {
    format!("{v:.1}").replace('.', ",")
}

fn level_json(x: &[f32], sr: u32) -> Value {
    let (f, l) = floor_and_level(x, sr);
    json!({"floorDb": (f * 10.0).round() / 10.0, "speechDb": (l * 10.0).round() / 10.0, "snrDb": ((l - f) * 10.0).round() / 10.0})
}

// ------------------------------------------------------------------------------------ the command

/// A 32-bit float WAV, interleaved.
pub fn write_wav_f32_multi(channels: &[Vec<f32>], rate: u32) -> Vec<u8> {
    let n = channels.first().map_or(0, Vec::len);
    let ch = channels.len().max(1) as u32;
    let data_len = n * ch as usize * 4;
    let mut v = Vec::with_capacity(44 + data_len);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&3u16.to_le_bytes());
    v.extend_from_slice(&(ch as u16).to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * 4 * ch).to_le_bytes());
    v.extend_from_slice(&((4 * ch) as u16).to_le_bytes());
    v.extend_from_slice(&32u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&(data_len as u32).to_le_bytes());
    for i in 0..n {
        for c in channels {
            v.extend_from_slice(&c.get(i).copied().unwrap_or(0.0).to_le_bytes());
        }
    }
    v
}

fn strength(p: &Value, cmd: &str) -> Result<f32> {
    if let Some(a) = f64_p(p, "amount") {
        if !(0.0..=100.0).contains(&a) {
            return Err(bad(cmd, "`amount` must be between 0 and 100"));
        }
        return Ok((a / 100.0) as f32);
    }
    match str_p(p, "strength").unwrap_or("standard") {
        "light" | "leger" | "léger" => Ok(0.5),
        "standard" => Ok(0.8),
        "strong" | "fort" => Ok(1.0),
        other => Err(bad(cmd, format!("unknown strength `{other}` (light, standard, strong)"))),
    }
}

fn out_dir(s: &Session, p: &Value) -> std::path::PathBuf {
    if let Some(d) = str_p(p, "dir") {
        return d.into();
    }
    if let Some(d) = s.project.settings.scratch.captured.clone().filter(|d| !d.is_empty()) {
        return d.into();
    }
    if let Some(d) = s.path.as_deref().and_then(|x| std::path::Path::new(x).parent()).filter(|d| !d.as_os_str().is_empty()) {
        return d.to_path_buf();
    }
    let base = s.prefs_path.as_ref().and_then(|p| p.parent()).map(|d| d.to_path_buf()).unwrap_or_else(|| crate::temp_dir().join("FilmCraft"));
    base.join("Voice Clean-up")
}

struct Job {
    clip: ClipId,
    item: ItemId,
    name: String,
    source_in: Tick,
    duration: Tick,
    channels: Vec<usize>,
}

fn targets(s: &Session, p: &Value, cmd: &str) -> Result<Vec<Job>> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let ids: Vec<ClipId> = match p.get("clips").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|v| v.as_u64().map(ClipId)).collect(),
        None => s.state.selection.to_vec(),
    };
    let mut jobs = Vec::new();
    for c in ids {
        let Some((t, it)) = q.find_item(c) else { continue };
        if q.track(t).is_none_or(|t| t.kind != TrackKind::Audio) {
            continue;
        }
        if filmcraft_render::remix::is_remixed(it) {
            return Err(bad(cmd, "a clip is a remix clip: revert the remix first"));
        }
        if it.reverse || (it.speed - 1.0).abs() > 1e-9 {
            return Err(bad(cmd, "a clip is retimed (speed or reverse): clean it at 100 % forwards, then retime it"));
        }
        let channels = if it.source_channels.is_empty() { vec![0, 1] } else { it.source_channels.iter().map(|c| usize::from(*c)).take(2).collect() };
        let name = s.project.items.get(&it.item).map(|i| i.name.clone()).unwrap_or_else(|| it.name.clone());
        jobs.push(Job { clip: c, item: it.item, name, source_in: it.source_in, duration: it.duration, channels });
    }
    if jobs.is_empty() {
        return Err(bad(cmd, "select the audio clip(s) to clean (or pass `clips`)"));
    }
    Ok(jobs)
}

fn denoise_voice(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "audio.denoiseVoice";
    let amount = strength(p, CMD)?;
    let jobs = targets(s, p, CMD)?;
    let model = voice_model(p, CMD)?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let sr = q.settings.sample_rate.max(1);
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let dir = out_dir(s, p);
    let n0 = s.history.undo.len();
    let t_all = std::time::Instant::now();
    let mut report = Vec::new();
    let mut lines = Vec::new();
    let mut taken: std::collections::HashSet<String> = s.project.items.values().map(|i| i.name.clone()).collect();
    for job in &jobs {
        let t0 = std::time::Instant::now();
        let m0 = job.source_in.to_units_floor(i64::from(sr));
        let len = (job.source_in + job.duration).to_units_floor(i64::from(sr)) - m0 + 1;
        let src = filmcraft_render::SourceProvider::source(&provider, job.item).ok_or_else(|| bad(CMD, "le média du clip est hors ligne"))?;
        let buf = src.audio(m0, len.max(0) as usize, sr).map_err(|e| bad(CMD, format!("lecture de l'audio du clip impossible : {e}")))?;
        let mut chans: Vec<Vec<f32>> = job.channels.iter().filter_map(|c| buf.channels.get(*c).or_else(|| buf.channels.first()).cloned()).collect();
        if job.channels.len() == 2 && buf.channels.len() == 1 {
            chans.truncate(1);
        }
        if chans.is_empty() || chans.iter().all(Vec::is_empty) {
            return Err(bad(CMD, "le clip n'a pas d'audio à nettoyer"));
        }
        let before = chans.first().map(|c| level_json(c, sr)).unwrap_or_default();
        let mut cleaned: Vec<Vec<f32>> = Vec::with_capacity(chans.len());
        for (i, ch) in chans.iter().enumerate() {
            if i > 0
                && chans.first() == Some(ch)
                && let Some(first) = cleaned.first().cloned()
            {
                cleaned.push(first);
                continue;
            }
            let out =
                model.denoise(ch, sr, &filmcraft_voice::Options { amount }, &mut |_| true).map_err(|e| bad(CMD, format!("nettoyage impossible : {e}")))?;
            cleaned.push(out);
        }
        let after = cleaned.first().map(|c| level_json(c, sr)).unwrap_or_default();
        // the file
        let stem = std::path::Path::new(&job.name).file_stem().map(|x| x.to_string_lossy().into_owned()).unwrap_or_else(|| "Audio".into());
        let mut k = 1;
        let (file, path) = loop {
            let f = if k == 1 { format!("{stem} (voix nettoyée).wav") } else { format!("{stem} (voix nettoyée {k}).wav") };
            let path = dir.join(&f);
            if !taken.contains(&f) && s.services.file_size(&path.to_string_lossy()).is_err() {
                break (f, path);
            }
            k += 1;
        };
        taken.insert(file);
        if !cfg!(target_arch = "wasm32") {
            std::fs::create_dir_all(&dir).map_err(|e| EngineError::Other(format!("{}: {e}", dir.display())))?;
        }
        let bytes = write_wav_f32_multi(&cleaned, sr);
        let path_s = path.to_string_lossy().into_owned();
        s.services.write_file(&path_s, &bytes).map_err(|e| EngineError::Other(format!("{path_s}: {e}")))?;
        let item = crate::commands::import_bytes(s, &path_s, bytes.into(), None)?;
        let clip = job.clip;
        s.edit_sequence("Clean Voice", move |q, _, _| {
            let (_, it) = q.find_item_mut(clip).ok_or_else(|| bad(CMD, "the clip disappeared"))?;
            it.item = item;
            it.source_in = Tick::ZERO;
            it.source_channels.clear();
            Ok(())
        })?;
        let secs = len.max(0) as f64 / f64::from(sr);
        let (b, a) = (before.clone(), after.clone());
        lines.push(format!(
            "bruit de fond {} → {} dBFS, rapport voix/bruit estimé {} → {} dB",
            fr(b["floorDb"].as_f64().unwrap_or(0.0)),
            fr(a["floorDb"].as_f64().unwrap_or(0.0)),
            fr(b["snrDb"].as_f64().unwrap_or(0.0)),
            fr(a["snrDb"].as_f64().unwrap_or(0.0))
        ));
        report.push(json!({
            "clip": clip.0, "item": item.0, "path": path_s, "channels": cleaned.len(), "seconds": secs,
            "before": before, "after": after, "elapsedMs": t0.elapsed().as_millis() as u64,
        }));
    }
    crate::clip_ops::collapse_history(s, n0, "Clean Voice");
    let n = report.len();
    Ok(json!({
        "model": MODEL_ID, "amount": amount, "clips": report,
        "summary": format!(
            "{n} clip{} nettoyé{} (DeepFilterNet3, force {:.0} %) : {} · {} s",
            if n > 1 { "s" } else { "" },
            if n > 1 { "s" } else { "" },
            amount * 100.0,
            lines.join(" ; "),
            fr(t_all.elapsed().as_secs_f64())
        ),
    }))
}

fn has_audio_clip(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    let q = s.active_sequence().ok_or("no sequence is open")?;
    if q.audio_tracks.iter().any(|t| !t.items.is_empty()) { Ok(()) } else { Err("the sequence has no audio clips".into()) }
}

fn always(_: &Session) -> std::result::Result<(), String> {
    Ok(())
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "audio.denoiseVoice",
            label: "Clean Voice…",
            menu: &[],
            shortcut: None,
            params: r#"{"clips":[id]? (default: the selected audio clips),"strength":"light|standard|strong"="standard","amount":0-100? (overrides strength: 50 = at most 6 dB of noise removed, 80 = 14 dB, 100 = all the model finds),"dir":str? (where the cleaned WAV is written),"modelsDir":str?}"#,
            enabled: has_audio_clip,
            run: denoise_voice,
            journal: true,
        },
        CommandSpec {
            id: "models.list",
            label: "List Neural Models",
            menu: &[],
            shortcut: None,
            params: r#"{"modelsDir":str?}"#,
            enabled: always,
            run: models_list,
            journal: false,
        },
        CommandSpec {
            id: "models.install",
            label: "Install Neural Model",
            menu: &[],
            shortcut: None,
            params: r#"{"model":"deepfilternet3|yolox-nano|yolox-tiny|yolox-s|yunet","accept":bool (the caller has shown the licence),"modelsDir":str?}"#,
            enabled: always,
            run: models_install,
            journal: false,
        },
    ]
}
