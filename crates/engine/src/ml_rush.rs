//! Semantic search in video rushes (build feature `ml`, model `siglip2-base-224`).
//!
//! | command | what it does |
//! |---|---|
//! | `media.rushIndex` | embeds one frame every `every` seconds (default 2) of the video media of the project with SigLIP 2 and stores the vectors next to the models (`index/<media key>.bin`, one file per media, re-used while the file is unchanged) |
//! | `media.rushSearch {query}` | embeds the sentence (French or English: SigLIP 2 is multilingual), ranks every indexed frame by cosine similarity and returns the best moments as media, time, timecode and score; `markers:true` also puts a marker on the media at each hit |
//!
//! The ranking is the model's, nothing is invented: a score is a cosine similarity (about 0.05 to 0.2 for
//! a real match with SigLIP: compare scores of one query, not across queries).

use serde_json::Value;

use crate::commands::{CommandSpec, bad};
#[allow(unused_imports)]
use crate::{EngineError, Result, Session};

#[cfg(not(feature = "ml"))]
fn no_ml(cmd: &str) -> EngineError {
    bad(cmd, "this build has no neural tools (build with --features ml)")
}

#[cfg(feature = "ml")]
mod on {
    use std::io::{Read, Write};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, OnceLock};

    use filmcraft_media::FrameRequest;
    use filmcraft_ml::catalogue;
    use filmcraft_mlsearch::Embedder;
    use filmcraft_project::{ItemId, ItemKind, Label, Marker, MarkerId, MarkerKind, MediaRef};
    use filmcraft_time::Tick;
    use serde_json::{Value, json};

    use super::*;
    use crate::commands::{bool_p, f64_p, str_p, u64_p};
    use crate::ml_tools::{models_dir, require};

    const MAGIC: &[u8; 8] = b"FCRUSH01";

    struct Index {
        every: f32,
        times: Vec<f32>,
        vecs: Vec<f32>,
    }

    fn index_dir() -> Option<PathBuf> {
        std::env::var_os("FILMCRAFT_ML_INDEX_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| models_dir().and_then(|d| d.parent().map(|p| p.join("index"))))
    }

    fn media_key(path: &str, model: &str) -> Option<String> {
        let md = std::fs::metadata(path).ok()?;
        let mtime = md.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
        Some(format!("{:016x}", filmcraft_mlsearch::key_of(&format!("{path}|{}|{mtime}|{model}", md.len()))))
    }

    fn load_index(path: &std::path::Path, dim: usize) -> Option<Index> {
        let mut f = std::fs::File::open(path).ok()?;
        let mut head = [0u8; 20];
        f.read_exact(&mut head).ok()?;
        if &head[..8] != MAGIC {
            return None;
        }
        let n = u32::from_le_bytes(head[8..12].try_into().ok()?) as usize;
        let d = u32::from_le_bytes(head[12..16].try_into().ok()?) as usize;
        let every = f32::from_le_bytes(head[16..20].try_into().ok()?);
        if d != dim {
            return None;
        }
        let mut raw = Vec::new();
        f.read_to_end(&mut raw).ok()?;
        if raw.len() != n * 4 + n * d * 4 {
            return None;
        }
        let fl: Vec<f32> = raw.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
        Some(Index { every, times: fl[..n].to_vec(), vecs: fl[n..].to_vec() })
    }

    fn save_index(path: &std::path::Path, ix: &Index, dim: usize) -> std::io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("part");
        let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        f.write_all(MAGIC)?;
        f.write_all(&(ix.times.len() as u32).to_le_bytes())?;
        f.write_all(&(dim as u32).to_le_bytes())?;
        f.write_all(&ix.every.to_le_bytes())?;
        for v in ix.times.iter().chain(ix.vecs.iter()) {
            f.write_all(&v.to_le_bytes())?;
        }
        f.flush()?;
        drop(f);
        std::fs::rename(tmp, path)
    }

    type Cache = Mutex<Option<(PathBuf, Arc<Embedder>)>>;
    fn embedder(cmd: &str) -> Result<Arc<Embedder>> {
        static C: OnceLock<Cache> = OnceLock::new();
        let dir = require(&catalogue::SIGLIP2, cmd)?;
        let mut c = C.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
        if let Some((d, e)) = c.as_ref()
            && *d == dir
        {
            return Ok(e.clone());
        }
        let e = Arc::new(Embedder::load(&dir).map_err(|e| bad(cmd, e.to_string()))?);
        *c = Some((dir, e.clone()));
        Ok(e)
    }

    fn video_media(s: &Session, p: &Value) -> Vec<(ItemId, String, String)> {
        let wanted: Option<Vec<u64>> = p.get("items").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).collect());
        s.project
            .items
            .iter()
            .filter(|(id, _)| wanted.as_ref().is_none_or(|w| w.contains(&id.0)))
            .filter_map(|(id, it)| match &it.kind {
                ItemKind::Media(m) if m.info.video.is_some() && !m.offline => match &m.media {
                    MediaRef::File { path } => Some((*id, it.name.clone(), path.clone())),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    pub fn index(s: &mut Session, p: &Value) -> Result<Value> {
        const CMD: &str = "media.rushIndex";
        let every = f64_p(p, "every").unwrap_or(2.0);
        if !(0.2..=60.0).contains(&every) {
            return Err(bad(CMD, "`every` must be between 0.2 and 60 seconds"));
        }
        let emb = embedder(CMD)?;
        let dir = index_dir().ok_or_else(|| bad(CMD, "no folder for the index"))?;
        let items = video_media(s, p);
        if items.is_empty() {
            return Err(bad(CMD, "no video media to index"));
        }
        // sources are opened here (the pool belongs to the session), the frames are read and embedded in the job
        let mut work = Vec::new();
        for (id, name, path) in items {
            let src = s.media.full_res_source(&s.project, id, &*s.services).ok_or_else(|| bad(CMD, format!("{name}: no source")))?;
            work.push((id, name, path, src));
        }
        let total: u64 = work.iter().map(|(_, _, _, src)| (src.info().duration.seconds() / every).floor() as u64 + 1).sum();
        let wait = bool_p(p, "wait").unwrap_or(true);
        crate::ml_tools::run_job(s, "Index rushes".into(), total, wait, move |prog| {
            use std::sync::atomic::Ordering;
            let t0 = std::time::Instant::now();
            let (mut done, mut reused, mut frames_total) = (0, 0, 0usize);
            let mut report = Vec::new();
            for (id, name, path, src) in work {
                if prog.cancel.load(Ordering::Relaxed) {
                    return Err("cancelled".into());
                }
                let Some(key) = media_key(&path, emb.id()) else { continue };
                let file = dir.join(format!("{key}.bin"));
                let n = ((src.info().duration.seconds() / every).floor() as usize + 1).min(100_000);
                if let Some(ix) = load_index(&file, emb.dim()).filter(|ix| (ix.every as f64 - every).abs() < 1e-6) {
                    reused += 1;
                    frames_total += ix.times.len();
                    prog.done.fetch_add(n as u64, Ordering::Relaxed);
                    report.push(json!({"item": id.0, "name": name, "frames": ix.times.len(), "reused": true}));
                    continue;
                }
                let dur = src.info().duration.seconds();
                let (w, h) = src.info().video.as_ref().map_or((0, 0), |v| (v.width, v.height));
                let scale = (448.0 / w.max(h).max(1) as f32).clamp(0.05, 1.0);
                let (mut times, mut vecs) = (Vec::new(), Vec::new());
                let mut batch: Vec<Vec<f32>> = Vec::new();
                let mut bt: Vec<f32> = Vec::new();
                let flush = |batch: &mut Vec<Vec<f32>>, bt: &mut Vec<f32>, times: &mut Vec<f32>, vecs: &mut Vec<f32>| -> std::result::Result<(), String> {
                    if batch.is_empty() {
                        return Ok(());
                    }
                    let e = emb.embed_images(batch).map_err(|e| e.to_string())?;
                    for v in e {
                        vecs.extend(v);
                    }
                    times.append(bt);
                    batch.clear();
                    Ok(())
                };
                for k in 0..n {
                    if prog.cancel.load(Ordering::Relaxed) {
                        return Err("cancelled".into());
                    }
                    let t = (k as f64 * every).min((dur - 0.04).max(0.0));
                    prog.done.fetch_add(1, Ordering::Relaxed);
                    let Ok(fr) = src.video_frame(FrameRequest { time: Tick::from_seconds_f64(t), scale }) else { continue };
                    let rgba = fr.to_rgba8();
                    let rgb: Vec<u8> = rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
                    if let Some(px) = filmcraft_mlsearch::preprocess::preprocess(&rgb, fr.width as usize, fr.height as usize, emb.resample()) {
                        batch.push(px);
                        bt.push(t as f32);
                    }
                    if batch.len() >= 16 {
                        flush(&mut batch, &mut bt, &mut times, &mut vecs)?;
                    }
                }
                flush(&mut batch, &mut bt, &mut times, &mut vecs)?;
                let ix = Index { every: every as f32, times, vecs };
                save_index(&file, &ix, emb.dim()).map_err(|e| format!("{}: {e}", file.display()))?;
                done += 1;
                frames_total += ix.times.len();
                report.push(json!({"item": id.0, "name": name, "frames": ix.times.len(), "reused": false}));
            }
            let secs = t0.elapsed().as_secs_f64();
            Ok(json!({
                "media": report, "indexed": done, "reused": reused, "frames": frames_total, "seconds": secs, "device": format!("{:?}", emb.device()),
                "summary": format!("{done} média indexé(s), {reused} déjà à jour, {frames_total} images au total ({secs:.1} s)"),
            }))
        })
    }

    pub fn search(s: &mut Session, p: &Value) -> Result<Value> {
        const CMD: &str = "media.rushSearch";
        let query = str_p(p, "query").map(str::trim).filter(|q| !q.is_empty()).ok_or_else(|| bad(CMD, "`query` is required"))?.to_string();
        let top = u64_p(p, "top").unwrap_or(8).clamp(1, 100) as usize;
        let emb = embedder(CMD)?;
        let dir = index_dir().ok_or_else(|| bad(CMD, "no folder for the index"))?;
        let q = emb.embed_text(&query.to_lowercase()).map_err(|e| bad(CMD, e.to_string()))?;
        let mut hits: Vec<(f32, ItemId, String, f32, f32)> = Vec::new();
        let mut searched = 0;
        let mut frames = 0;
        for (id, name, path) in video_media(s, p) {
            let Some(key) = media_key(&path, emb.id()) else { continue };
            let Some(ix) = load_index(&dir.join(format!("{key}.bin")), emb.dim()) else { continue };
            searched += 1;
            frames += ix.times.len();
            for (i, t) in ix.times.iter().enumerate() {
                let v = &ix.vecs[i * emb.dim()..(i + 1) * emb.dim()];
                let c: f32 = v.iter().zip(&q).map(|(a, b)| a * b).sum();
                hits.push((c, id, name.clone(), *t, ix.every));
            }
        }
        if searched == 0 {
            return Err(bad(CMD, "no indexed media: run media.rushIndex first"));
        }
        hits.sort_by(|a, b| b.0.total_cmp(&a.0));
        // one hit per moment: drop a hit that is within 3 index steps of a better one in the same media
        let mut kept: Vec<(f32, ItemId, String, f32, f32)> = Vec::new();
        for h in hits {
            if kept.iter().any(|k| k.1 == h.1 && (k.3 - h.3).abs() < 3.0 * h.4) {
                continue;
            }
            kept.push(h);
            if kept.len() >= top {
                break;
            }
        }
        if bool_p(p, "markers").unwrap_or(false) {
            let list = kept.clone();
            let q2 = query.clone();
            s.edit("Rush Search Markers", move |pr, _| {
                for (score, id, _, t, _) in &list {
                    let mid = MarkerId(pr.alloc_id());
                    if let Some(m) = pr.item_mut(*id).and_then(|i| i.as_media_mut()) {
                        let at = Tick::from_seconds_f64(*t as f64);
                        m.markers.push(Marker {
                            id: mid,
                            start: at,
                            duration: Tick::ZERO,
                            name: format!("{q2} ({score:.3})"),
                            comment: String::new(),
                            kind: MarkerKind::Comment,
                            color: Label::Iris,
                        });
                        m.markers.sort_by_key(|x| x.start);
                    }
                }
                Ok(())
            })?;
        }
        let results: Vec<Value> = kept
            .iter()
            .map(|(score, id, name, t, _)| {
                let rate = s.project.resolve_media(*id).map(|(_, m, _)| m.frame_rate()).unwrap_or_default();
                let tc = filmcraft_time::format_timecode_frames(rate.frame_at(Tick::from_seconds_f64(*t as f64)), rate, false);
                json!({"item": id.0, "name": name, "seconds": t, "timecode": tc, "score": score})
            })
            .collect();
        let best = results
            .first()
            .map(|r| format!("{} à {} ({:.3})", r["name"].as_str().unwrap_or(""), r["timecode"].as_str().unwrap_or(""), r["score"].as_f64().unwrap_or(0.0)))
            .unwrap_or_default();
        Ok(
            json!({"query": query, "results": results, "searchedMedia": searched, "searchedFrames": frames, "summary": format!("« {query} » : {} résultat(s) sur {frames} images, meilleur : {best}", results.len())}),
        )
    }
}

fn index(s: &mut Session, p: &Value) -> Result<Value> {
    #[cfg(feature = "ml")]
    return on::index(s, p);
    #[cfg(not(feature = "ml"))]
    {
        let _ = (s, p);
        Err(no_ml("media.rushIndex"))
    }
}

fn search(s: &mut Session, p: &Value) -> Result<Value> {
    #[cfg(feature = "ml")]
    return on::search(s, p);
    #[cfg(not(feature = "ml"))]
    {
        let _ = (s, p);
        Err(no_ml("media.rushSearch"))
    }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "media.rushIndex",
            label: "Index Rushes for Search…",
            menu: &[],
            shortcut: None,
            params: r#"{"items":[id]? (video media; default: all),"every":seconds=2 (one frame every N seconds),"wait":bool=true (false: a background job)}"#,
            enabled: crate::commands::always,
            run: index,
            journal: false,
        },
        CommandSpec {
            id: "media.rushSearch",
            label: "Search Rushes…",
            menu: &[],
            shortcut: None,
            params: r#"{"query":str (what you see, French or English),"top":n=8,"items":[id]? (default: all indexed),"markers":bool=false (a marker on the media at each hit)}"#,
            enabled: crate::commands::always,
            run: search,
            journal: false,
        },
    ]
}
