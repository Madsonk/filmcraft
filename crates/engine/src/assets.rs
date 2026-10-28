//! Resources dropped on the timeline (assets-dnd): the user's own LUTs, sounds, music and title
//! videos, from the Finder or from Window ▸ Resources.
//!
//! `assets.place` routes each file by kind, from where it was dropped:
//!
//! - **LUT** (`.cube`, `.3dl`): on a video clip → that clip's Lumetri **Creative Look**
//!   (`lumetri.setLook`); anywhere else → the project's LUT library (`lut.import`).
//! - **Sound** (WAV, AIFF, MP3, M4A, FLAC, Ogg, Opus, CAF): imported, then placed at the drop time
//!   on the audio track dropped on if it is free there, else on the next free audio track below,
//!   else on a new audio track. Dropped on a video clip or track: the first free audio track.
//! - **Picture** (video, still, a title rendered with transparency): imported, then placed at the
//!   drop time on the video track dropped on if it is free there, else on the next free video track
//!   **above** (a title dropped on a shot goes over it), else on a new video track.
//! - **`.mogrt`** made in **Premiere**: converted once into a native graphics template
//!   (`graphics.template.importMogrt`, editable text, colours, fonts) and placed like a title, on
//!   a free video track above. One made in **After Effects** is refused ("preview only"): it would
//!   need After Effects.
//!
//! A drop never overwrites a clip already on the timeline, and all its steps (import, new track,
//! placement) are one undo step. Nothing is written to the dropped files.

use filmcraft_project::{ClipId, ItemId, TrackId, TrackKind};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, clip_p, str_p, time_p, track_p};
use crate::{Result, Session};

const CMD: &str = "assets.place";

/// What a dropped file is, from its extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetKind {
    Lut,
    Sound,
    Picture,
    Mogrt,
    Other,
}

const SOUND_EXT: &[&str] = &["wav", "wave", "aif", "aiff", "aifc", "mp3", "m4a", "aac", "flac", "ogg", "oga", "opus", "caf"];
const PICTURE_EXT: &[&str] =
    &["mov", "mp4", "m4v", "mxf", "mkv", "webm", "avi", "mts", "m2ts", "png", "jpg", "jpeg", "tif", "tiff", "exr", "dpx", "gif", "webp", "heic", "bmp", "psd"];

impl AssetKind {
    pub fn of(path: &str) -> AssetKind {
        let ext = std::path::Path::new(path).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        match ext.as_str() {
            "cube" | "3dl" => AssetKind::Lut,
            "mogrt" => AssetKind::Mogrt,
            e if SOUND_EXT.contains(&e) => AssetKind::Sound,
            e if PICTURE_EXT.contains(&e) => AssetKind::Picture,
            _ => AssetKind::Other,
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            AssetKind::Lut => "lut",
            AssetKind::Sound => "sound",
            AssetKind::Picture => "picture",
            AssetKind::Mogrt => "mogrt",
            AssetKind::Other => "other",
        }
    }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: CMD,
        label: "Place Resources",
        menu: &[],
        shortcut: None,
        params: r#"{"paths":[str],"clip":id?,"track":id|"V1"|"A1"?,"newTrack":{"kind":"video"|"audio","index":n}?,"time":ticks|"seconds":f64|"frame":i64?,"bin":binId?}"#,
        enabled: always,
        run: place,
        journal: true,
    }]
}

/// First track of `kind`, from index `from` in the direction away from V1/A1 (up for video, down
/// for audio: higher indices), that is unlocked and free over `[t, t + d)`.
fn free_track(s: &Session, kind: TrackKind, from: usize, t: Tick, d: Tick) -> Option<TrackId> {
    let q = s.active_sequence()?;
    let tracks = if kind == TrackKind::Video { &q.video_tracks } else { &q.audio_tracks };
    let end = Tick(t.0.saturating_add(d.0.max(1)));
    tracks.iter().skip(from).find(|tr| !tr.locked && tr.items.iter().all(|it| it.end() <= t || it.start >= end)).map(|tr| tr.id)
}

/// The track a clip sits on, its kind and index.
fn clip_track(s: &Session, clip: ClipId) -> Option<(TrackId, TrackKind, usize)> {
    let q = s.active_sequence()?;
    let (tid, _) = q.find_item(clip)?;
    let tr = q.track(tid)?;
    let list = if tr.kind == TrackKind::Video { &q.video_tracks } else { &q.audio_tracks };
    let idx = list.iter().position(|x| x.id == tid)?;
    Some((tid, tr.kind, idx))
}

fn track_info(s: &Session, t: TrackId) -> Option<(TrackKind, usize)> {
    let q = s.active_sequence()?;
    let tr = q.track(t)?;
    let list = if tr.kind == TrackKind::Video { &q.video_tracks } else { &q.audio_tracks };
    Some((tr.kind, list.iter().position(|x| x.id == t)?))
}

/// The track a resource of `kind` goes on: the hovered track of the right kind when it is free,
/// else the next free one, else a new one.
fn destination(s: &mut Session, kind: TrackKind, hovered: Option<(TrackKind, usize)>, t: Tick, d: Tick) -> Result<TrackId> {
    let from = match hovered {
        Some((k, i)) if k == kind => i,
        _ => 0,
    };
    if let Some(id) = free_track(s, kind, from, t, d) {
        return Ok(id);
    }
    let (v, a) = if kind == TrackKind::Video { (1, 0) } else { (0, 1) };
    let r = s.execute("sequence.addTracks", json!({"video": v, "audio": a}))?;
    let key = if kind == TrackKind::Video { "video" } else { "audio" };
    r.get(key).and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_u64).map(TrackId).ok_or_else(|| bad(CMD, "a new track could not be added"))
}

/// `newTrack {kind, index}`: a drop in the zone above the top video track, below the last audio
/// track or between two tracks makes a new track there (index = how many tracks of that kind come
/// before it), in the same undo step as the placement.
/// Where the first placed clip really starts: `timeline.place` puts clips on the sequence's frame grid,
/// so this can be a few ticks before the asked time `asked`.
fn placed_at(s: &Session, placed: &Value, asked: Tick) -> Tick {
    placed["clips"][0].as_u64().and_then(|c| s.active_sequence().and_then(|q| q.find_item(ClipId(c)).map(|(_, it)| it.start))).unwrap_or(asked)
}

pub(crate) fn new_track_p(p: &Value) -> Result<Option<(TrackKind, usize)>> {
    let Some(v) = p.get("newTrack").filter(|v| !v.is_null()) else { return Ok(None) };
    let kind = match v.get("kind").and_then(Value::as_str) {
        Some("video") => TrackKind::Video,
        Some("audio") => TrackKind::Audio,
        _ => return Err(bad(CMD, "`newTrack.kind` must be \"video\" or \"audio\"")),
    };
    let index = v.get("index").and_then(Value::as_u64).ok_or_else(|| bad(CMD, "`newTrack.index` must be a number"))? as usize;
    Ok(Some((kind, index)))
}

/// Make the requested new track (once) and return its id when `kind` is the one asked for.
fn new_track(s: &mut Session, wanted: Option<(TrackKind, usize)>, made: &mut Option<TrackId>, kind: TrackKind) -> Result<Option<TrackId>> {
    let Some((k, index)) = wanted.filter(|(k, _)| *k == kind) else { return Ok(None) };
    if let Some(id) = made {
        return Ok(Some(*id));
    }
    let id = add_track_at(s, k, index)?;
    *made = Some(id);
    Ok(Some(id))
}

/// Add one track of `kind` with `index` tracks of that kind before it (clamped). Its id.
pub(crate) fn add_track_at(s: &mut Session, kind: TrackKind, index: usize) -> Result<TrackId> {
    let len = s.active_sequence().map(|q| if kind == TrackKind::Video { q.video_tracks.len() } else { q.audio_tracks.len() }).unwrap_or(0);
    let q = if kind == TrackKind::Video {
        json!({"video": 1, "audio": 0, "videoAfter": index.min(len)})
    } else {
        json!({"video": 0, "audio": 1, "audioAfter": index.min(len)})
    };
    let r = s.execute("sequence.addTracks", q)?;
    let key = if kind == TrackKind::Video { "video" } else { "audio" };
    r.get(key).and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_u64).map(TrackId).ok_or_else(|| bad(CMD, "a new track could not be added"))
}

fn import_one(s: &mut Session, path: &str, bin: Option<&Value>) -> Result<ItemId> {
    let mut q = json!({"paths": [path]});
    if let Some(b) = bin {
        q["bin"] = b.clone();
    }
    let r = s.execute("file.import", q)?;
    r.get("items").and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_u64).map(ItemId).ok_or_else(|| {
        let why = r.get("errors").and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_str).unwrap_or("not a media file");
        bad(CMD, format!("{path}: {why}"))
    })
}

fn name_of(path: &str) -> String {
    std::path::Path::new(path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string())
}

fn place(s: &mut Session, p: &Value) -> Result<Value> {
    let paths: Vec<String> = match p.get("paths").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        None => str_p(p, "path").map(|x| vec![x.to_string()]).unwrap_or_default(),
    };
    if paths.is_empty() {
        return Err(bad(CMD, "need `paths`"));
    }
    let clip = clip_p(p, "clip");
    let has_seq = s.active_sequence().is_some();
    let hovered_clip = clip.filter(|_| has_seq).and_then(|c| clip_track(s, c));
    if clip.is_some() && has_seq && hovered_clip.is_none() {
        return Err(bad(CMD, "no such clip in the active sequence"));
    }
    let track = if has_seq { track_p(s, p, "track", CMD)? } else { None };
    let hovered = hovered_clip.map(|(_, k, i)| (k, i)).or_else(|| track.and_then(|t| track_info(s, t)));
    let time = if has_seq { time_p(s, p, "") } else { None };
    // a clip under the pointer gives its own start when no time is given
    let time = time.or_else(|| clip.and_then(|c| s.active_sequence()?.find_item(c).map(|(_, it)| it.start)));
    let bin = p.get("bin").filter(|b| !b.is_null());
    let wanted_track = if has_seq { new_track_p(p)? } else { None };
    let mut made_track: Option<TrackId> = None;

    let n0 = s.history.undo.len();
    let mut results = Vec::new();
    let mut refused = Vec::new();
    let mut first_error = None;
    let mut summary = Vec::new();
    for path in &paths {
        let kind = AssetKind::of(path);
        let r: Result<Value> = (|| match kind {
            AssetKind::Mogrt => {
                let (entry, info) = crate::graphic_templates::import_mogrt(s, path)?;
                let id = entry.template.id.clone();
                let (Some(t), true) = (time, has_seq) else {
                    summary.push(format!("Title “{}” converted into Graphics Templates", entry.template.name));
                    return Ok(json!({"path": path, "kind": "mogrt", "action": "converted", "template": id, "conversion": info}));
                };
                let v = match new_track(s, wanted_track, &mut made_track, TrackKind::Video)? {
                    Some(v) => v,
                    None => destination(s, TrackKind::Video, hovered, t, entry.template.duration)?,
                };
                let index = s.active_sequence().and_then(|q| q.video_tracks.iter().position(|x| x.id == v)).unwrap_or(0);
                let r = s.execute("graphics.template.apply", json!({"template": id, "time": t.0, "track": index}))?;
                summary.push(format!("Title “{}” placed on a video track", entry.template.name));
                Ok(json!({"path": path, "kind": "mogrt", "action": "placed", "template": id, "clip": r["clip"], "track": v.0, "time": t.0, "conversion": info}))
            }
            AssetKind::Lut => match (hovered_clip, clip) {
                (Some((_, TrackKind::Video, _)), Some(c)) => {
                    let r = s.execute("lumetri.setLook", json!({"clip": c.0, "path": path}))?;
                    summary.push(format!("Look “{}” on clip {}", name_of(path), c.0));
                    Ok(json!({"path": path, "kind": "lut", "action": "look", "clip": c.0, "result": r}))
                }
                (Some((_, TrackKind::Audio, _)), _) => Err(bad(CMD, "a LUT goes on a video clip, not on a sound")),
                _ => {
                    let r = s.execute("lut.import", json!({"path": path}))?;
                    summary.push(format!("LUT “{}” added to the project's LUT library", name_of(path)));
                    Ok(json!({"path": path, "kind": "lut", "action": "library", "lut": r.get("ref").cloned().unwrap_or(Value::Null)}))
                }
            },
            AssetKind::Sound | AssetKind::Picture | AssetKind::Other => {
                let item = import_one(s, path, bin)?;
                let (Some(t), true) = (time, has_seq) else {
                    summary.push(format!("“{}” imported", name_of(path)));
                    return Ok(json!({"path": path, "kind": kind.id(), "action": "imported", "item": item.0}));
                };
                let (has_v, has_a, dur) = {
                    let it = s.project.item(item).ok_or_else(|| bad(CMD, "imported item vanished"))?;
                    let d = if it.duration().0 > 0 { it.duration() } else { s.prefs.timeline.still_duration(s.sequence_rate()) };
                    (it.has_video(), it.has_audio(), d)
                };
                if has_v {
                    let v = match new_track(s, wanted_track, &mut made_track, TrackKind::Video)? {
                        Some(v) => v,
                        None => destination(s, TrackKind::Video, hovered, t, dur)?,
                    };
                    let mut q = json!({"item": item.0, "track": v.0, "time": t.0, "audio": false});
                    if has_a {
                        // the picture's own sound goes on a free audio track at the same time
                        let a = match new_track(s, wanted_track, &mut made_track, TrackKind::Audio)? {
                            Some(a) => a,
                            None => destination(s, TrackKind::Audio, None, t, dur)?,
                        };
                        q = json!({"item": item.0, "track": v.0, "audioTrack": a.0, "time": t.0});
                    }
                    let placed = s.execute("timeline.place", q)?;
                    summary.push(format!("“{}” placed on a video track", name_of(path)));
                    Ok(
                        json!({"path": path, "kind": kind.id(), "action": "placed", "item": item.0, "track": v.0, "time": placed_at(s, &placed, t).0, "clips": placed["clips"]}),
                    )
                } else if has_a {
                    let a = match new_track(s, wanted_track, &mut made_track, TrackKind::Audio)? {
                        Some(a) => a,
                        None => destination(s, TrackKind::Audio, hovered, t, dur)?,
                    };
                    let placed = s.execute("timeline.place", json!({"item": item.0, "audioTrack": a.0, "time": t.0, "video": false}))?;
                    summary.push(format!("“{}” placed on an audio track", name_of(path)));
                    Ok(
                        json!({"path": path, "kind": kind.id(), "action": "placed", "item": item.0, "track": a.0, "time": placed_at(s, &placed, t).0, "clips": placed["clips"]}),
                    )
                } else {
                    Err(bad(CMD, format!("{path}: neither picture nor sound")))
                }
            }
        })();
        match r {
            Ok(v) => results.push(v),
            Err(e) => {
                let code = match kind {
                    AssetKind::Mogrt if e.to_string().contains("After Effects") => "afterEffects",
                    AssetKind::Mogrt => "mogrt",
                    AssetKind::Lut if hovered_clip.is_some() => "lutOnSound",
                    _ => "failed",
                };
                refused.push(json!({"path": path, "kind": kind.id(), "code": code, "message": e.to_string()}));
                first_error.get_or_insert(e);
            }
        }
    }
    crate::clip_ops::collapse_history(s, n0, "Place Resources");
    if results.is_empty()
        && refused.len() == 1
        && let Some(e) = first_error
    {
        return Err(e);
    }
    Ok(json!({"placed": results, "refused": refused, "summary": summary.join("; ")}))
}

/// A frame of a clip as its Lumetri Creative Look slot sees it (look switched off), in the display
/// signal (sRGB-encoded Rec.709, unpremultiplied), at most `max_w` pixels wide: the base of the
/// Resources panel's LUT preview, which applies each LUT to these pixels on the CPU.
#[derive(Clone, Debug, PartialEq)]
pub struct SignalFrame {
    pub width: usize,
    pub height: usize,
    /// RGB signal + alpha per pixel.
    pub px: Vec<[f32; 4]>,
}

/// Render [`SignalFrame`] for `clip` at sequence time `at` (clamped into the clip).
pub fn look_base_frame(s: &Session, clip: ClipId, at: Tick, max_w: usize) -> Option<SignalFrame> {
    let seq = s.state.active_sequence?;
    let q = s.active_sequence()?;
    let (tid, it) = q.find_item(clip)?;
    if q.track(tid).is_none_or(|t| t.kind != TrackKind::Video) || it.duration <= Tick::ZERO {
        return None;
    }
    let t = Tick(at.0.clamp(it.start.0, (it.end().0 - 1).max(it.start.0)));
    let mut project: filmcraft_project::Project = (*s.project).clone();
    if let Some((_, it)) = project.sequence_mut(seq).and_then(|q| q.find_item_mut(clip))
        && let Some(e) = it.effects.iter_mut().find(|e| e.effect == "lumetri")
    {
        let v = filmcraft_project::ParamValue::Float(0.0);
        let prm = e.params.entry("look_intensity".to_string()).or_insert_with(|| filmcraft_project::Param::new(v.clone()));
        prm.keyframes.clear();
        prm.value = v;
    }
    let provider = s.media.provider(std::sync::Arc::new(project.clone()), s.services.clone());
    let opts = filmcraft_render::RenderOptions { scale: 0.25, working_output: true, ..Default::default() };
    let img = filmcraft_render::render_clip(&project, seq, clip, t, opts, &provider)?;
    let img = filmcraft_render::color_match::shrink(&img, max_w.max(16));
    let gs = filmcraft_color::GradeSpace::new(filmcraft_color::WorkingSpace::Rec709, filmcraft_color::grade::DEFAULT_HDR_WHITE_NITS);
    let px = img
        .px
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| {
            if p[3] > 1e-4 {
                let c = gs.encode([p[0] / p[3], p[1] / p[3], p[2] / p[3]]);
                [c[0], c[1], c[2], p[3]]
            } else {
                [0.0; 4]
            }
        })
        .collect();
    Some(SignalFrame { width: img.w, height: img.h, px })
}

/// `frame` with `lut` applied (strength 1), as 8-bit RGBA for a texture.
pub fn apply_look_rgba8(frame: &SignalFrame, lut: Option<&filmcraft_color::Lut>) -> Vec<u8> {
    let mut out = Vec::with_capacity(frame.px.len() * 4);
    for p in &frame.px {
        let c = match lut {
            Some(l) => l.apply([p[0], p[1], p[2]]),
            None => [p[0], p[1], p[2]],
        };
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        // shown over the panel: transparent parts of a title stay dark
        out.extend_from_slice(&[q(c[0] * p[3]), q(c[1] * p[3]), q(c[2] * p[3]), 255]);
    }
    out
}

/// The clip a LUT preview applies to: the selected video clip, else the topmost enabled video
/// clip under the playhead.
pub fn preview_clip(s: &Session) -> Option<ClipId> {
    let q = s.active_sequence()?;
    let is_video = |c: ClipId| q.find_item(c).and_then(|(t, _)| q.track(t)).is_some_and(|t| t.kind == TrackKind::Video);
    if let Some(c) = s.state.selection.iter().copied().find(|c| is_video(*c)) {
        return Some(c);
    }
    let t = s.playhead();
    q.video_tracks.iter().rev().filter(|tr| tr.enabled).find_map(|tr| tr.items.iter().find(|it| it.start <= t && t < it.end()).map(|it| it.id))
}
