//! Optional neural tools (build feature `ml`): model store and the commands that use them.
//!
//! | command | what it does | model |
//! |---|---|---|
//! | `ml.models` | lists the optional models, their size, licence and whether they are installed | none |
//! | `ml.install {model, acceptLicense}` | downloads one (SHA-256 checked) after the licence was accepted | none |
//! | `sequence.markBeats` / `sequence.cutOnBeats` with `model:"beat-this"` | beats from the Beat This! network instead of the DSP tracker (see `beats`) | `beat-this-final0` |
//!
//! No model is part of FilmCraft: `ml.install` is the only way one arrives, it refuses without
//! `acceptLicense:true` and says what the licence is, how big the download is and where it comes from.
//! Models live in `FILMCRAFT_ML_MODELS_DIR`, else `<data dir>/models/ml/<id>/`.

use std::path::PathBuf;

use filmcraft_audio_dsp::beatgrid::Grid;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, str_p};
use crate::{EngineError, Result, Session};

/// Where the optional models live.
pub fn models_dir() -> Option<PathBuf> {
    std::env::var_os("FILMCRAFT_ML_MODELS_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::autosave::default_data_dir().map(|d| d.join("models").join("ml")))
}

#[cfg(not(feature = "ml"))]
fn no_ml(cmd: &str) -> EngineError {
    bad(cmd, "this build has no neural tools (build with --features ml)")
}

/// Beat times (and downbeat times) in seconds as a [`Grid`] at `sr` for a window of `len` samples.
/// `hint` picks the tempo octave among the tracked beats (thinned or doubled); `bars` fills the bar start.
pub(crate) fn grid_from_beats(beats: &[f64], downbeats: &[f64], sr: u32, len: i64, hint: Option<f64>, bars: bool) -> Option<Grid> {
    if beats.len() < 4 {
        return None;
    }
    let mut ivals: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
    ivals.sort_by(f64::total_cmp);
    let med = ivals[ivals.len() / 2].max(1e-3);
    let mut t: Vec<f64> = beats.to_vec();
    let mut bpm = 60.0 / med;
    if let Some(h) = hint.filter(|h| *h > 0.0) {
        // the octave of the tracked tempo nearest to the hint (in log distance)
        let (mut best, mut best_d) = (1.0f64, f64::MAX);
        for r in [0.25, 0.5, 1.0, 2.0, 4.0] {
            let d = ((bpm * r) / h).ln().abs();
            if d < best_d {
                best_d = d;
                best = r;
            }
        }
        if best < 1.0 {
            // keep one beat in 1/best, starting on a downbeat when there is one
            let step = (1.0 / best).round() as usize;
            let first = downbeats.first().and_then(|d| t.iter().position(|b| (b - d).abs() < 1e-6)).map_or(0, |i| i % step);
            t = t.iter().copied().skip(first).step_by(step).collect();
        } else if best > 1.0 {
            let k = best.round() as usize;
            let mut u = Vec::with_capacity(t.len() * k);
            for (i, b) in t.iter().enumerate() {
                u.push(*b);
                if let Some(n) = t.get(i + 1) {
                    for j in 1..k {
                        u.push(b + (n - b) * j as f64 / k as f64);
                    }
                }
            }
            t = u;
        }
        bpm *= best;
    }
    let (mut bar_start, mut conf) = (None, 0.0);
    if bars && !downbeats.is_empty() {
        // the first beat that is a downbeat; confidence = share of bars as long as the usual bar
        let idx: Vec<usize> = downbeats.iter().filter_map(|d| t.iter().position(|b| (b - d).abs() < 1e-6)).collect();
        if let Some(&f) = idx.first() {
            bar_start = Some(f.min(7));
            let gaps: Vec<usize> = idx.windows(2).map(|w| w[1] - w[0]).collect();
            if !gaps.is_empty() {
                let mut g = gaps.clone();
                g.sort_unstable();
                let mode = g[g.len() / 2];
                conf = gaps.iter().filter(|x| **x == mode).count() as f64 / gaps.len() as f64;
            }
        }
    }
    Some(Grid {
        sample_rate: sr,
        len,
        beats: t.iter().map(|s| (s * sr as f64).round() as i64).collect(),
        bpm,
        bar_start,
        bar_confidence: conf,
        candidates: Vec::new(),
    })
}

#[cfg(feature = "ml")]
mod on {
    use super::*;
    use std::sync::{Arc, Mutex, OnceLock};

    use filmcraft_audio_dsp::beatgrid::GridOptions;
    use filmcraft_ml::catalogue::{self, Model};
    use filmcraft_project::ItemId;
    use filmcraft_time::Tick;

    /// The folder of an installed model, or the message telling how to install it.
    pub fn require(m: &Model, cmd: &str) -> Result<PathBuf> {
        let dir = models_dir().ok_or_else(|| bad(cmd, "no data directory for the neural models"))?;
        if !m.installed(&dir) {
            return Err(bad(
                cmd,
                format!(
                    "le modèle « {} » n'est pas installé ({:.0} Mo). Installez-le avec ml.install {{\"model\":\"{}\",\"acceptLicense\":true}} après avoir lu sa licence : {}",
                    m.title,
                    m.size() as f64 / 1e6,
                    m.id,
                    m.licence
                ),
            ));
        }
        Ok(m.dir(&dir))
    }

    pub fn model_json(m: &Model) -> Value {
        let dir = models_dir();
        json!({
            "id": m.id, "title": m.title, "use": m.use_for, "bytes": m.size(),
            "installed": dir.as_deref().is_some_and(|d| m.installed(d)),
            "licence": m.licence, "licenceUrl": m.licence_url, "source": m.source_url, "personalOnly": m.personal_only,
            "files": m.files.iter().map(|f| json!({"name": f.name, "bytes": f.size, "sha256": f.sha256})).collect::<Vec<_>>(),
        })
    }

    pub fn models(_: &mut Session, _: &Value) -> Result<Value> {
        let list: Vec<Value> = catalogue::all().into_iter().map(model_json).collect();
        Ok(
            json!({"dir": models_dir().map(|d| d.display().to_string()), "models": list, "summary": format!("{} modèles optionnels, {} installé(s)", list.len(), list.iter().filter(|m| m["installed"] == true).count())}),
        )
    }

    /// Run `work` as a background job (or inline when `wait`): its JSON result is the command's result when
    /// waiting, or what `job.list` reports as `result.path`-less success when not.
    pub fn run_job(
        s: &mut Session,
        label: String,
        total: u64,
        wait: bool,
        work: impl FnOnce(&Arc<filmcraft_export::Progress>) -> std::result::Result<Value, String> + Send + 'static,
    ) -> Result<Value> {
        use std::sync::atomic::Ordering;
        let jid = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
        let job = crate::Job { id: jid, label, progress: Default::default(), result: Default::default() };
        job.progress.total.store(total, Ordering::Relaxed);
        let (prog, res) = (job.progress.clone(), job.result.clone());
        let out: Arc<Mutex<Option<Value>>> = Arc::default();
        let out2 = out.clone();
        let run = move || {
            let t0 = std::time::Instant::now();
            let r = work(&prog);
            let secs = t0.elapsed().as_secs_f64();
            let r = match r {
                Ok(v) => {
                    *out2.lock().unwrap_or_else(|e| e.into_inner()) = Some(v);
                    Ok(filmcraft_export::Report {
                        path: String::new(),
                        frames: prog.done.load(Ordering::Relaxed),
                        seconds: secs,
                        bytes: 0,
                        render_fps: 0.0,
                        extra_files: Vec::new(),
                    })
                }
                Err(e) => {
                    *prog.error.lock().unwrap_or_else(|x| x.into_inner()) = Some(e.clone());
                    Err(e)
                }
            };
            *prog.status.lock().unwrap_or_else(|e| e.into_inner()) = match &r {
                Ok(_) => format!("Done in {secs:.1}s"),
                Err(e) => e.clone(),
            };
            prog.finished.store(true, Ordering::Relaxed);
            *res.lock().unwrap_or_else(|x| x.into_inner()) = Some(r);
        };
        let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
        s.jobs.push(job);
        if wait {
            run();
            let j = s.jobs.iter().find(|j| j.id == jid).map(|j| j.to_json()).unwrap_or(Value::Null);
            if let Some(e) = j["result"]["error"].as_str() {
                return Err(EngineError::Other(e.to_string()));
            }
            let mut v = out.lock().unwrap_or_else(|e| e.into_inner()).take().unwrap_or(Value::Null);
            if let Value::Object(m) = &mut v {
                m.insert("job".into(), json!(jid));
            }
            return Ok(v);
        }
        std::thread::Builder::new().name("filmcraft-ml-job".into()).spawn(run).map_err(|e| EngineError::Other(e.to_string()))?;
        Ok(json!({"job": jid, "background": true}))
    }

    pub fn install(s: &mut Session, p: &Value) -> Result<Value> {
        const CMD: &str = "ml.install";
        let id = str_p(p, "model").ok_or_else(|| bad(CMD, "`model` is required (see ml.models)"))?;
        let m = catalogue::find(id).ok_or_else(|| bad(CMD, format!("unknown model {id} (see ml.models)")))?;
        let dir = models_dir().ok_or_else(|| bad(CMD, "no data directory for the neural models"))?;
        if !bool_p(p, "acceptLicense").unwrap_or(false) {
            // the `ML_LICENCE[id]` marker lets the window turn this refusal into its licence dialog
            return Err(bad(
                CMD,
                format!(
                    "ML_LICENCE[{}] licence à accepter avant le téléchargement de « {} » ({:.1} Mo depuis {}) : {} — {}{} Relancez avec acceptLicense:true.",
                    m.id,
                    m.title,
                    m.size() as f64 / 1e6,
                    m.source_url,
                    m.licence,
                    m.licence_url,
                    if m.personal_only { " USAGE PERSONNEL / NON COMMERCIAL uniquement." } else { "" }
                ),
            ));
        }
        let was = m.installed(&dir);
        let licence_note = format!("{}\n{}\nSource: {}\nAccepted by the user with ml.install.\n", m.licence, m.licence_url, m.source_url);
        if bool_p(p, "wait") == Some(false) {
            // a background job (the window): bytes done / total in the job's progress
            use std::sync::atomic::Ordering;
            let jid = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
            let job = crate::Job { id: jid, label: format!("Download {}", m.title), progress: Default::default(), result: Default::default() };
            job.progress.total.store(m.size(), Ordering::Relaxed);
            let (prog, res) = (job.progress.clone(), job.result.clone());
            let run = move || {
                let t0 = std::time::Instant::now();
                let r = catalogue::download(m, &dir, &prog.cancel, &mut |pr| {
                    prog.total.store(pr.total, Ordering::Relaxed);
                    prog.done.store(pr.done, Ordering::Relaxed);
                })
                .map_err(|e| format!("{}: {e}", m.title));
                let secs = t0.elapsed().as_secs_f64();
                let r = match r {
                    Ok(()) => {
                        let _ = std::fs::write(m.dir(&dir).join("LICENSE.txt"), &licence_note);
                        Ok(filmcraft_export::Report {
                            path: m.dir(&dir).display().to_string(),
                            frames: 0,
                            seconds: secs,
                            bytes: m.size(),
                            render_fps: 0.0,
                            extra_files: Vec::new(),
                        })
                    }
                    Err(e) => {
                        *prog.error.lock().unwrap_or_else(|x| x.into_inner()) = Some(e.clone());
                        Err(e)
                    }
                };
                *prog.status.lock().unwrap_or_else(|e| e.into_inner()) = match &r {
                    Ok(_) => format!("Installed in {secs:.1}s"),
                    Err(e) => e.clone(),
                };
                prog.finished.store(true, Ordering::Relaxed);
                *res.lock().unwrap_or_else(|x| x.into_inner()) = Some(r);
            };
            let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
            s.jobs.push(job);
            std::thread::Builder::new().name("filmcraft-ml-download".into()).spawn(run).map_err(|e| EngineError::Other(e.to_string()))?;
            return Ok(
                json!({"job": jid, "model": m.id, "bytes": m.size(), "summary": format!("téléchargement de {} lancé ({:.1} Mo)", m.title, m.size() as f64 / 1e6)}),
            );
        }
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let t0 = std::time::Instant::now();
        catalogue::download(m, &dir, &cancel, &mut |_| {}).map_err(|e| EngineError::Other(format!("{}: {e}", m.title)))?;
        let dest = m.dir(&dir);
        let _ = std::fs::write(dest.join("LICENSE.txt"), licence_note);
        Ok(json!({
            "model": m.id, "dir": dest.display().to_string(), "bytes": m.size(), "alreadyInstalled": was, "seconds": t0.elapsed().as_secs_f64(),
            "summary": if was { format!("{} était déjà installé (fichiers vérifiés)", m.title) } else { format!("{} installé ({:.1} Mo, SHA-256 vérifié)", m.title, m.size() as f64 / 1e6) },
        }))
    }

    type Cache = Mutex<Option<(PathBuf, Arc<filmcraft_ml::beat_this::BeatThis>)>>;
    fn beat_cache() -> &'static Cache {
        static C: OnceLock<Cache> = OnceLock::new();
        C.get_or_init(Default::default)
    }

    fn beat_model(cmd: &str) -> Result<Arc<filmcraft_ml::beat_this::BeatThis>> {
        let dir = require(&catalogue::BEAT_THIS, cmd)?;
        let mut c = beat_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some((d, m)) = c.as_ref()
            && *d == dir
        {
            return Ok(m.clone());
        }
        let m = Arc::new(filmcraft_ml::beat_this::BeatThis::load(&dir).map_err(|e| bad(cmd, e.to_string()))?);
        *c = Some((dir, m.clone()));
        Ok(m)
    }

    /// The beat grid of `[source_in, source_in + dur)` of the item's media from Beat This!.
    pub fn neural_grid(s: &Session, item: ItemId, source_in: Tick, dur: Tick, opts: GridOptions, cmd: &str) -> Result<Grid> {
        let sr = s.active_sequence().map(|q| q.settings.sample_rate).unwrap_or(48_000).max(1);
        let model = beat_model(cmd)?;
        let r = filmcraft_ml::beat_this::SAMPLE_RATE as i64;
        let m0 = source_in.to_units_floor(r);
        let len = ((source_in + dur).to_units_floor(r) - m0).max(0) as usize;
        let provider = s.media.provider(s.project.clone(), s.services.clone());
        let src = filmcraft_render::SourceProvider::source(&provider, item).ok_or_else(|| bad(cmd, "le média du clip est hors ligne"))?;
        let buf = src.audio(m0, len, r as u32).map_err(|e| bad(cmd, format!("lecture de l'audio du clip impossible : {e}")))?;
        let n = buf.channels.iter().map(Vec::len).min().unwrap_or(0);
        if buf.channels.is_empty() || n == 0 {
            return Err(bad(cmd, "le clip n'a pas d'audio"));
        }
        let mono: Vec<f32> = (0..n).map(|i| buf.channels.iter().map(|c| c[i]).sum::<f32>() / buf.channels.len() as f32).collect();
        let tracked = model.track(&mono, &mut |_, _| true).map_err(|e| bad(cmd, format!("Beat This!: {e}")))?;
        grid_from_beats(&tracked.beats, &tracked.downbeats, sr, (dur.seconds() * sr as f64) as i64, opts.tempo_hint, opts.downbeat)
            .ok_or_else(|| bad(cmd, "0 beat détecté dans ce clip par Beat This! : pas de pulsation (voix, ambiance, bruit, ou clip trop court)"))
    }
}

#[cfg(feature = "ml")]
pub(crate) use on::{neural_grid, require, run_job};

#[cfg(not(feature = "ml"))]
pub(crate) fn neural_grid(
    _: &Session,
    _: filmcraft_project::ItemId,
    _: filmcraft_time::Tick,
    _: filmcraft_time::Tick,
    _: filmcraft_audio_dsp::beatgrid::GridOptions,
    cmd: &str,
) -> Result<Grid> {
    Err(no_ml(cmd))
}

fn models(s: &mut Session, p: &Value) -> Result<Value> {
    #[cfg(feature = "ml")]
    return on::models(s, p);
    #[cfg(not(feature = "ml"))]
    {
        let _ = (s, p);
        Err(no_ml("ml.models"))
    }
}

fn install(s: &mut Session, p: &Value) -> Result<Value> {
    #[cfg(feature = "ml")]
    return on::install(s, p);
    #[cfg(not(feature = "ml"))]
    {
        let _ = (s, p);
        Err(no_ml("ml.install"))
    }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec { id: "ml.models", label: "Neural models…", menu: &[], shortcut: None, params: r#"{}"#, enabled: always, run: models, journal: false },
        CommandSpec {
            id: "ml.install",
            label: "Install a neural model…",
            menu: &[],
            shortcut: None,
            params: r#"{"model":id (see ml.models),"acceptLicense":bool (the licence was read and accepted; without it the command only says what the licence is),"wait":bool (false: a background job, what the window uses)}"#,
            enabled: always,
            run: install,
            journal: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(bpm: f64, n: usize) -> Vec<f64> {
        (0..n).map(|i| i as f64 * 60.0 / bpm).collect()
    }

    #[test]
    fn grid_keeps_tempo_and_bars() {
        let b = line(120.0, 17);
        let d: Vec<f64> = b.iter().copied().skip(1).step_by(4).collect();
        let g = grid_from_beats(&b, &d, 48_000, 48_000 * 10, None, true).unwrap();
        assert!((g.bpm - 120.0).abs() < 0.01);
        assert_eq!(g.bar_start, Some(1));
        assert!(g.bar_confidence > 0.99);
        assert_eq!(g.beats[2], 48_000);
    }

    #[test]
    fn hint_picks_the_octave() {
        let b = line(180.0, 33);
        let half = grid_from_beats(&b, &[], 48_000, 48_000 * 10, Some(90.0), false).unwrap();
        assert!((half.bpm - 90.0).abs() < 0.01 && half.beats.len() == 17, "{} {}", half.bpm, half.beats.len());
        let dbl = grid_from_beats(&line(90.0, 9), &[], 48_000, 48_000 * 10, Some(180.0), false).unwrap();
        assert!((dbl.bpm - 180.0).abs() < 0.01 && dbl.beats.len() == 17);
        assert!(grid_from_beats(&line(90.0, 3), &[], 48_000, 1, None, false).is_none());
    }
}
