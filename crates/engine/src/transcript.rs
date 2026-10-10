//! Text-based editing commands (`transcript.*`): the Text panel ▸ Transcript tab.
//!
//! Transcripts belong to media items (`Project::transcripts`, media time); the sequence transcript
//! is derived from them ([`filmcraft_edit::transcript::sequence_words`]). Words of the sequence
//! transcript are addressed by index (`from`, `to`, inclusive), as `transcript.inspect` lists them.
//!
//! Speech recognition goes through a [`Transcriber`]: [`Session::transcriber`] when a host or a
//! test installed one, else the Whisper model named by `model` from `<data dir>/models` (needs the
//! engine feature `whisper`; without it `transcript.generate` fails with a clear error, and agents
//! can still bring their own transcript with `transcript.set`).

use std::sync::Arc;

use serde_json::{Value, json};

use filmcraft_edit as edit;
use filmcraft_edit::transcript::{self as tx, CaptionRules, SeqWord};
use filmcraft_project::{CaptionFormat, CaptionTrack, ItemId, ItemKind, TrackId, Transcript};
use filmcraft_speech::{Options, SpeechError, Transcriber};
use filmcraft_time::{TICKS_PER_SECOND, Tick, TimeRange};

use crate::commands::{CommandSpec, always, bad, bool_p, f64_p, has_seq, str_p, u64_p};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

fn spec(id: &'static str, label: &'static str, menu: &'static [&'static str], params: &'static str, enabled: Enabled, run: Run, journal: bool) -> CommandSpec {
    CommandSpec { id, label, menu, shortcut: None, params, enabled, run, journal }
}

/// Where downloaded speech models live (`<data dir>/models`).
pub fn models_dir() -> Option<std::path::PathBuf> {
    crate::autosave::default_data_dir().map(|d| d.join("models"))
}

/// Whether this build can transcribe with Whisper (feature `whisper`).
pub fn speech_available() -> bool {
    filmcraft_speech::available()
}

/// The words of the active sequence's transcript.
pub fn sequence_words(s: &Session) -> Vec<SeqWord> {
    match s.active_sequence() {
        Some(q) => tx::sequence_words(q, &s.project.transcripts),
        None => Vec::new(),
    }
}

pub(crate) fn has_transcript(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if sequence_words(s).is_empty() { Err("the sequence has no transcript (Transcribe first)".into()) } else { Ok(()) }
}

fn has_transcripts(s: &Session) -> std::result::Result<(), String> {
    if s.project.transcripts.is_empty() { Err("there are no transcripts".into()) } else { Ok(()) }
}

/// Why speech-to-text can't run in this build (no installed transcriber, built without `whisper`).
pub(crate) const NO_SPEECH: &str =
    "speech-to-text is not available in this build (built without the `whisper` feature); import a transcript with transcript.set instead";

/// `transcript.generate` can run: a host installed a transcriber or the build has speech-to-text
/// (#97: it reported enabled and then always failed).
pub(crate) fn can_transcribe(s: &Session) -> std::result::Result<(), String> {
    if s.transcriber.is_some() || speech_available() { Ok(()) } else { Err(NO_SPEECH.into()) }
}

/// `transcript.downloadModel` can run: built with `speech-download` (#98).
fn can_download(_: &Session) -> std::result::Result<(), String> {
    if cfg!(feature = "speech-download") {
        Ok(())
    } else {
        Err("model downloads are not available in this build (built without the `speech-download` feature)".into())
    }
}

/// The media item behind a project item (subclips resolve to their parent).
fn media_item(s: &Session, item: ItemId) -> Option<ItemId> {
    s.project.resolve_media(item).map(|(root, _, _)| root)
}

fn ids_p(p: &Value, k: &str) -> Option<Vec<ItemId>> {
    p.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).map(ItemId).collect())
}

/// Items to transcribe: `items` / `item`, else the Project panel selection, else the media of the
/// active sequence's enabled audio clips. Subclips resolve to their media; duplicates are removed.
fn targets(s: &Session, p: &Value) -> Vec<ItemId> {
    let mut raw = ids_p(p, "items").or_else(|| u64_p(p, "item").map(|i| vec![ItemId(i)])).unwrap_or_default();
    if raw.is_empty() {
        raw = s.state.project_selection.clone();
    }
    if raw.is_empty()
        && let Some(q) = s.active_sequence()
    {
        raw = q.audio_tracks.iter().flat_map(|t| t.items.iter()).filter(|it| it.enabled).map(|it| it.item).collect();
    }
    let mut out = Vec::new();
    for i in raw {
        if let Some(m) = media_item(s, i)
            && !out.contains(&m)
        {
            out.push(m);
        }
    }
    out
}

fn speech_err(e: SpeechError) -> EngineError {
    EngineError::Other(e.to_string())
}

/// Where the speech model comes from: one a host installed, or a catalogue model loaded when the job runs
/// (loading takes a second or more, so it is part of the slow step, not of the preparation).
enum Model {
    Ready(Arc<dyn Transcriber>),
    Load { dir: std::path::PathBuf, model: String },
}

/// Transcription in three parts, so that a host can run the slow middle one off its UI thread:
/// [`begin_transcribe`] (validates, reads the session; fast) → [`TranscribeJob::run`] (reads the audio,
/// loads the model, recognises speech; slow, needs no session, reports progress and can be cancelled) →
/// the `transcript.generate` command, which uses the output when [`Session::finished_transcription`]
/// holds it (and recognises on the spot otherwise: the CLI, scripts, MCP).
pub struct TranscribeJob {
    model: Model,
    opts: Options,
    /// (media item, its source and length in speech samples; `None`: no audio).
    inputs: Vec<(ItemId, Option<(filmcraft_media::SharedSource, usize)>)>,
}

/// What [`TranscribeJob::run`] recognised, for the items it was started with.
pub struct TranscribeOutput {
    items: Vec<ItemId>,
    done: Vec<(ItemId, Transcript)>,
    skipped: Vec<u64>,
}

impl TranscribeJob {
    /// The media items the job transcribes (with or without audio).
    pub fn items(&self) -> usize {
        self.inputs.len()
    }

    /// Recognise the speech. `progress(fraction 0…1, what)` returns false to cancel.
    pub fn run(self, progress: &mut dyn FnMut(f32, &str) -> bool) -> Result<TranscribeOutput> {
        let t = match self.model {
            Model::Ready(t) => t,
            Model::Load { dir, model } => {
                if !progress(0.0, "Chargement du modèle de reconnaissance vocale") {
                    return Err(EngineError::Other("transcription annulée".into()));
                }
                filmcraft_speech::load(&dir, &model).map_err(speech_err)?
            }
        };
        let n = self.inputs.len().max(1) as f32;
        let items: Vec<ItemId> = self.inputs.iter().map(|i| i.0).collect();
        let (mut done, mut skipped) = (Vec::new(), Vec::new());
        for (k, (item, src)) in self.inputs.into_iter().enumerate() {
            let audio = src.and_then(|(src, len)| {
                let buf = src.audio(0, len, filmcraft_speech::SAMPLE_RATE).ok()?;
                Some(filmcraft_speech::downmix(&buf.channels))
            });
            let Some(audio) = audio else {
                skipped.push(item.0);
                continue;
            };
            let base = k as f32 / n;
            let mut tr =
                t.transcribe(&audio, &self.opts, &mut |f, what| progress((base + f.clamp(0.0, 1.0) / n).clamp(0.0, 1.0), what)).map_err(|e| match e {
                    SpeechError::Cancelled => EngineError::Other("transcription annulée".into()),
                    e => speech_err(e),
                })?;
            tr.normalize();
            done.push((item, tr));
        }
        Ok(TranscribeOutput { items, done, skipped })
    }
}

/// Validate a `transcript.generate` call and gather what the slow step needs.
pub fn begin_transcribe(s: &Session, p: &Value) -> Result<TranscribeJob> {
    let items = targets(s, p);
    if items.is_empty() {
        return Err(bad("transcript.generate", "nothing to transcribe (pass `items`, select clips, or open a sequence with audio)"));
    }
    let model = match &s.transcriber {
        Some(t) => Model::Ready(t.clone()),
        None => {
            // Settings ▸ Media Analysis & Transcription ▸ Speech model
            let name = str_p(p, "model").unwrap_or(&s.prefs.media_analysis.whisper_model).to_string();
            if filmcraft_speech::models::find(&name).is_none() {
                return Err(speech_err(SpeechError::UnknownModel(name)));
            }
            if !filmcraft_speech::available() {
                return Err(EngineError::Other(NO_SPEECH.into()));
            }
            let dir = models_dir().ok_or_else(|| EngineError::Other("no data directory for speech models".into()))?;
            Model::Load { dir, model: name }
        }
    };
    // Settings ▸ Media Analysis & Transcription: language (or auto-detect) and speaker labelling
    let ma = &s.prefs.media_analysis;
    let default_language = if ma.language_auto_detect { None } else { Some(ma.default_language.clone()) };
    let opts = Options {
        language: match str_p(p, "language") {
            Some(l) => Some(l).filter(|l| !l.is_empty() && *l != "auto").map(str::to_string),
            None => default_language,
        },
        diarize: bool_p(p, "diarize").unwrap_or(ma.speaker_labeling != "off"),
        max_speakers: u64_p(p, "maxSpeakers").map(|n| n.clamp(1, 32) as usize).unwrap_or(Options::default().max_speakers),
        prompt: str_p(p, "prompt").map(str::trim).filter(|t| !t.is_empty()).map(str::to_string),
        hesitations: bool_p(p, "hesitations").unwrap_or(false),
        hesitation_bias: f64_p(p, "hesitationBias").map(|v| v.clamp(0.0, 12.0) as f32).unwrap_or(0.0),
    };
    let sr = filmcraft_speech::SAMPLE_RATE;
    let inputs = items
        .into_iter()
        .map(|item| {
            let src = match s.project.item(item).map(|i| &i.kind) {
                Some(ItemKind::Media(m)) => {
                    s.source(item).filter(|src| src.info().has_audio()).map(|src| (src, m.duration().to_units_floor(sr as i64).max(0) as usize))
                }
                _ => None,
            };
            (item, src)
        })
        .collect();
    Ok(TranscribeJob { model, opts, inputs })
}

/// Which command a host may run in the background, and with which parameters: `transcript.generate`
/// as called, `sequence.transcribe` through the same translation the command does.
pub fn async_transcribe_params(s: &Session, id: &str, p: &Value) -> Option<Result<Value>> {
    match id {
        "transcript.generate" => Some(Ok(p.clone())),
        "sequence.transcribe" => Some(crate::sequence_extras::transcribe_params(s, p)),
        _ => None,
    }
}

fn generate(s: &mut Session, p: &Value) -> Result<Value> {
    let job = begin_transcribe(s, p)?;
    let wanted: Vec<ItemId> = job.inputs.iter().map(|i| i.0).collect();
    // a host already ran the slow step for exactly these items
    let out = match s.finished_transcription.take() {
        Some(o) if o.items == wanted => o,
        _ => job.run(&mut |_, _| true)?,
    };
    let TranscribeOutput { done, skipped, .. } = out;
    if done.is_empty() {
        return Err(EngineError::Other("none of the clips has audio to transcribe".into()));
    }
    let report: Vec<Value> = done
        .iter()
        .map(|(i, t)| json!({"item": i.0, "words": t.words.len(), "speakers": t.speakers.len(), "language": t.language, "source": t.source}))
        .collect();
    s.edit("Transcribe", move |pr, _| {
        for (i, t) in done {
            pr.transcripts.insert(i, Arc::new(t));
        }
        Ok(())
    })?;
    Ok(json!({"items": report, "skipped": skipped}))
}

fn set(s: &mut Session, p: &Value) -> Result<Value> {
    let item = u64_p(p, "item").map(ItemId).ok_or_else(|| bad("transcript.set", "`item` is required"))?;
    let item = media_item(s, item).ok_or_else(|| bad("transcript.set", "no such media item"))?;
    let v = p.get("transcript").cloned().ok_or_else(|| bad("transcript.set", "`transcript` is required"))?;
    let mut t: Transcript = serde_json::from_value(v).map_err(|e| bad("transcript.set", e.to_string()))?;
    if t.source.is_empty() {
        t.source = "imported".into();
    }
    t.normalize();
    t.check().map_err(|e| bad("transcript.set", e))?;
    let n = t.words.len();
    s.edit("Set Transcript", move |pr, _| {
        pr.transcripts.insert(item, Arc::new(t));
        Ok(())
    })?;
    Ok(json!({"item": item.0, "words": n}))
}

fn delete(s: &mut Session, p: &Value) -> Result<Value> {
    let items: Vec<ItemId> = match ids_p(p, "items").or_else(|| u64_p(p, "item").map(|i| vec![ItemId(i)])) {
        Some(v) => v.into_iter().filter_map(|i| media_item(s, i)).collect(),
        None => s.project.transcripts.keys().copied().collect(),
    };
    let n = items.iter().filter(|i| s.project.transcripts.contains_key(i)).count();
    if n == 0 {
        return Err(EngineError::Other("no transcript to delete".into()));
    }
    s.edit("Delete Transcript", move |pr, _| {
        for i in items {
            pr.transcripts.remove(&i);
        }
        Ok(())
    })?;
    Ok(json!({"deleted": n}))
}

fn word_json(i: usize, w: &SeqWord) -> Value {
    json!({"i": i, "text": w.text, "start": w.start.0, "end": w.end.0, "speaker": w.speaker, "clip": w.clip.0, "item": w.item.0, "confidence": w.confidence})
}

fn inspect(s: &mut Session, p: &Value) -> Result<Value> {
    let words = sequence_words(s);
    let gap = Tick::from_seconds_f64(f64_p(p, "paragraphGapSeconds").unwrap_or(1.5));
    let paras: Vec<Value> = tx::paragraphs(&words, gap)
        .into_iter()
        .map(|r| {
            let text = words[r.clone()].iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
            json!({"from": r.start, "to": r.end - 1, "speaker": words[r.start].speaker, "start": words[r.start].start.0, "end": words[r.end - 1].end.0, "text": text})
        })
        .collect();
    let speakers: Vec<String> = {
        let mut v: Vec<String> = Vec::new();
        for w in &words {
            if let Some(n) = &w.speaker
                && !v.contains(n)
            {
                v.push(n.clone());
            }
        }
        v
    };
    let current = tx::word_at(&words, s.playhead());
    Ok(json!({
        "words": words.iter().enumerate().map(|(i, w)| word_json(i, w)).collect::<Vec<_>>(),
        "paragraphs": paras,
        "speakers": speakers,
        "current": current,
        "items": s.project.transcripts.iter().map(|(i, t)| json!({"item": i.0, "words": t.words.len(), "language": t.language, "source": t.source, "speakers": t.speakers.iter().map(|k| &k.name).collect::<Vec<_>>()})).collect::<Vec<_>>(),
    }))
}

fn search(s: &mut Session, p: &Value) -> Result<Value> {
    let q = str_p(p, "query").ok_or_else(|| bad("transcript.search", "`query` is required"))?;
    let words = sequence_words(s);
    let hits: Vec<Value> = tx::search(&words, q)
        .into_iter()
        .map(|r| json!({"from": r.start, "to": r.end - 1, "start": words[r.start].start.0, "end": words[r.end - 1].end.0}))
        .collect();
    Ok(json!({"matches": hits}))
}

/// Timeline range of the words `from..=to` (frame-snapped outward).
fn range_p(s: &Session, p: &Value, cmd: &str) -> Result<TimeRange> {
    let words = sequence_words(s);
    let from = u64_p(p, "from").ok_or_else(|| bad(cmd, "`from` (word index) is required"))? as usize;
    let to = u64_p(p, "to").map(|n| n as usize).unwrap_or(from);
    tx::word_range(&words, from, to, s.sequence_rate()).ok_or_else(|| bad(cmd, format!("word index out of range (the transcript has {} words)", words.len())))
}

fn range_json(r: TimeRange) -> Value {
    json!({"start": r.start.0, "end": r.end().0})
}

fn select(s: &mut Session, p: &Value) -> Result<Value> {
    let r = range_p(s, p, "transcript.select")?;
    let fd = s.sequence_rate().frame_duration();
    s.edit_sequence("Mark Transcript Selection", |q, _, _| {
        q.mark_in = Some(r.start);
        q.mark_out = Some(r.end() - fd);
        Ok(())
    })?;
    s.set_playhead(r.start);
    Ok(range_json(r))
}

fn extract_or_lift(s: &mut Session, p: &Value, extract: bool) -> Result<Value> {
    let cmd = if extract { "transcript.extract" } else { "transcript.lift" };
    let r = range_p(s, p, cmd)?;
    let tg = s.targeting().targeted;
    s.edit_sequence(if extract { "Extract Text" } else { "Lift Text" }, |q, ctx, _| {
        if extract {
            edit::extract(q, &tg, r, ctx);
        } else {
            edit::lift(q, &tg, r, ctx);
        }
        q.mark_in = None;
        q.mark_out = None;
        Ok(())
    })?;
    s.set_playhead(r.start);
    Ok(range_json(r))
}

fn rename_speaker(s: &mut Session, p: &Value) -> Result<Value> {
    let name = str_p(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad("transcript.renameSpeaker", "`name` is required"))?.to_string();
    let item = u64_p(p, "item").map(ItemId).and_then(|i| media_item(s, i));
    // `speaker`: the current name (every transcript), or an index (needs `item`)
    let (old_name, index) = match p.get("speaker") {
        Some(Value::String(n)) => (Some(n.clone()), None),
        Some(v) if v.is_u64() => (None, v.as_u64().map(|n| n as usize)),
        _ => return Err(bad("transcript.renameSpeaker", "`speaker` (name, or index with `item`) is required")),
    };
    if index.is_some() && item.is_none() {
        return Err(bad("transcript.renameSpeaker", "a speaker index needs `item`"));
    }
    let mut n = 0;
    let mut next = s.project.transcripts.clone();
    for (i, t) in next.iter_mut() {
        if item.is_some_and(|x| x != *i) {
            continue;
        }
        let tt = Arc::make_mut(t);
        for (k, sp) in tt.speakers.iter_mut().enumerate() {
            if old_name.as_ref().is_some_and(|o| *o == sp.name) || index == Some(k) {
                sp.name = name.clone();
                n += 1;
            }
        }
    }
    if n == 0 {
        return Err(EngineError::Other("no such speaker".into()));
    }
    s.edit("Rename Speaker", move |pr, _| {
        pr.transcripts = next;
        Ok(())
    })?;
    Ok(json!({"renamed": n}))
}

pub(crate) fn remove_ranges(s: &mut Session, label: &str, ranges: Vec<TimeRange>) -> Result<Value> {
    let n = ranges.len();
    if n == 0 {
        return Ok(json!({"removed": 0, "ticks": 0}));
    }
    let total = s.edit_sequence(label, |q, ctx, _| Ok(tx::ripple_delete_ranges(q, ranges, ctx)))?;
    Ok(json!({"removed": n, "ticks": total.0, "seconds": total.0 as f64 / TICKS_PER_SECOND as f64}))
}

fn remove_pauses(s: &mut Session, p: &Value) -> Result<Value> {
    let words = sequence_words(s);
    let min = Tick::from_seconds_f64(f64_p(p, "minSeconds").unwrap_or(1.0));
    let keep = Tick::from_seconds_f64(f64_p(p, "keepSeconds").unwrap_or(0.15));
    let ranges = tx::find_pauses(&words, min, keep, s.sequence_rate());
    let found = ranges.len();
    let mut out = remove_ranges(s, "Remove Pauses", ranges)?;
    out["summary"] = json!(if found == 0 {
        format!("Aucune pause de {} s ou plus entre les mots : rien à supprimer", fmt_s(min.seconds()))
    } else {
        format!(
            "{found} pause{} supprimée{}, {} s retirées",
            if found > 1 { "s" } else { "" },
            if found > 1 { "s" } else { "" },
            fmt_s(out["seconds"].as_f64().unwrap_or(0.0))
        )
    });
    Ok(out)
}

/// Isolation rule for a word that is also a real word (« eux »), measured on the audio, not on the
/// word times: the transcriber stretches a word over the silence that follows or precedes it, so its
/// times say little about pauses. The sound containing the word's loudest 10 ms must be shorter than
/// `max_seconds` and sit between `pause_seconds` of quiet on both sides (or the start / end of the
/// sequence); the quiet is judged against the sequence's own noise floor (`vad::threshold`).
struct Isolation {
    levels: Vec<f32>,
    threshold: f32,
    max_seconds: f64,
    pause_seconds: f64,
}

impl Isolation {
    fn new(s: &Session, p: &Value) -> Result<Self> {
        let levels = crate::sequence_extras::mix_levels(s)?;
        let threshold = filmcraft_speech::vad::threshold(&levels);
        Ok(Isolation {
            levels,
            threshold,
            max_seconds: f64_p(p, "isolatedMaxSeconds").unwrap_or(0.4).clamp(0.05, 5.0),
            pause_seconds: f64_p(p, "isolatedPauseSeconds").unwrap_or(0.15).clamp(0.0, 5.0),
        })
    }

    /// `Ok` when the word is a hesitation standing alone; `Err(why not)` otherwise.
    fn check(&self, w: &SeqWord) -> std::result::Result<(), String> {
        let win = crate::sequence_extras::SILENCE_WINDOW_SECONDS;
        let at = |t: Tick| (t.seconds() / win).max(0.0) as usize;
        let (a, b) = (at(w.start), (at(w.end) + 1).min(self.levels.len()));
        let loud = (a..b).max_by(|x, y| self.levels[*x].total_cmp(&self.levels[*y])).ok_or("hors de l'audio")?;
        if self.levels[loud] < self.threshold {
            return Err("aucun son à cet endroit".into());
        }
        let (mut from, mut to) = (loud, loud + 1);
        while from > 0 && self.levels[from - 1] >= self.threshold {
            from -= 1;
        }
        while to < self.levels.len() && self.levels[to] >= self.threshold {
            to += 1;
        }
        let seconds = |frames: usize| frames as f64 * win;
        if seconds(to - from) >= self.max_seconds {
            return Err(format!("le son dure {} s (limite {} s) : le mot est dans la phrase", fmt_s(seconds(to - from)), fmt_s(self.max_seconds)));
        }
        let need = (self.pause_seconds / win).ceil() as usize;
        let quiet_before = (0..from).rev().take_while(|i| self.levels[*i] < self.threshold).count();
        let quiet_after = (to..self.levels.len()).take_while(|i| self.levels[*i] < self.threshold).count();
        if from > 0 && quiet_before < need {
            return Err(format!("collé au mot précédent ({} s de silence avant, il en faut {})", fmt_s(seconds(quiet_before)), fmt_s(self.pause_seconds)));
        }
        if to < self.levels.len() && quiet_after < need {
            return Err(format!("collé au mot suivant ({} s de silence après, il en faut {})", fmt_s(seconds(quiet_after)), fmt_s(self.pause_seconds)));
        }
        Ok(())
    }
}

fn fmt_s(x: f64) -> String {
    format!("{x:.2}").replace('.', ",")
}

/// The language of the transcripts behind `words` (the first word's media), lower case.
fn words_language(s: &Session, words: &[SeqWord]) -> Option<String> {
    words.iter().find_map(|w| s.project.transcripts.get(&w.item)).map(|t| t.language.to_lowercase()).filter(|l| !l.is_empty())
}

/// `transcript.removeFillers`: the hesitations the transcript wrote down, cut out of the sequence.
///
/// * `fillers`: the exact list (replaces the default); `language` (`fr`, `en`, `auto`) picks the default
///   list (`auto`/absent: the transcript's language; French: [`tx::FILLERS_FR`], else
///   [`tx::DEFAULT_FILLERS`]); `also`: words added to the list.
/// * `conditional`: words cut only when they stand alone (default in French without an explicit
///   `fillers`: « eux », which whisper-base writes for a spoken « euh »): see [`Isolation`]
///   (`isolatedMaxSeconds` 0.4, `isolatedPauseSeconds` 0.15).
/// * `apply` false lists what would be cut.
fn remove_fillers(s: &mut Session, p: &Value) -> Result<Value> {
    let words = sequence_words(s);
    let strings =
        |k: &str| -> Option<Vec<String>> { p.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()) };
    let lang = str_p(p, "language")
        .filter(|l| !l.is_empty() && *l != "auto")
        .map(str::to_lowercase)
        .or_else(|| words_language(s, &words))
        .unwrap_or_else(|| "en".into());
    let french = lang.starts_with("fr");
    let explicit = p.get("fillers").is_some();
    let mut fillers = strings("fillers").unwrap_or_else(|| (if french { tx::FILLERS_FR } else { tx::DEFAULT_FILLERS }).iter().map(|f| f.to_string()).collect());
    for extra in strings("also").unwrap_or_default() {
        if !fillers.contains(&extra) {
            fillers.push(extra);
        }
    }
    let conditional =
        strings("conditional").unwrap_or_else(|| if french && !explicit { tx::CONDITIONAL_FR.iter().map(|f| f.to_string()).collect() } else { Vec::new() });
    let rate = s.sequence_rate();
    let mut hits = tx::find_fillers(&words, &fillers);
    let mut kept: Vec<Value> = Vec::new();
    let cond_hits: Vec<std::ops::Range<usize>> = tx::find_fillers(&words, &conditional).into_iter().filter(|h| !hits.contains(h)).collect();
    if !cond_hits.is_empty() {
        let iso = Isolation::new(s, p)?;
        for h in cond_hits {
            let w = &words[h.start];
            match iso.check(w) {
                Ok(()) => hits.push(h),
                Err(why) => kept.push(json!({"word": w.text, "seconds": w.start.seconds(), "why": why})),
            }
        }
        hits.sort_by_key(|h| h.start);
    }
    let ranges = tx::filler_ranges(&words, &hits, rate);
    let cut: Vec<Value> = hits.iter().map(|h| json!({"word": words[h.start].text, "seconds": words[h.start].start.seconds()})).collect();
    let apply = bool_p(p, "apply").unwrap_or(true);
    let planned: f64 = ranges.iter().map(|r| r.duration.seconds()).sum();
    let mut out = if apply { remove_ranges(s, "Remove Filler Words", ranges)? } else { json!({"removed": 0, "ticks": 0, "seconds": 0.0}) };
    out["language"] = json!(lang);
    out["fillers"] = Value::Array(cut);
    out["kept"] = Value::Array(kept.clone());
    // « 5 hésitations supprimées (heu ×3, ben, bah), 1,2 s retirées »
    let mut counts: Vec<(String, usize)> = Vec::new();
    for h in &hits {
        let w = words[h.start].normalized();
        match counts.iter_mut().find(|c| c.0 == w) {
            Some(c) => c.1 += 1,
            None => counts.push((w, 1)),
        }
    }
    let list = counts.iter().map(|(w, n)| if *n > 1 { format!("{w} ×{n}") } else { w.clone() }).collect::<Vec<_>>().join(", ");
    let n = hits.len();
    let kept_text = if kept.is_empty() {
        String::new()
    } else {
        let mut kc: Vec<(String, usize)> = Vec::new();
        for k in &kept {
            let w = filmcraft_project::transcript::normalize_word(k["word"].as_str().unwrap_or_default());
            match kc.iter_mut().find(|c| c.0 == w) {
                Some(c) => c.1 += 1,
                None => kc.push((w, 1)),
            }
        }
        let total: usize = kc.iter().map(|c| c.1).sum();
        let names = kc.iter().map(|(w, n)| if *n > 1 { format!("« {w} » ×{n}") } else { format!("« {w} »") }).collect::<Vec<_>>().join(", ");
        format!(" ; {names} gardé{} (pas isolé{})", if total > 1 { "s" } else { "" }, if total > 1 { "s" } else { "" })
    };
    out["summary"] = json!(if n == 0 {
        format!(
            "Aucune hésitation trouvée parmi les {} mots transcrits (liste {} de {} mots : {}…){kept_text}. Whisper n'écrit pas toujours les euh : seuls les mots écrits se coupent.",
            words.len(),
            if french { "français" } else { "anglais" },
            fillers.len(),
            fillers.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
        )
    } else if apply {
        format!(
            "{n} hésitation{} supprimée{} ({list}), {} s retirées{kept_text}",
            if n > 1 { "s" } else { "" },
            if n > 1 { "s" } else { "" },
            fmt_s(out["seconds"].as_f64().unwrap_or(0.0))
        )
    } else {
        format!("{n} hésitation{} à supprimer ({list}), {} s{kept_text} (rien supprimé)", if n > 1 { "s" } else { "" }, fmt_s(planned))
    });
    Ok(out)
}

fn create_captions(s: &mut Session, p: &Value) -> Result<Value> {
    let words = sequence_words(s);
    let d = CaptionRules::default();
    let rules = CaptionRules {
        max_chars: u64_p(p, "maxChars").map(|n| n as usize).unwrap_or(d.max_chars),
        lines: u64_p(p, "lines").map(|n| n as usize).unwrap_or(d.lines),
        min_duration: f64_p(p, "minSeconds").map(Tick::from_seconds_f64).unwrap_or(d.min_duration),
        max_duration: f64_p(p, "maxSeconds").map(Tick::from_seconds_f64).unwrap_or(d.max_duration),
        gap_frames: p.get("gapFrames").and_then(Value::as_i64).unwrap_or(d.gap_frames),
        break_pause: d.break_pause,
    };
    let blocks = tx::caption_blocks(&words, &rules, s.sequence_rate());
    let format = str_p(p, "format").and_then(CaptionFormat::from_name).unwrap_or_default();
    let name = str_p(p, "name").unwrap_or("Transcript").to_string();
    // the track carries the spoken language (what `captions.translate` takes as its source)
    let language = words_language(s, &words);
    let n = blocks.len();
    let tid = s.edit_sequence("Create Captions", |q, ctx, st| {
        let tid = TrackId(ctx.alloc());
        let mut t = CaptionTrack::new(tid, name, format);
        if let Some(l) = language {
            t.language = l;
        }
        t.captions = tx::blocks_to_captions(&blocks, ctx);
        q.caption_tracks.insert(0, t);
        st.caption_selection.clear();
        Ok(tid)
    })?;
    Ok(json!({"track": tid.0, "captions": n}))
}

fn models(_: &mut Session, _: &Value) -> Result<Value> {
    let dir = models_dir();
    Ok(json!({
        "available": filmcraft_speech::available(),
        "default": filmcraft_speech::models::DEFAULT_MODEL,
        "dir": dir.as_ref().map(|d| d.to_string_lossy().to_string()),
        "models": filmcraft_speech::models::catalogue().iter().map(|m| json!({
            "id": m.id, "name": m.name, "multilingual": m.multilingual, "description": m.description,
            "license": m.license, "source": m.source, "size": m.size(),
            "installed": dir.as_ref().is_some_and(|d| filmcraft_speech::models::installed(d, m)),
        })).collect::<Vec<_>>(),
        // offline translation models for `captions.translate`
        "translation": filmcraft_speech::models::mt_catalogue().iter().map(|m| json!({
            "id": m.id, "name": m.name, "description": m.description,
            "license": m.license, "licenseUrl": m.license_url, "source": m.source, "size": m.size(),
            "installed": dir.as_ref().is_some_and(|d| filmcraft_speech::models::installed(d, m)),
        })).collect::<Vec<_>>(),
    }))
}

/// Download a catalogue model into `<data dir>/models` (feature `speech-download`). Hosts show
/// the size, source and licence (`transcript.models`) and ask before running this.
fn download_model(_: &mut Session, p: &Value) -> Result<Value> {
    let id = str_p(p, "model").unwrap_or(filmcraft_speech::models::DEFAULT_MODEL);
    let m =
        filmcraft_speech::models::find(id).or_else(|| filmcraft_speech::models::find_mt(id)).ok_or_else(|| speech_err(SpeechError::UnknownModel(id.into())))?;
    let dir = models_dir().ok_or_else(|| EngineError::Other("no data directory for speech models".into()))?;
    #[cfg(feature = "speech-download")]
    {
        filmcraft_speech::models::download(&dir, m, &mut |_, _, _| true).map_err(speech_err)?;
        Ok(json!({"model": m.id, "dir": filmcraft_speech::models::model_dir(&dir, m).to_string_lossy()}))
    }
    #[cfg(not(feature = "speech-download"))]
    {
        let _ = (m, dir);
        Err(EngineError::Other("model downloads are not available in this build (built without the `speech-download` feature)".into()))
    }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec(
            "transcript.generate",
            "Transcribe…",
            &["Sequence", "Transcript"],
            r#"{"items":[id]?,"model":"whisper-base"?,"language":"en|auto"?,"diarize":bool?,"maxSpeakers":n?,"prompt":str? (initial prompt: a text in the speaker's style, e.g. "Euh, alors, heu, hum…", read by Whisper as what was said before),"hesitations":bool=false (keep the hesitations: built-in disfluency prompt of the language, plus the filled pauses Whisper left out)}"#,
            can_transcribe,
            generate,
            true,
        ),
        spec(
            "transcript.set",
            "Import Transcript",
            &[],
            r#"{"item":id,"transcript":{"language":str,"speakers":[{"name":str}],"words":[{"text":str,"start":tick,"end":tick,"speaker":n?}]}}"#,
            always,
            set,
            true,
        ),
        spec("transcript.delete", "Delete Transcript", &["Sequence", "Transcript"], r#"{"items":[id]?}"#, has_transcripts, delete, true),
        spec("transcript.inspect", "Inspect Transcript", &[], r#"{"paragraphGapSeconds":f?}"#, always, inspect, false),
        spec("transcript.search", "Search Transcript", &[], r#"{"query":str}"#, always, search, false),
        spec("transcript.models", "List Speech Models", &[], "{}", always, models, false),
        spec(
            "transcript.downloadModel",
            "Download Speech Model",
            &[],
            r#"{"model":"whisper-base"|"whisper-large-v3-turbo"|"opus-mt-tc-big-fr-en"|"opus-mt-tc-big-en-fr"?}"#,
            can_download,
            download_model,
            true,
        ),
        spec("transcript.select", "Mark Selected Text", &[], r#"{"from":word,"to":word?}"#, has_transcript, select, true),
        spec("transcript.extract", "Extract Selected Text", &[], r#"{"from":word,"to":word?}"#, has_transcript, |s, p| extract_or_lift(s, p, true), true),
        spec("transcript.lift", "Lift Selected Text", &[], r#"{"from":word,"to":word?}"#, has_transcript, |s, p| extract_or_lift(s, p, false), true),
        spec(
            "transcript.renameSpeaker",
            "Rename Speaker…",
            &[],
            r#"{"speaker":"Speaker 1"|index,"name":str,"item":id?}"#,
            has_transcripts,
            rename_speaker,
            true,
        ),
        spec(
            "transcript.removePauses",
            "Remove Pauses",
            &["Sequence", "Transcript"],
            r#"{"minSeconds":f?,"keepSeconds":f?}"#,
            has_transcript,
            remove_pauses,
            true,
        ),
        spec(
            "transcript.removeFillers",
            "Remove Filler Words",
            &["Sequence", "Transcript"],
            r#"{"fillers":[str]? (replaces the default list),"language":"fr|en|auto"? (default list; auto = the transcript's),"also":[str]? (added to the list),"conditional":[str]? (cut only when isolated; default fr: eux),"isolatedMaxSeconds":f=0.4,"isolatedPauseSeconds":f=0.15,"apply":bool=true}"#,
            has_transcript,
            remove_fillers,
            true,
        ),
        spec(
            "transcript.createCaptions",
            "Create Captions from Transcript…",
            &["Sequence", "Transcript"],
            r#"{"maxChars":n?,"lines":1|2?,"minSeconds":f?,"maxSeconds":f?,"gapFrames":n?,"format":str?,"name":str?}"#,
            has_transcript,
            create_captions,
            true,
        ),
    ]
}
