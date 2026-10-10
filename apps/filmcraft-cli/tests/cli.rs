//! End-to-end tests of the CLI binary (headless backend).

use std::io::Write;
use std::process::{Command, Output, Stdio};

use serde_json::Value;

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_filmcraft-cli")).args(args).output().expect("spawn filmcraft-cli")
}

fn json_out(o: &Output) -> Value {
    assert!(o.status.success(), "failed: {}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).expect("JSON on stdout")
}

#[test]
fn help_and_usage_errors() {
    let h = cli(&["help"]);
    assert!(h.status.success());
    assert!(String::from_utf8_lossy(&h.stdout).contains("exec <id>"));
    assert_eq!(cli(&[]).status.code(), Some(2));
    assert_eq!(cli(&["frobnicate"]).status.code(), Some(2));
    assert_eq!(cli(&["exec", "file.newBin", "notkv"]).status.code(), Some(2));
}

#[test]
fn probe_reports_mpeg_transport_and_program_streams() {
    let Some(ffmpeg) = ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg"].into_iter().find(|p| std::path::Path::new(p).exists()) else {
        eprintln!("SKIPPED (probe_reports_mpeg_transport_and_program_streams): ffmpeg not found");
        return;
    };
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/cli-tests").join(format!("mpeg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, fmt, extra) in [("a.ts", "mpegts", &["-mpegts_m2ts_mode", "0"][..]), ("b.vob", "vob", &[][..])] {
        let out = dir.join(name);
        let st = Command::new(ffmpeg)
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=25", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000"])
            .args(["-t", "0.5", "-c:v", "mpeg2video", "-c:a", "mp2", "-ac", "2", "-f", fmt])
            .args(extra)
            .arg(&out)
            .status()
            .unwrap();
        assert!(st.success());
        let v = json_out(&cli(&["probe", out.to_str().unwrap()]));
        assert!(v["video"]["codec"].as_str().unwrap().starts_with("MPEG-2 Video"), "{v}");
        assert_eq!(v["audio_streams"][0]["codec"], "MPEG Audio");
        let streams = v["mpeg"]["streams"].as_array().unwrap();
        assert_eq!(streams.len(), 2, "{v}");
        assert_eq!(streams[0]["codec"], "MPEG-2 Video");
        assert!(streams[0]["pictures"]["I"].as_u64().unwrap() >= 1);
        assert_eq!(v["mpeg"]["format"], if fmt == "mpegts" { "MPEG-2 TS" } else { "MPEG-2 PS" });
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn exec_inspect_and_describe() {
    let seq = json_out(&cli(&["--demo", "inspect", "sequence", "--compact"]));
    assert!(seq["tracks"].is_array() || seq.is_object(), "{seq}");
    let d = json_out(&cli(&["describe", "timeline.razor"]));
    assert_eq!(d["id"], "timeline.razor");
    assert!(d["params"].is_string());
    let list = json_out(&cli(&["commands", "razor", "--json"]));
    assert!(list.as_array().unwrap().iter().any(|c| c["id"] == "timeline.razor"));
    // An unknown command fails with exit status 1.
    let bad = cli(&["--demo", "exec", "no.such.command"]);
    assert_eq!(bad.status.code(), Some(1));
}

#[test]
fn exec_save_as_then_reopen() {
    let dir = std::env::temp_dir().join(format!("fc-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let proj = dir.join("p.fcproj");
    let p = proj.to_str().unwrap();
    json_out(&cli(&["--demo", "exec", "file.newBin", "name=CLI Selects", "--save-as", p]));
    let tree = json_out(&cli(&["--project", p, "inspect", "project"]));
    assert!(tree.to_string().contains("CLI Selects"), "{tree}");
    // `run` from stdin, editing and saving back in place.
    let mut child = Command::new(env!("CARGO_BIN_EXE_filmcraft-cli"))
        .args(["--project", p, "--save", "run", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"# comment\n{\"id\":\"file.newBin\",\"params\":{\"name\":\"Second\"}}\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let line: Value = serde_json::from_slice(out.stdout.split(|b| *b == b'\n').next().unwrap()).unwrap();
    assert_eq!(line["ok"], true);
    assert!(json_out(&cli(&["--project", p, "inspect", "project"])).to_string().contains("Second"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn run_keep_going_reports_failures() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_filmcraft-cli"))
        .args(["--demo", "--keep-going", "run", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"{\"id\":\"no.such\"}\n{\"id\":\"file.newBin\",\"params\":{\"name\":\"x\"}}\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let lines: Vec<Value> = String::from_utf8_lossy(&out.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["ok"], false);
    assert_eq!(lines[1]["ok"], true);
}

#[test]
fn export_wav_waits_for_job() {
    let dir = std::env::temp_dir().join(format!("fc-cli-x-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("still.wav");
    let o = cli(&["--demo", "export", out.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(std::fs::metadata(&out).map(|m| m.len() > 44).unwrap_or(false));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `export --preset`: a built-in preset by name (dash or en dash), a custom range, a user preset
/// from `--data-dir`, `--list-presets`, `--queue`, and a clean failure for an unknown preset.
#[test]
fn export_with_presets() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/export-tests").join(format!("cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let data = dir.join("data");
    let data_s = data.to_str().unwrap();
    let out = dir.join("proxy.mov");
    let o = cli(&["--demo", "--data-dir", data_s, "export", out.to_str().unwrap(), "--preset", "Apple ProRes 422 Proxy", "--start", "0", "--end", "0.25"]);
    let v = json_out(&o);
    assert_eq!(v["preset"], "Apple ProRes 422 Proxy");
    assert!(std::fs::metadata(&out).map(|m| m.len() > 1000).unwrap_or(false));
    if let Some(ffprobe) = ["/opt/homebrew/bin/ffprobe", "/usr/local/bin/ffprobe", "/usr/bin/ffprobe"].into_iter().find(|p| std::path::Path::new(p).exists()) {
        let p = Command::new(ffprobe)
            .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=codec_name,profile,width", "-of", "csv=p=0", out.to_str().unwrap()])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&p.stdout).trim(), "prores,Proxy,1920");
    }
    // the extension comes from the preset; a hyphen finds the en-dash name
    let base = dir.join("adaptive");
    json_out(&cli(&[
        "--demo",
        "--data-dir",
        data_s,
        "export",
        base.to_str().unwrap(),
        "--preset",
        "Match Source - Adaptive Low Bitrate",
        "--start",
        "0",
        "--end",
        "0.2",
    ]));
    assert!(dir.join("adaptive.mp4").exists());
    // a user preset saved into the data directory is found by later runs
    json_out(&cli(&[
        "--data-dir",
        data_s,
        "exec",
        "export.presets.save",
        "name=Tiny WAV",
        "from=Waveform Audio 48 kHz 16-bit",
        "settings={\"audio\":{\"channels\":1}}",
    ]));
    let list = json_out(&cli(&["--data-dir", data_s, "export", "--list-presets", "tiny"]));
    assert_eq!(list.as_array().unwrap().len(), 1, "{list}");
    let wav = dir.join("tiny.wav");
    json_out(&cli(&["--demo", "--data-dir", data_s, "export", wav.to_str().unwrap(), "--preset", "Tiny WAV", "--start", "0", "--end", "0.5"]));
    let b = std::fs::read(&wav).unwrap();
    assert_eq!(u16::from_le_bytes([b[22], b[23]]), 1, "mono from the user preset");
    // through the queue
    let q = dir.join("queued.wav");
    let v = json_out(&cli(&["--demo", "--data-dir", data_s, "export", q.to_str().unwrap(), "--preset", "Tiny WAV", "--queue", "--range", "entire"]));
    assert_eq!(v["items"][0]["status"], "done", "{v}");
    assert!(q.exists());
    // a format that changes the extension: the completion message names the file written (#341)
    let apv = dir.join("apv.mp4");
    let o =
        cli(&["--demo", "--data-dir", data_s, "export", apv.to_str().unwrap(), "--preset", "APV 422-10", "--start", "0", "--end", "0.1", "--scale", "0.125"]);
    let v = json_out(&o);
    let written = v["path"].as_str().unwrap().to_string();
    assert!(written.ends_with("apv.mp4.mov") && std::path::Path::new(&written).exists(), "{v}");
    let msg = String::from_utf8_lossy(&o.stderr);
    assert!(msg.contains(&format!("exported {written} in ")), "{msg}");
    let qa = dir.join("queued-apv.mp4");
    let o = cli(&[
        "--demo",
        "--data-dir",
        data_s,
        "export",
        qa.to_str().unwrap(),
        "--preset",
        "APV 422-10",
        "--queue",
        "--start",
        "0",
        "--end",
        "0.1",
        "--scale",
        "0.125",
    ]);
    let v = json_out(&o);
    let written = v["items"][0]["path"].as_str().unwrap().to_string();
    assert!(written.ends_with("queued-apv.mp4.mov"), "{v}");
    assert!(String::from_utf8_lossy(&o.stderr).contains(&format!("exported {written} in ")), "{}", String::from_utf8_lossy(&o.stderr));
    // unknown preset: exit status 1 and a message naming it
    let bad = cli(&["--demo", "--data-dir", data_s, "export", dir.join("x.mp4").to_str().unwrap(), "--preset", "No Such Preset"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("No Such Preset"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `filmcraft-cli commands | head -c 10` panicked with "failed printing to stdout: Broken pipe
/// (os error 32)" and exit status 101 (#285).
#[test]
fn closed_stdout_ends_quietly() {
    use std::io::Read;
    for args in [&["commands"][..], &["help"], &["--demo", "commands", "--json"]] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_filmcraft-cli")).args(args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let mut first = [0u8; 10];
        child.stdout.as_mut().unwrap().read_exact(&mut first).unwrap();
        drop(child.stdout.take());
        let out = child.wait_with_output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success() && err.is_empty(), "{args:?}: {:?}: {err}", out.status);
    }
}

/// `run -` with stdout already closed: `run` reads its script to the end before it prints, so its
/// first write fails for sure. The rest of the script and the save still happen, and the exit status
/// still reports a failing command.
fn run_with_closed_stdout(args: &[&str], script: &[u8]) -> Output {
    let mut child =
        Command::new(env!("CARGO_BIN_EXE_filmcraft-cli")).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    drop(child.stdout.take());
    child.stdin.take().unwrap().write_all(script).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn closed_stdout_still_runs_the_script_and_saves() {
    let dir = std::env::temp_dir().join(format!("fc-cli-pipe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let proj = dir.join("p.fcproj");
    let p = proj.to_str().unwrap();
    let script = b"{\"id\":\"file.newBin\",\"params\":{\"name\":\"Piped One\"}}\n{\"id\":\"file.newBin\",\"params\":{\"name\":\"Piped Two\"}}\n";
    let out = run_with_closed_stdout(&["--demo", "--save-as", p, "run", "-"], script);
    assert!(out.status.success(), "{:?}: {}", out.status, String::from_utf8_lossy(&out.stderr));
    let tree = json_out(&cli(&["--project", p, "inspect", "project"])).to_string();
    assert!(tree.contains("Piped One") && tree.contains("Piped Two"), "{tree}");

    // a failing command is still a failure, whether or not anyone reads stdout
    let out = run_with_closed_stdout(&["--demo", "run", "-"], b"{\"id\":\"no.such.command\"}\n");
    assert_eq!(out.status.code(), Some(1));
    let out = run_with_closed_stdout(
        &["--demo", "--keep-going", "run", "-"],
        b"{\"id\":\"no.such.command\"}\n{\"id\":\"file.newBin\",\"params\":{\"name\":\"x\"}}\n",
    );
    assert_eq!(out.status.code(), Some(1));
}

// ---------------------------------------------------------------------------------------------------
// extensions (docs/extensions.md)
// ---------------------------------------------------------------------------------------------------

fn bundled_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions-bundled")
}

fn fresh_dir(tag: &str) -> std::path::PathBuf {
    let d = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/cli-tests").join(format!("{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 16-bit stereo 48 kHz WAV: (seconds, loud) stretches of a 440 Hz tone or faint room noise.
fn talk_wav(parts: &[(f64, bool)]) -> Vec<u8> {
    let sr = 48_000u32;
    let mut pcm: Vec<u8> = Vec::new();
    let mut seed = 7u32;
    for (secs, loud) in parts {
        for i in 0..(secs * sr as f64) as usize {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let noise = ((seed >> 16) as f32 / 32768.0 - 1.0) * 0.0004;
            let v = if *loud { 0.3 * (i as f32 * 440.0 * std::f32::consts::TAU / sr as f32).sin() + noise } else { noise };
            let s = (v * 32767.0) as i16;
            for _ in 0..2 {
                pcm.extend(s.to_le_bytes());
            }
        }
    }
    let mut w = Vec::new();
    w.extend(b"RIFF");
    w.extend((36 + pcm.len() as u32).to_le_bytes());
    w.extend(b"WAVEfmt ");
    w.extend(16u32.to_le_bytes());
    w.extend(1u16.to_le_bytes());
    w.extend(2u16.to_le_bytes());
    w.extend(sr.to_le_bytes());
    w.extend((sr * 4).to_le_bytes());
    w.extend(4u16.to_le_bytes());
    w.extend(16u16.to_le_bytes());
    w.extend(b"data");
    w.extend((pcm.len() as u32).to_le_bytes());
    w.extend(pcm);
    w
}

#[test]
fn extensions_install_list_validate_pack_remove() {
    let dir = fresh_dir("ext-manage");
    let data = dir.join("data");
    let data_s = data.to_str().unwrap();
    let bundled = bundled_dir();
    // a folder holding several extensions installs each of them
    let r = json_out(&cli(&["--data-dir", data_s, "extensions", "install", bundled.to_str().unwrap(), "--compact"]));
    let ids: Vec<&str> = r.as_array().unwrap().iter().map(|x| x["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        [
            "autoedit",
            "beat-edit",
            "copy-pasta",
            "export-en-un-clic",
            "silence-remover",
            "sous-titres",
            "sous-titres-animes",
            "storyboard-maker",
            "tics-de-langage",
            "traduction-sous-titres"
        ],
        "{r}"
    );
    // the list is sorted by name and carries the entries
    let l = json_out(&cli(&["--data-dir", data_s, "extensions", "list"]));
    let names: Vec<&str> = l.as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "AutoEdit",
            "Beat Edit",
            "Copy Pasta",
            "Export en un clic",
            "Silence Remover",
            "Sous-titres",
            "Sous-titres animés",
            "Storyboard Maker",
            "Tics de langage",
            "Traduction des sous-titres"
        ]
    );
    let silence = l.as_array().unwrap().iter().find(|x| x["id"] == "silence-remover").unwrap();
    assert_eq!(silence["entries"].as_array().unwrap().len(), 3);
    assert_eq!(silence["entries"][0]["kind"], "commands");
    let story = l.as_array().unwrap().iter().find(|x| x["id"] == "storyboard-maker").unwrap();
    assert_eq!(story["entries"][0]["kind"], "exec");
    // every command the bundled extensions use exists in the engine
    for id in [
        "silence-remover",
        "sous-titres",
        "export-en-un-clic",
        "beat-edit",
        "copy-pasta",
        "autoedit",
        "storyboard-maker",
        "sous-titres-animes",
        "traduction-sous-titres",
        "tics-de-langage",
    ] {
        let v = json_out(&cli(&["extensions", "validate", bundled.join(id).to_str().unwrap()]));
        assert_eq!(v["valid"], true, "{v}");
        assert_eq!(v["commandsUnknownToThisCli"], serde_json::json!([]), "{id}: {v}");
    }
    // pack an extension, install the archive somewhere else, update in place
    let archive = dir.join("silence.fcext");
    let p = json_out(&cli(&["extensions", "pack", bundled.join("silence-remover").to_str().unwrap(), archive.to_str().unwrap()]));
    assert_eq!(p["packed"], "silence-remover");
    let data2 = dir.join("data2");
    let r = json_out(&cli(&["--data-dir", data2.to_str().unwrap(), "extensions", "install", archive.to_str().unwrap()]));
    assert_eq!((r["id"].as_str(), r["replaced"].is_null()), (Some("silence-remover"), true));
    let r = json_out(&cli(&["--data-dir", data2.to_str().unwrap(), "extensions", "install", archive.to_str().unwrap()]));
    assert_eq!(r["replaced"], "1.0.0");
    // errors are readable and exit 1; usage errors exit 2
    let bad = cli(&["--data-dir", data_s, "extensions", "install", dir.join("nothing-here").to_str().unwrap()]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("does not exist"), "{}", String::from_utf8_lossy(&bad.stderr));
    std::fs::write(dir.join("junk.fcext"), b"not a zip").unwrap();
    let bad = cli(&["--data-dir", data_s, "extensions", "install", dir.join("junk.fcext").to_str().unwrap()]);
    assert!(String::from_utf8_lossy(&bad.stderr).contains("not a valid archive"));
    let bad = cli(&["--data-dir", data_s, "extensions", "remove", "../data"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(data.exists(), "a hostile id deleted nothing");
    assert_eq!(cli(&["extensions"]).status.code(), Some(2));
    assert_eq!(cli(&["extensions", "frobnicate"]).status.code(), Some(2));
    // remove
    json_out(&cli(&["--data-dir", data_s, "extensions", "remove", "sous-titres"]));
    let l = json_out(&cli(&["--data-dir", data_s, "extensions", "list"]));
    assert_eq!(l.as_array().unwrap().len(), 9);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_silence_remover_cuts_a_real_recording() {
    let dir = fresh_dir("ext-silence");
    let data = dir.join("data");
    let data_s = data.to_str().unwrap();
    json_out(&cli(&["--data-dir", data_s, "extensions", "install", bundled_dir().join("silence-remover").to_str().unwrap()]));
    // 1 s speech, 2 s silence, 1 s speech, 0.3 s breath, 1 s speech, 1.5 s silence: 6.8 s
    let wav = dir.join("talk.wav");
    std::fs::write(&wav, talk_wav(&[(1.0, true), (2.0, false), (1.0, true), (0.3, false), (1.0, true), (1.5, false)])).unwrap();
    let proj = dir.join("talk.fcproj");
    let p = proj.to_str().unwrap();
    let imported = json_out(&cli(&["--data-dir", data_s, "--demo", "exec", "file.import", &format!("paths=[{:?}]", wav.to_str().unwrap()), "--save-as", p]));
    let item = imported["items"][0].as_u64().unwrap();
    json_out(&cli(&["--data-dir", data_s, "--project", p, "--save", "exec", "file.newSequence", "name=Talk", "video=1", "audio=1"]));
    json_out(&cli(&["--data-dir", data_s, "--project", p, "--save", "exec", "timeline.place", &format!("item={item}"), "track=A1", "seconds=0"]));
    let duration = |p: &str| -> f64 {
        let v = json_out(&cli(&["--data-dir", data_s, "--project", p, "inspect", "sequence", "--compact"]));
        v["duration"].as_f64().unwrap_or_else(|| panic!("no duration in {v}")) / 254_016_000_000.0
    };
    let before = duration(p);
    assert!((before - 6.8).abs() < 0.1, "{before}");
    // run the default entry on the saved project, in place
    let r = json_out(&cli(&["--data-dir", data_s, "--project", p, "--save", "extensions", "run", "silence-remover"]));
    assert_eq!(r["extension"], "silence-remover");
    assert_eq!(r["steps"][0]["ok"], true, "{r}");
    assert_eq!(r["steps"][0]["result"]["silences"], 2, "{r}");
    let after = duration(p);
    assert!((before - after - 3.2).abs() < 0.2, "{before} -> {after}: {r}");
    // an entry by label, with a variable replacing the 0.5 s default: nothing is left to cut
    let r = json_out(&cli(&["--data-dir", data_s, "--project", p, "extensions", "run", "silence-remover", "couper les silences", "--var", "min_silence=5"]));
    assert_eq!(r["steps"][0]["result"]["silences"], 0, "{r}");
    // errors: unknown entry, unknown extension, unmet requirement (no sequence in an empty project)
    let bad = cli(&["--data-dir", data_s, "--project", p, "extensions", "run", "silence-remover", "no such entry"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("no entry"), "{}", String::from_utf8_lossy(&bad.stderr));
    let bad = cli(&["--data-dir", data_s, "extensions", "run", "silence-remover"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("open a sequence first"), "{}", String::from_utf8_lossy(&bad.stderr));
    let bad = cli(&["--data-dir", data_s, "extensions", "run", "ghost"]);
    assert!(String::from_utf8_lossy(&bad.stderr).contains("not installed"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A binary without a speech recogniser: Sous-titres says so instead of pretending.
#[cfg(not(feature = "whisper"))]
#[test]
fn the_subtitles_extension_reports_a_build_without_speech_to_text() {
    let dir = fresh_dir("ext-subs");
    let data = dir.join("data");
    let data_s = data.to_str().unwrap();
    json_out(&cli(&["--data-dir", data_s, "extensions", "install", bundled_dir().join("sous-titres").to_str().unwrap()]));
    let o = cli(&["--data-dir", data_s, "--demo", "extensions", "run", "sous-titres", "0"]);
    assert_eq!(o.status.code(), Some(1));
    let all = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(all.contains("speech-to-text is not available"), "{all}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A program extension: gets the context on stdin, its printed commands run in the session.
#[cfg(unix)]
#[test]
fn a_program_extension_hands_commands_back() {
    use std::os::unix::fs::PermissionsExt;
    let dir = fresh_dir("ext-exec");
    let src = dir.join("src/marker-script");
    std::fs::create_dir_all(src.join("bin")).unwrap();
    std::fs::write(
        src.join("extension.json"),
        r#"{"id":"marker-script","name":"Marker Script","version":"0.1.0","entries":[{"label":"Mark","requires":["sequence"],"action":{"exec":{"program":"bin/mark.sh","args":["${var.label|\"Script marker\"}"],"timeoutSeconds":20}}}]}"#,
    )
    .unwrap();
    std::fs::write(src.join("bin/mark.sh"), "#!/bin/sh\ncat > \"$FILMCRAFT_EXTENSION_DIR/context.json\"\necho \"{\\\"log\\\": \\\"marking\\\"}\"\necho \"{\\\"id\\\": \\\"markers.add\\\", \\\"params\\\": {\\\"name\\\": \\\"$1\\\"}}\"\n").unwrap();
    std::fs::set_permissions(src.join("bin/mark.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let data = dir.join("data");
    let data_s = data.to_str().unwrap();
    let r = json_out(&cli(&["--data-dir", data_s, "extensions", "install", src.to_str().unwrap()]));
    assert_eq!(r["runsPrograms"], true);
    let r = json_out(&cli(&["--data-dir", data_s, "--demo", "extensions", "run", "marker-script", "--var", "label=Hello"]));
    assert_eq!(r["steps"][0]["id"], "markers.add");
    assert_eq!(r["log"][0], "marking");
    let ctx: Value = serde_json::from_slice(&std::fs::read(data.join("extensions/marker-script/context.json")).unwrap()).unwrap();
    assert_eq!(ctx["host"], "cli");
    assert_eq!(ctx["hasSequence"], true);
    assert_eq!(ctx["vars"]["label"], "Hello");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The caption half of Sous-titres (the steps after the transcription) on an imported transcript:
/// a caption track is built from the words and written next to the project as .srt.
#[test]
fn captions_from_a_transcript_are_written_next_to_the_project() {
    let dir = fresh_dir("ext-srt");
    let data = dir.join("data");
    let data_s = data.to_str().unwrap();
    let wav = dir.join("talk.wav");
    std::fs::write(&wav, talk_wav(&[(3.0, true)])).unwrap();
    let proj = dir.join("Interview.fcproj");
    let p = proj.to_str().unwrap();
    let imported = json_out(&cli(&["--data-dir", data_s, "--demo", "exec", "file.import", &format!("paths=[{:?}]", wav.to_str().unwrap()), "--save-as", p]));
    let item = imported["items"][0].as_u64().unwrap();
    json_out(&cli(&["--data-dir", data_s, "--project", p, "--save", "exec", "file.newSequence", "name=Talk", "video=1", "audio=1"]));
    json_out(&cli(&["--data-dir", data_s, "--project", p, "--save", "exec", "timeline.place", &format!("item={item}"), "track=A1", "seconds=0"]));
    let t = |s: f64| (s * 254_016_000_000.0) as i64;
    let words = serde_json::json!([
        {"text": "Bonjour", "start": t(0.2), "end": t(0.6)},
        {"text": "tout", "start": t(0.7), "end": t(0.9)},
        {"text": "le", "start": t(0.95), "end": t(1.05)},
        {"text": "monde", "start": t(1.1), "end": t(1.6)}
    ]);
    let transcript = serde_json::json!({"language": "fr", "speakers": [{"name": "Speaker 1"}], "words": words});
    json_out(&cli(&[
        "--data-dir",
        data_s,
        "--project",
        p,
        "--save",
        "exec",
        "transcript.set",
        &serde_json::json!({"item": item, "transcript": transcript}).to_string(),
    ]));
    // the second half of the bundled "Générer et exporter en .srt" entry, as an extension of its own
    let src = dir.join("src/srt-only");
    std::fs::create_dir_all(&src).unwrap();
    let manifest = std::fs::read_to_string(bundled_dir().join("sous-titres/extension.json")).unwrap();
    let mut m: Value = serde_json::from_str(&manifest).unwrap();
    let steps = m["entries"][1]["action"]["commands"].as_array().unwrap().iter().skip(1).cloned().collect::<Vec<_>>();
    m["id"] = "srt-only".into();
    m["entries"] = serde_json::json!([{"label": "Go", "requires": ["sequence", "project"], "action": {"commands": steps}}]);
    std::fs::write(src.join("extension.json"), m.to_string()).unwrap();
    json_out(&cli(&["--data-dir", data_s, "extensions", "install", src.to_str().unwrap()]));
    let r = json_out(&cli(&["--data-dir", data_s, "--project", p, "--save", "extensions", "run", "srt-only"]));
    assert_eq!(r["steps"].as_array().unwrap().len(), 2, "{r}");
    let srt = std::fs::read_to_string(dir.join("Interview.srt")).unwrap();
    assert!(srt.contains("Bonjour tout le monde") || (srt.contains("Bonjour") && srt.contains("monde")), "{srt}");
    assert!(srt.contains("-->"), "{srt}");
    // an unsaved project has no place to write the file: a clear message, nothing written
    let bad = cli(&["--data-dir", data_s, "--demo", "extensions", "run", "srt-only"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("save the project"), "{}", String::from_utf8_lossy(&bad.stderr));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Export en un clic writes next to the project, also when the project is named by a relative path
/// (the engine's own default would have fallen back to ~/Movies).
#[test]
fn export_en_un_clic_writes_next_to_the_project() {
    let dir = fresh_dir("ext-export");
    let data = dir.join("data");
    let data_s = data.to_str().unwrap();
    json_out(&cli(&["--data-dir", data_s, "extensions", "install", bundled_dir().join("export-en-un-clic").to_str().unwrap()]));
    let wav = dir.join("talk.wav");
    std::fs::write(&wav, talk_wav(&[(1.0, true), (0.5, false)])).unwrap();
    let proj = dir.join("talk.fcproj");
    let p = proj.to_str().unwrap();
    let imported = json_out(&cli(&["--data-dir", data_s, "--demo", "exec", "file.import", &format!("paths=[{:?}]", wav.to_str().unwrap()), "--save-as", p]));
    let item = imported["items"][0].as_u64().unwrap();
    json_out(&cli(&["--data-dir", data_s, "--project", p, "--save", "exec", "file.newSequence", "name=Cut", "video=1", "audio=1"]));
    json_out(&cli(&["--data-dir", data_s, "--project", p, "--save", "exec", "timeline.place", &format!("item={item}"), "track=A1", "seconds=0"]));
    // run from inside the project folder with a relative project path
    let out = Command::new(env!("CARGO_BIN_EXE_filmcraft-cli"))
        .current_dir(&dir)
        .args(["--data-dir", data_s, "--project", "talk.fcproj", "extensions", "run", "export-en-un-clic", "YouTube 1080p", "--compact"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let v = json_out(&out);
    assert_eq!(v["steps"][0]["result"]["preset"], "YouTube 1080p Full HD", "{v}");
    let written = v["steps"][0]["result"]["path"].as_str().unwrap();
    assert!(written.starts_with(dir.to_str().unwrap()) || written.contains("cli-tests"), "{written}");
    let file = dir.join("Cut.mp4");
    assert!(std::fs::metadata(&file).map(|m| m.len() > 1000).unwrap_or(false), "{written}");
    // an unsaved project has no folder to write to
    let bad = cli(&["--data-dir", data_s, "--demo", "extensions", "run", "export-en-un-clic", "0"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("save the project first"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 16-bit stereo 48 kHz WAV of drums at `bpm`: a kick on beats 1 and 3, a noise snare on 2 and 4,
/// hi-hats on the eighths, a bass note on every beat (the true beats sit at k * 60 / bpm seconds).
fn beat_wav(bpm: f64, bars: usize) -> Vec<u8> {
    let sr = 48_000u32;
    let beat = sr as f64 * 60.0 / bpm;
    let n = (bars as f64 * 4.0 * beat) as usize;
    let mut x = vec![0f32; n];
    let mut seed = 5u32;
    for k in 0..bars * 4 {
        let start = (k as f64 * beat).round() as usize;
        for i in 0..((beat * 0.9) as usize).min(n - start) {
            let t = i as f32 / sr as f32;
            if k % 2 == 0 {
                let f = 50.0 + 90.0 * (-t * 25.0).exp();
                x[start + i] += 0.8 * (-t * 9.0).exp() * (t * f * std::f32::consts::TAU).sin();
            } else if t < 0.2 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                x[start + i] += 0.35 * (-t * 22.0).exp() * ((seed >> 16) as f32 / 32768.0 - 1.0);
            }
            x[start + i] += 0.15 * (-t * 3.0).exp() * (t * 110.0 * std::f32::consts::TAU).sin();
        }
        // hi-hats on the eighths: without them the tracker may settle on half the tempo
        for h in 0..2 {
            let hs = start + (h as f64 * beat / 2.0) as usize;
            for i in 0..(sr as usize / 25).min(n.saturating_sub(hs)) {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                x[hs + i] += 0.08 * (-(i as f32 / sr as f32) * 120.0).exp() * ((seed >> 16) as f32 / 32768.0 - 1.0);
            }
        }
    }
    let mut pcm: Vec<u8> = Vec::new();
    for v in x {
        let s = (v * 0.8 * 32767.0) as i16;
        for _ in 0..2 {
            pcm.extend(s.to_le_bytes());
        }
    }
    let mut w = Vec::new();
    w.extend(b"RIFF");
    w.extend((36 + pcm.len() as u32).to_le_bytes());
    w.extend(b"WAVEfmt ");
    w.extend(16u32.to_le_bytes());
    w.extend(1u16.to_le_bytes());
    w.extend(2u16.to_le_bytes());
    w.extend(sr.to_le_bytes());
    w.extend((sr * 4).to_le_bytes());
    w.extend(4u16.to_le_bytes());
    w.extend(16u16.to_le_bytes());
    w.extend(b"data");
    w.extend((pcm.len() as u32).to_le_bytes());
    w.extend(pcm);
    w
}

/// A saved project with a sequence (video 2, audio 2) named `Cut`, and the ids of `n_media` black
/// videos of 2 s (`file.newBlackVideo`) placed one after the other on V1 plus a music clip (music
/// WAV of 120 BPM) on A1 at 0 s. Returns (project path, music clip id, video clip ids).
fn music_project(dir: &std::path::Path, data_s: &str, n_media: usize) -> (String, u64, Vec<u64>) {
    let wav = dir.join("music.wav");
    std::fs::write(&wav, beat_wav(120.0, 8)).unwrap();
    let proj = dir.join("beats.fcproj");
    let p = proj.to_str().unwrap().to_string();
    let imported = json_out(&cli(&["--data-dir", data_s, "--demo", "exec", "file.import", &format!("paths=[{:?}]", wav.to_str().unwrap()), "--save-as", &p]));
    let music = imported["items"][0].as_u64().unwrap();
    json_out(&cli(&["--data-dir", data_s, "--project", &p, "--save", "exec", "file.newSequence", "name=Cut", "video=2", "audio=2", "fps=24"]));
    let placed =
        json_out(&cli(&["--data-dir", data_s, "--project", &p, "--save", "exec", "timeline.place", &format!("item={music}"), "track=A1", "seconds=0"]));
    let music_clip = placed["clips"][0].as_u64().unwrap();
    let mut videos = Vec::new();
    for i in 0..n_media {
        let b = json_out(&cli(&["--data-dir", data_s, "--project", &p, "--save", "exec", "file.newBlackVideo", "seconds=2"]));
        let item = b["item"].as_u64().or_else(|| b["items"][0].as_u64()).unwrap_or_else(|| panic!("no item in {b}"));
        let r = json_out(&cli(&[
            "--data-dir",
            data_s,
            "--project",
            &p,
            "--save",
            "exec",
            "timeline.place",
            &format!("item={item}"),
            "track=V1",
            &format!("seconds={}", i * 2),
        ]));
        videos.push(r["clips"][0].as_u64().unwrap());
    }
    (p, music_clip, videos)
}

fn sequence_of(data_s: &str, p: &str) -> Value {
    json_out(&cli(&["--data-dir", data_s, "--project", p, "inspect", "sequence", "--compact"]))
}

#[test]
fn beat_edit_marks_and_cuts_on_the_beats_of_generated_music() {
    let dir = fresh_dir("ext-beats");
    let data_s = dir.join("data").to_str().unwrap().to_string();
    let data_s = data_s.as_str();
    json_out(&cli(&["--data-dir", data_s, "extensions", "install", bundled_dir().join("beat-edit").to_str().unwrap()]));
    let (p, music, videos) = music_project(&dir, data_s, 2);
    let tick = 254_016_000_000.0;
    // every beat: 120 BPM music of 16 s, the true beats every 0.5 s (a beat at the very start is not reported)
    let r = json_out(&cli(&["--data-dir", data_s, "--project", &p, "--save", "extensions", "run", "beat-edit", "Marquer les beats"]));
    assert_eq!(r["steps"][0]["ok"], true, "{r}");
    let bpm = r["steps"][0]["result"]["bpm"].as_f64().unwrap();
    assert!((bpm - 120.0).abs() < 2.0, "{r}");
    let markers = sequence_of(data_s, &p)["markers"].as_array().unwrap().clone();
    assert!((30..=32).contains(&markers.len()), "{} markers", markers.len());
    for m in &markers {
        let t = m["start"].as_f64().unwrap() / tick;
        assert!((t / 0.5 - (t / 0.5).round()).abs() * 0.5 < 0.025, "marker at {t} s is off the 0.5 s grid");
    }
    // every fourth beat (a separate name, so nothing is deduplicated)
    let out = cli(&["--data-dir", data_s, "--project", &p, "--save", "extensions", "run", "beat-edit", "Marquer toutes les 4 beats"]);
    let r = json_out(&out);
    let markers = sequence_of(data_s, &p)["markers"].as_array().unwrap().clone();
    // 32 beats of 0.5 s: 8 bars, one fewer when the first beat of the clip is not reported and is the bar start
    let bars = markers.iter().filter(|m| m["name"].as_str().unwrap().starts_with("Mesure")).count();
    assert!((7..=8).contains(&bars), "{bars} bars: {markers:?}");
    // what it did is said in words, in the report and on stderr
    let line = r["steps"][0]["result"]["summary"].as_str().unwrap();
    assert!(line.contains("marqueurs posés") && line.contains("BPM"), "{line}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Marquer toutes les 4 beats: ") && err.contains("marqueurs posés"), "{err}");
    // an entry that needs a selection says so; with --select the video clips (2 x 2 s) are cut on the beats
    let bad = cli(&["--data-dir", data_s, "--project", &p, "extensions", "run", "beat-edit", "Couper sur les beats"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("select clips"), "{}", String::from_utf8_lossy(&bad.stderr));
    let sel = format!("{music},{},{}", videos[0], videos[1]);
    let r = json_out(&cli(&["--data-dir", data_s, "--project", &p, "--save", "extensions", "run", "beat-edit", "Couper sur les beats", "--select", &sel]));
    assert_eq!(r["steps"][0]["ok"], true, "{r}");
    let q = sequence_of(data_s, &p);
    let v1 = q["video"][0]["items"].as_array().unwrap();
    // 2 s clips on a 0.5 s grid: 4 pieces each
    assert_eq!(v1.len(), 8, "{v1:?}");
    assert_eq!(q["audio"][0]["items"].as_array().unwrap().len(), 1, "the music is the beat source, not a target");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn copy_pasta_pastes_the_first_clips_attributes_on_the_others() {
    let dir = fresh_dir("ext-pasta");
    let data_s = dir.join("data").to_str().unwrap().to_string();
    let data_s = data_s.as_str();
    json_out(&cli(&["--data-dir", data_s, "extensions", "install", bundled_dir().join("copy-pasta").to_str().unwrap()]));
    let (p, _, videos) = music_project(&dir, data_s, 3);
    json_out(&cli(&[
        "--data-dir",
        data_s,
        "--project",
        &p,
        "--save",
        "exec",
        "effects.setParam",
        &format!("clip={}", videos[0]),
        "effect=motion",
        "param=scale",
        "value=150",
    ]));
    json_out(&cli(&[
        "--data-dir",
        data_s,
        "--project",
        &p,
        "--save",
        "exec",
        "effects.setParam",
        &format!("clip={}", videos[0]),
        "effect=opacity",
        "param=opacity",
        "value=40",
    ]));
    let scale_opacity = |q: &Value, clip: u64| -> (String, String) {
        let it = q["video"][0]["items"].as_array().unwrap().iter().find(|i| i["clip"] == clip).unwrap().clone();
        let get = |eff: &str, param: &str| it["effects"].as_array().unwrap().iter().find(|e| e["effect"] == eff).unwrap()["params"][param]["value"].to_string();
        (get("motion", "scale"), get("opacity", "opacity"))
    };
    let q = sequence_of(data_s, &p);
    assert!(scale_opacity(&q, videos[1]).0.contains("100"), "{:?}", scale_opacity(&q, videos[1]));
    let sel = videos.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    // only motion and opacity are copied by the other one-step entry; here, everything
    let r = json_out(&cli(&[
        "--data-dir",
        data_s,
        "--project",
        &p,
        "--save",
        "extensions",
        "run",
        "copy-pasta",
        "Premier clip vers les autres (tout)",
        "--select",
        &sel,
    ]));
    assert_eq!(r["steps"][2]["result"]["clips"], 2, "{r}");
    let q = sequence_of(data_s, &p);
    for c in &videos[1..] {
        let (scale, opacity) = scale_opacity(&q, *c);
        assert!(scale.contains("150"), "clip {c}: {scale}");
        assert!(opacity.contains("40"), "clip {c}: {opacity}");
    }
    // one clip selected: nothing to paste on, said plainly
    let bad = cli(&[
        "--data-dir",
        data_s,
        "--project",
        &p,
        "extensions",
        "run",
        "copy-pasta",
        "Premier clip vers les autres (tout)",
        "--select",
        &videos[0].to_string(),
    ]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("at least two clips") || String::from_utf8_lossy(&bad.stdout).contains("at least two clips"));
    let _ = std::fs::remove_dir_all(&dir);
}

fn has_python_with_pillow() -> bool {
    Command::new("python3").args(["-c", "import PIL"]).output().map(|o| o.status.success()).unwrap_or(false)
}

#[test]
fn storyboard_maker_exports_a_still_per_shot_and_a_sheet() {
    let dir = fresh_dir("ext-story");
    let data_s = dir.join("data").to_str().unwrap().to_string();
    let data_s = data_s.as_str();
    json_out(&cli(&["--data-dir", data_s, "extensions", "install", bundled_dir().join("storyboard-maker").to_str().unwrap()]));
    let python = Command::new("python3").arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    if !python {
        eprintln!("python3 is not installed: skipping the storyboard run");
        return;
    }
    let (p, _, _) = music_project(&dir, data_s, 3);
    json_out(&cli(&["--data-dir", data_s, "--project", &p, "--save", "exec", "markers.add", "seconds=1", "name=Premier", "comment=idee"]));
    let r = json_out(&cli(&["--data-dir", data_s, "--project", &p, "extensions", "run", "storyboard-maker", "Exporter une image par plan"]));
    assert_eq!(r["steps"].as_array().unwrap().len(), 6, "{r}");
    let folder = dir.join("beats-storyboard");
    for n in ["001.png", "002.png", "003.png", "index.json"] {
        assert!(std::fs::metadata(folder.join(n)).map(|m| m.len() > 50).unwrap_or(false), "{n} in {r}");
    }
    let r = json_out(&cli(&["--data-dir", data_s, "--project", &p, "extensions", "run", "storyboard-maker", "Exporter une image par marqueur"]));
    assert_eq!(r["steps"].as_array().unwrap().len(), 2, "{r}");
    let index: Value = serde_json::from_slice(&std::fs::read(folder.join("index.json")).unwrap()).unwrap();
    assert_eq!(index["frames"][0]["title"], "Premier - idee");
    assert!(!folder.join("002.png").exists(), "the stills of the previous run are replaced");
    if has_python_with_pillow() {
        json_out(&cli(&["--data-dir", data_s, "--project", &p, "extensions", "run", "storyboard-maker", "Assembler la planche (PNG + PDF)"]));
        let pdf = std::fs::read(dir.join("beats-storyboard.pdf")).unwrap();
        assert!(pdf.starts_with(b"%PDF"), "a PDF");
        assert!(std::fs::metadata(dir.join("beats-storyboard.png")).map(|m| m.len() > 500).unwrap_or(false));
    } else {
        eprintln!("Pillow is not installed: skipping the contact sheet");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn has_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().map(|o| o.status.success()).unwrap_or(false) || std::path::Path::new("/opt/homebrew/bin/ffmpeg").exists()
}

/// Duration of a PCM WAV file, from its header.
fn wav_seconds(path: &std::path::Path) -> f64 {
    let b = std::fs::read(path).unwrap();
    let rate = u32::from_le_bytes(b[24..28].try_into().unwrap()) as f64;
    let align = u16::from_le_bytes(b[32..34].try_into().unwrap()) as f64;
    // find the "data" chunk
    let mut i = 12;
    while i + 8 <= b.len() {
        let len = u32::from_le_bytes(b[i + 4..i + 8].try_into().unwrap()) as usize;
        if &b[i..i + 4] == b"data" {
            return len as f64 / align / rate;
        }
        i += 8 + len + (len & 1);
    }
    panic!("no data chunk");
}

#[test]
fn doublage_voix_dubs_a_clip_with_a_stand_in_for_voicebox() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let dir = fresh_dir("ext-dub");
    let data_s = dir.join("data").to_str().unwrap().to_string();
    let data_s = data_s.as_str();
    json_out(&cli(&["--data-dir", data_s, "extensions", "install", bundled_dir().join("doublage-voix").to_str().unwrap()]));
    let python = Command::new("python3").arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    if !python || !has_ffmpeg() {
        eprintln!("python3 or ffmpeg is missing: skipping the dubbing run");
        return;
    }
    let (p, music_clip, _) = music_project(&dir, data_s, 0);
    // the translated phrases (what a subtitle translation, or any .srt, provides) and their places
    let srt = dir.join("en.srt");
    std::fs::write(
        &srt,
        "1\n00:00:01,000 --> 00:00:03,000\nHello everyone.\n\n2\n00:00:05,000 --> 00:00:06,500\nToday we are cutting silences.\n\n3\n00:00:09,000 --> 00:00:09,600\nThanks a lot for watching this very long video until the end!\n",
    )
    .unwrap();
    let fake = dir.join("fake_voicebox.py");
    std::fs::write(&fake, include_str!("fake_voicebox.py")).unwrap();
    let mut server = Command::new("python3").arg(&fake).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let port = {
        let mut line = String::new();
        BufReader::new(server.stdout.as_mut().unwrap()).read_line(&mut line).unwrap();
        line.trim().to_string()
    };
    let url = format!("http://127.0.0.1:{port}");
    let run = |extra: &[&str]| {
        let sel = music_clip.to_string();
        let mut a = vec![
            "--data-dir",
            data_s,
            "--project",
            &p,
            "--save",
            "extensions",
            "run",
            "doublage-voix",
            "Doubler en anglais",
            "--select",
            &sel,
            "--var",
            "voice=Test",
            "--var",
        ];
        let v = format!("voicebox={url}");
        a.push(&v);
        let t = format!("translation={}", srt.to_str().unwrap());
        a.push("--var");
        a.push(&t);
        a.extend_from_slice(extra);
        cli(&a)
    };
    let o = run(&[]);
    let _ = server.kill();
    let _ = server.wait();
    let r = json_out(&o);
    let text = r.to_string();
    // 3 phrases, the last one cannot fit 0.6 s (a 62-character sentence takes 4 s): flagged, not cut
    assert!(text.contains("3 phrases doublées"), "{text}");
    assert!(text.contains("1 dépassement"), "{text}");
    assert!(text.contains("voix d'origine atténuée de 18 dB"), "{text}");
    let q = sequence_of(data_s, &p);
    let tracks = q["audio"].as_array().unwrap_or_else(|| panic!("{q}"));
    assert_eq!(tracks.len(), 3, "a new audio track after A1 and A2: {q}");
    let work = dir.join("beats-doublage");
    let wavs: Vec<_> =
        std::fs::read_dir(&work).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "wav")).collect();
    assert_eq!(wavs.len(), 1, "one dubbed file: {wavs:?}");
    let report: Value = serde_json::from_slice(&std::fs::read(wavs[0].with_extension("report.json")).unwrap()).unwrap();
    assert_eq!(report["stats"]["n"], 3);
    // the 62-character phrase (4 s of voice) is flagged: even at +15 % it needs more than its 0.6 s
    assert!(report["segments"][2]["overrun"].as_f64().unwrap() > 0.5, "{report}");
    // the audio starts at the first phrase and ends with the last one
    let d = wav_seconds(&wavs[0]);
    assert!(d > 8.0 && d < 13.0, "{d} s");
    let _ = std::fs::remove_dir_all(&dir);
}
