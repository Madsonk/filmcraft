use std::sync::Arc;

use serde_json::json;

use crate::Session;
use filmcraft_project::{ItemId, Transcript, Word};
use filmcraft_speech::FixedTranscriber;
use filmcraft_time::Tick;

/// The demo project with a fake transcriber whose transcript fits the first A1 clip's media:
/// "Hello um world." (Speaker 1), a 1.2 s pause, "Second speaker here." (Speaker 2).
fn session() -> (Session, ItemId, Tick) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    let a = &q.audio_tracks[0].items[0];
    let (item, sin) = (a.item, a.source_in);
    let sec = |x: f64| sin + Tick::from_seconds_f64(x);
    let mut t = Transcript { language: "en".into(), ..Default::default() };
    for (text, a, b, sp) in
        [("Hello", 0.2, 0.5, 0), ("um", 0.6, 0.9, 0), ("world.", 1.0, 1.4, 0), ("Second", 2.6, 3.0, 1), ("speaker", 3.0, 3.5, 1), ("here.", 3.5, 4.0, 1)]
    {
        let mut w = Word::new(text, sec(a), sec(b));
        w.speaker = Some(sp);
        t.words.push(w);
    }
    t.normalize();
    s.transcriber = Some(Arc::new(FixedTranscriber { transcript: t, id: "fixed".into() }));
    let start = a_start(&s);
    (s, item, start)
}

fn a_start(s: &Session) -> Tick {
    s.active_sequence().unwrap().audio_tracks[0].items[0].start
}

fn words(s: &mut Session) -> Vec<String> {
    let r = s.execute("transcript.inspect", json!({})).unwrap();
    r["words"].as_array().unwrap().iter().map(|w| w["text"].as_str().unwrap().to_string()).collect()
}

#[test]
fn generate_inspect_search_and_rename() {
    let (mut s, item, start) = session();
    assert!(s.execute("transcript.select", json!({"from": 0})).is_err(), "disabled before transcribing");
    let r = s.execute("transcript.generate", json!({"items": [item.0]})).unwrap();
    assert_eq!(r["items"][0]["words"], 6, "{r}");
    assert_eq!(r["items"][0]["source"], "fixed");
    assert_eq!(s.project.transcripts[&item].words.len(), 6);

    let r = s.execute("transcript.inspect", json!({})).unwrap();
    assert_eq!(words(&mut s), ["Hello", "um", "world.", "Second", "speaker", "here."]);
    assert_eq!(r["words"][0]["start"].as_i64().unwrap(), (start + Tick::from_seconds_f64(0.2)).0, "mapped to sequence time");
    assert_eq!(r["paragraphs"].as_array().unwrap().len(), 2, "speaker change splits paragraphs: {}", r["paragraphs"]);
    assert_eq!(r["speakers"], json!(["Speaker 1", "Speaker 2"]));

    let r = s.execute("transcript.search", json!({"query": "second spea"})).unwrap();
    assert_eq!(r["matches"], json!([{"from": 3, "to": 4, "start": r["matches"][0]["start"], "end": r["matches"][0]["end"]}]));

    s.execute("transcript.renameSpeaker", json!({"speaker": "Speaker 2", "name": "Ann"})).unwrap();
    let r = s.execute("transcript.inspect", json!({})).unwrap();
    assert_eq!(r["words"][3]["speaker"], "Ann");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.transcripts[&item].speakers[1].name, "Speaker 2");
    s.execute("transcript.renameSpeaker", json!({"speaker": 0, "item": item.0, "name": "Bo"})).unwrap();
    assert_eq!(s.project.transcripts[&item].speakers[0].name, "Bo");
    assert!(s.execute("transcript.renameSpeaker", json!({"speaker": "Nobody", "name": "X"})).is_err());

    // transcribing is one undo step
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.transcripts.is_empty());
}

#[test]
fn select_extract_and_lift_by_words() {
    let (mut s, item, _) = session();
    s.execute("transcript.generate", json!({"items": [item.0]})).unwrap();
    let rate = s.sequence_rate();
    let before = s.active_sequence().unwrap().duration();

    let r = s.execute("transcript.select", json!({"from": 3, "to": 5})).unwrap();
    let (a, b) = (Tick(r["start"].as_i64().unwrap()), Tick(r["end"].as_i64().unwrap()));
    let q = s.active_sequence().unwrap();
    assert_eq!(q.mark_in, Some(a));
    assert_eq!(q.mark_out, Some(b - rate.frame_duration()), "Out is the last frame inside");
    assert_eq!(rate.snap(a), a, "frame aligned");
    assert_eq!(s.playhead(), a);

    // Extract "um": the sequence gets shorter by the word's frames and the word is gone
    let r = s.execute("transcript.extract", json!({"from": 1})).unwrap();
    let cut = Tick(r["end"].as_i64().unwrap()) - Tick(r["start"].as_i64().unwrap());
    assert!(cut > Tick::ZERO);
    assert_eq!(s.active_sequence().unwrap().duration(), before - cut);
    assert!(!words(&mut s).contains(&"um".to_string()), "{:?}", words(&mut s));
    s.active_sequence().unwrap().check().unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().duration(), before);

    // Lift leaves a gap: same duration, word gone
    s.execute("transcript.lift", json!({"from": 1, "to": 1})).unwrap();
    assert_eq!(s.active_sequence().unwrap().duration(), before);
    assert_eq!(words(&mut s), ["Hello", "world.", "Second", "speaker", "here."]);
    assert!(s.execute("transcript.extract", json!({"from": 99})).is_err());
}

#[test]
fn remove_fillers_pauses_and_create_captions() {
    let (mut s, item, _) = session();
    s.execute("transcript.generate", json!({"items": [item.0]})).unwrap();
    let before = s.active_sequence().unwrap().duration();
    let r = s.execute("transcript.removeFillers", json!({})).unwrap();
    assert_eq!(r["removed"], 1, "{r}");
    assert!(!words(&mut s).contains(&"um".to_string()));
    let r = s.execute("transcript.removePauses", json!({"minSeconds": 1.0, "keepSeconds": 0.1})).unwrap();
    assert_eq!(r["removed"], 1, "{r}");
    let after = s.active_sequence().unwrap().duration();
    assert!(after < before - Tick::from_seconds_f64(1.0), "the pause and the filler are gone");
    assert_eq!(words(&mut s).len(), 5);
    s.active_sequence().unwrap().check().unwrap();

    let r = s.execute("transcript.createCaptions", json!({"maxChars": 32})).unwrap();
    assert_eq!(r["captions"], 2, "one caption per speaker: {r}");
    let q = s.active_sequence().unwrap();
    let tr = &q.caption_tracks[0];
    assert_eq!(tr.captions[0].text, "Hello world.");
    assert_eq!(tr.captions[1].speaker.as_deref(), Some("Speaker 2"));
    q.check().unwrap();
}

#[test]
fn set_delete_and_models() {
    let (mut s, item, _) = session();
    let t = json!({"language": "en", "words": [
        {"text": "b", "start": 2000, "end": 3000, "speaker": 1},
        {"text": "a", "start": 0, "end": 1000},
    ]});
    let r = s.execute("transcript.set", json!({"item": item.0, "transcript": t})).unwrap();
    assert_eq!(r["words"], 2);
    let tr = &s.project.transcripts[&item];
    assert_eq!(tr.words[0].text, "a", "normalized: sorted");
    assert_eq!(tr.speakers.len(), 2, "missing speaker labels added");
    assert_eq!(tr.source, "imported");
    assert!(s.execute("transcript.set", json!({"item": 999_999, "transcript": {}})).is_err());
    assert!(s.execute("transcript.set", json!({"item": item.0, "transcript": {"words": 3}})).is_err());

    // survives save/load
    let bytes = filmcraft_format::encode(&s.project, true);
    let back = filmcraft_format::decode(&bytes).unwrap();
    assert_eq!(back.project.transcripts[&item].words.len(), 2);

    s.execute("transcript.delete", json!({"items": [item.0]})).unwrap();
    assert!(s.project.transcripts.is_empty());
    assert!(s.execute("transcript.delete", json!({})).is_err());

    let m = s.execute("transcript.models", json!({})).unwrap();
    assert_eq!(m["available"], filmcraft_speech::available());
    assert!(m["models"].as_array().unwrap().iter().any(|x| x["id"] == "whisper-base"));
}

#[test]
fn generate_without_a_transcriber() {
    let (mut s, item, _) = session();
    s.transcriber = None;
    let e = s.execute("transcript.generate", json!({"items": [item.0], "model": "nope"})).unwrap_err().to_string();
    // a build without speech-to-text says that first: the command is disabled (#97)
    let why = if filmcraft_speech::available() { "unknown speech model" } else { "not available in this build" };
    assert!(e.contains(why), "{e}");
    if !filmcraft_speech::available() {
        assert!(!s.is_enabled("sequence.transcribe"), "Transcribe Sequence follows transcript.generate");
        let e = s.execute("transcript.generate", json!({"items": [item.0]})).unwrap_err().to_string();
        assert!(e.contains("whisper") && e.contains("not available"), "{e}");
        #[cfg(not(feature = "speech-download"))]
        assert!(s.execute("transcript.downloadModel", json!({})).unwrap_err().to_string().contains("not available"));
    }
    // defaults to the media of the open sequence's audio clips
    s.transcriber = Some(Arc::new(FixedTranscriber { transcript: Transcript::default(), id: "empty".into() }));
    assert!(s.is_enabled("sequence.transcribe"), "an installed recogniser enables Transcribe Sequence");
    let r = s.execute("transcript.generate", json!({})).unwrap();
    assert!(r["items"].as_array().unwrap().len() >= 2, "{r}");
}

/// A 9 s mono recording of tone bursts (the "words") over room noise, and a French transcript whose
/// word times are stretched over the silences the way a transcriber writes them: « eux » alone
/// (2.0–2.25 s), « pour eux » in one breath (3.5–4.2 s), « Euh, » (5.5–5.8 s), « Ben » (7.0–7.6 s).
fn french_session() -> Session {
    use filmcraft_project::{SequenceSettings, TrackKind};
    use filmcraft_time::TimeRange;
    let sr = 48_000usize;
    let bursts = [(0.5, 1.3), (2.0, 2.25), (3.5, 4.2), (5.5, 5.8), (7.0, 7.6)];
    let mut seed = 777u32;
    let samples: Vec<f32> = (0..9 * sr)
        .flat_map(|i| {
            let t = i as f64 / sr as f64;
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let noise = ((seed >> 16) as f32 / 32768.0 - 1.0) * 0.0004;
            let loud = bursts.iter().any(|(a, b)| t >= *a && t < *b);
            let v = if loud { 0.3 * (i as f32 * 300.0 * std::f32::consts::TAU / sr as f32).sin() + noise } else { noise };
            [v, v]
        })
        .collect();
    let dir = crate::media_test_util::tmp_dir("fillers-fr");
    let wav = dir.join("talk.wav");
    std::fs::write(&wav, filmcraft_media::wav::write_wav16(&samples, 2, sr as u32)).unwrap();
    let mut s = Session::default();
    let r = s.execute("file.import", json!({"paths": [wav.to_string_lossy()]})).unwrap();
    let item = ItemId(r["items"][0].as_u64().unwrap());
    let mut p = (*s.project).clone();
    let rate = filmcraft_time::FrameRate::FPS_24;
    let seq = p.new_sequence("Talk", SequenceSettings { frame_rate: rate, ..Default::default() }, 1, 1, None);
    let range = TimeRange::new(Tick::ZERO, p.item(item).unwrap().duration());
    let a = p.make_track_item(item, TrackKind::Audio, Tick::ZERO, range, rate).unwrap();
    p.sequence_mut(seq).unwrap().audio_tracks[0].items.push(a);
    s.project = Arc::new(p);
    s.state.active_sequence = Some(seq);
    s.state.open_sequences = vec![seq];
    let t = |x: f64| Tick::from_seconds_f64(x).0;
    let words: Vec<_> =
        [("Bonjour", 0.5, 1.9), ("eux", 2.0, 2.25), ("pour", 3.5, 3.95), ("eux", 3.95, 4.2), ("Euh,", 5.5, 5.8), ("Ben", 7.0, 7.6), ("voilà.", 8.0, 8.4)]
            .iter()
            .map(|(w, a, b)| json!({"text": w, "start": t(*a), "end": t(*b)}))
            .collect();
    s.execute("transcript.set", json!({"item": item.0, "transcript": {"language": "fr", "speakers": [], "words": words}})).unwrap();
    s
}

fn texts(s: &mut Session) -> Vec<String> {
    words(s)
}

#[test]
fn french_fillers_cut_the_hesitations_and_an_isolated_eux_but_keep_a_real_eux() {
    let mut s = french_session();
    // dry run: what would be cut, what is kept and why
    let d = s.execute("transcript.removeFillers", json!({"apply": false})).unwrap();
    assert_eq!(d["language"], "fr", "{d}");
    let cut: Vec<&str> = d["fillers"].as_array().unwrap().iter().map(|f| f["word"].as_str().unwrap()).collect();
    assert_eq!(cut, ["eux", "Euh,", "Ben"], "{d}");
    let kept = d["kept"].as_array().unwrap();
    assert_eq!(kept.len(), 1, "{d}");
    assert!((kept[0]["seconds"].as_f64().unwrap() - 3.95).abs() < 0.02, "the eux of « pour eux »: {d}");
    assert!(kept[0]["why"].as_str().unwrap().contains("phrase"), "{d}");
    assert_eq!(texts(&mut s).len(), 7, "a dry run changes nothing");
    let r = s.execute("transcript.removeFillers", json!({})).unwrap();
    assert_eq!(r["removed"], 3, "{r}");
    let line = r["summary"].as_str().unwrap();
    assert!(line.starts_with("3 hésitations supprimées (eux, euh, ben)"), "{line}");
    assert!(line.contains("« eux » gardé (pas isolé)"), "{line}");
    assert_eq!(texts(&mut s), ["Bonjour", "pour", "eux", "voilà."]);
    s.active_sequence().unwrap().check().unwrap();
}

#[test]
fn fillers_lists_are_configurable() {
    // an explicit list replaces the default and « eux » is then not conditional (the previous behaviour)
    let mut s = french_session();
    let r = s.execute("transcript.removeFillers", json!({"fillers": ["euh", "eux"]})).unwrap();
    assert_eq!(r["removed"], 3, "both eux and the euh: {r}");
    assert_eq!(texts(&mut s), ["Bonjour", "pour", "Ben", "voilà."]);
    // `also` adds a word to the French list; `conditional: []` turns the isolated eux off
    let mut s = french_session();
    let r = s.execute("transcript.removeFillers", json!({"also": ["voilà"], "conditional": []})).unwrap();
    assert_eq!(r["removed"], 3, "{r}");
    assert_eq!(texts(&mut s), ["Bonjour", "eux", "pour", "eux"]);
    // English list by language: nothing of the French hesitations
    let mut s = french_session();
    let r = s.execute("transcript.removeFillers", json!({"language": "en"})).unwrap();
    assert_eq!(r["removed"], 0, "{r}");
    assert!(r["summary"].as_str().unwrap().starts_with("Aucune hésitation trouvée parmi les 7 mots"), "{r}");
    // a very long allowed duration and no pause needed: the « eux » of « pour eux » is still glued to « pour »
    let mut s = french_session();
    let d = s.execute("transcript.removeFillers", json!({"apply": false, "isolatedMaxSeconds": 1.5, "isolatedPauseSeconds": 0.0})).unwrap();
    assert_eq!(d["fillers"].as_array().unwrap().len(), 4, "with no pause required every eux counts: {d}");
}
