//! End-to-end tests of the CLI binary (headless backend).

use std::io::Write;
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};

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
    // Invalid --bridge is a usage error (exit 2) for both exec and mcp (#921).
    assert_eq!(cli(&["--bridge", "0.0.0.0:9876", "exec", "inspect.project"]).status.code(), Some(2));
    assert_eq!(cli(&["mcp", "--bridge", "0.0.0.0:9876"]).status.code(), Some(2));
}

#[test]
fn version_after_global_options() {
    let want = format!("filmcraft-cli {}\n", env!("CARGO_PKG_VERSION"));
    for args in [&["--version"][..], &["-V"], &["version"], &["--demo", "--version"], &["--compact", "-V"], &["--demo", "version"]] {
        let o = cli(args);
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(String::from_utf8_lossy(&o.stdout), want, "{args:?}");
    }
}

#[test]
fn long_help_matches_help_command() {
    let reference = cli(&["help"]);
    assert!(reference.status.success());
    let long_help = cli(&["--help"]);
    assert!(long_help.status.success(), "--help failed: {}", String::from_utf8_lossy(&long_help.stderr));
    assert_eq!(long_help.stdout, reference.stdout);
    assert!(long_help.stderr.is_empty());
    let short_help = cli(&["-h"]);
    assert!(short_help.status.success());
    assert_eq!(short_help.stdout, reference.stdout);
    let sub_help = cli(&["export", "--help"]);
    assert!(sub_help.status.success(), "export --help failed: {}", String::from_utf8_lossy(&sub_help.stderr));
    assert_eq!(sub_help.stdout, reference.stdout);
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

/// A new folder only this user can enter, never one that already existed in the shared temp dir.
fn private_dir(prefix: &str) -> std::path::PathBuf {
    let mut b = tempfile::Builder::new();
    b.prefix(prefix);
    // tempfile creates with 0777 before the umask, so ask for owner-only
    #[cfg(unix)]
    b.permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700));
    b.tempdir().unwrap().keep()
}

/// A project in a new folder of its own.
fn lock_test_project(tag: &str) -> (std::path::PathBuf, String) {
    let dir = private_dir(&format!("fc-cli-lock-{tag}"));
    let proj = dir.join("p.fcproj").to_str().unwrap().to_string();
    json_out(&cli(&["--demo", "--save-as", &proj, "exec", "file.newBin", "name=start"]));
    (dir, proj)
}

/// The lock of the project file `name`, held by this process.
fn hold_lock(dir: &std::path::Path, name: &str) -> std::fs::File {
    let lock = std::fs::File::options().create(true).truncate(false).read(true).write(true).open(dir.join(format!(".{name}.lock"))).unwrap();
    lock.lock().unwrap();
    lock
}

/// Start a run that has to wait for a lock this process holds; returns once it says it is waiting.
fn start_waiting_run(args: &[&str]) -> std::process::Child {
    use std::io::BufRead;
    let mut child = Command::new(env!("CARGO_BIN_EXE_filmcraft-cli")).args(args).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
    let mut line = String::new();
    std::io::BufReader::new(child.stderr.take().unwrap()).read_line(&mut line).unwrap();
    assert!(line.contains("waiting for another run saving"), "{args:?}: {line}");
    assert!(child.try_wait().unwrap().is_none(), "{args:?} did not wait");
    child
}

#[test]
fn parallel_saves_keep_every_edit() {
    let (dir, proj) = lock_test_project("parallel");
    let names = |writer: usize| (0..300).map(move |i| format!("edit-{writer}-{i}"));
    let mut writers: Vec<_> = (0..4)
        .map(|_| {
            Command::new(env!("CARGO_BIN_EXE_filmcraft-cli"))
                .args(["--project", &proj, "--save", "run", "-"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    // Each writer reads its whole script before it opens the project, so closing the stdins together
    // starts all the edits at once, and long scripts keep each read-to-save span over the others.
    for (n, writer) in writers.iter_mut().enumerate() {
        let script: String = names(n).map(|name| format!("{}\n", json!({"id": "file.newBin", "params": {"name": name}}))).collect();
        writer.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
    }
    for mut writer in writers {
        assert!(writer.wait().unwrap().success());
    }
    let tree = json_out(&cli(&["--project", &proj, "inspect", "project"])).to_string();
    let lost: Vec<String> = (0..4).flat_map(names).filter(|name| !tree.contains(&format!("\"{name}\""))).collect();
    assert!(lost.is_empty(), "lost {} edits: {lost:?}", lost.len());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_save_in_place_waits_for_the_lock_then_edits_the_latest_project() {
    let (dir, proj) = lock_test_project("wait");
    for save in [&["--save"][..], &["--save-as", proj.as_str()]] {
        let held = hold_lock(&dir, "p.fcproj");
        let before = std::fs::read(&proj).unwrap();
        let name = format!("name=waited-{}", save.len());
        let args = [&["--project", proj.as_str()][..], save, &["exec", "file.newBin", &name]].concat();
        let mut child = start_waiting_run(&args);
        assert_eq!(std::fs::read(&proj).unwrap(), before, "{save:?} wrote while another run held the lock");
        // an edit made meanwhile by the holder must survive: the waiter reads only after the lock is free
        json_out(&cli(&["--project", &proj, "--save-as", &format!("{proj}.tmp"), "exec", "file.newBin", &format!("name=held-{}", save.len())]));
        std::fs::rename(format!("{proj}.tmp"), &proj).unwrap();
        drop(held);
        assert!(child.wait().unwrap().success(), "{save:?}");
        let tree = json_out(&cli(&["--project", &proj, "inspect", "project"])).to_string();
        for want in [format!("waited-{}", save.len()), format!("held-{}", save.len())] {
            assert!(tree.contains(&want), "{save:?} lost {want}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A save through a symlink replaces the link. The runs before and after that must share one lock,
/// or a run still waiting on the old one could overwrite a later run's save.
#[cfg(unix)]
#[test]
fn a_symlinked_project_keeps_its_lock_after_a_save() {
    let (dir, proj) = lock_test_project("symlink");
    let link = dir.join("link.fcproj");
    std::os::unix::fs::symlink(&proj, &link).unwrap();
    for name in ["name=first", "name=second"] {
        let held = hold_lock(&dir, "link.fcproj");
        let mut child = start_waiting_run(&["--project", link.to_str().unwrap(), "--save", "exec", "file.newBin", name]);
        drop(held);
        assert!(child.wait().unwrap().success(), "{name}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn runs_that_do_not_save_in_place_ignore_the_lock() {
    let (dir, proj) = lock_test_project("ignore");
    let other = dir.join("other.fcproj").to_str().unwrap().to_string();
    let _held = hold_lock(&dir, "p.fcproj");
    let cases: [&[&str]; 3] = [
        &["--project", &proj, "inspect", "project"],
        &["--project", &proj, "exec", "file.newBin", "name=copy-only"],
        &["--project", &proj, "--save-as", &other, "exec", "file.newBin", "name=copy-only"],
    ];
    for args in cases {
        let mut child = Command::new(env!("CARGO_BIN_EXE_filmcraft-cli")).args(args).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if std::time::Instant::now() > deadline {
                let _ = child.kill();
                panic!("{args:?} is waiting for a lock it doesn't need");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(status.success(), "{args:?}");
    }
    assert!(!std::fs::read_to_string(&proj).unwrap().contains("\"copy-only\""));
    assert!(std::fs::read_to_string(&other).unwrap().contains("\"copy-only\""));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_project_gets_no_lock_file_and_keeps_its_error() {
    let (dir, _) = lock_test_project("missing");
    let gone = dir.join("gone.fcproj").to_str().unwrap().to_string();
    let plain = cli(&["--project", &gone, "inspect", "project"]);
    assert_eq!(plain.status.code(), Some(1));
    let other = dir.join("other.fcproj").to_str().unwrap().to_string();
    // saving in place, saving elsewhere, or not saving: the error is the one opening gives
    for save in [&["--save"][..], &["--save-as", gone.as_str()], &["--save-as", other.as_str()], &[]] {
        let args = [&["--project", gone.as_str()][..], save, &["exec", "file.newBin", "name=copy-only"]].concat();
        let o = cli(&args);
        assert_eq!(o.status.code(), Some(1), "{save:?}");
        assert_eq!(String::from_utf8_lossy(&o.stderr), String::from_utf8_lossy(&plain.stderr), "{save:?}");
    }
    assert!(!dir.join(".gone.fcproj.lock").exists());
    assert!(!dir.join("other.fcproj").exists());
    // --project with --bridge stays a usage error.
    assert_eq!(cli(&["--bridge", "127.0.0.1:1", "--project", &gone, "--save", "exec", "file.newBin", "name=copy-only"]).status.code(), Some(2));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn exec_save_as_then_reopen() {
    let dir = private_dir("fc-cli");
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
    let dir = private_dir("fc-cli-x");
    let out = dir.join("still.wav");
    let o = cli(&["--demo", "export", out.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(std::fs::metadata(&out).map(|m| m.len() > 44).unwrap_or(false));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn run_waits_for_an_export_on_its_last_line() {
    // #2342: `file.exportMedia` as the last line of a script is written before `run` exits
    let dir = private_dir("fc-cli-run-x");
    let out = dir.join("last.wav");
    // `"wait": null` waits too: only an explicit `false` doesn't
    let script = json!({"id": "file.exportMedia", "params": {"path": out.to_str().unwrap(), "format": "wav", "wait": null}}).to_string();
    let mut child =
        Command::new(env!("CARGO_BIN_EXE_filmcraft-cli")).args(["--demo", "run", "-"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(format!("{script}\n").as_bytes()).unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
    assert!(std::fs::metadata(&out).map(|m| m.len() > 44).unwrap_or(false), "{}", String::from_utf8_lossy(&o.stdout));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `export --preset`: a built-in preset by name (dash or en dash), a custom range, a user preset
/// from `--data-dir`, `--list-presets`, `--queue`, and a clean failure for an unknown preset.
#[test]
fn single_frame_in_out_export_warns_in_json_and_stderr() {
    let dir = private_dir("fc-cli-single-frame");
    let project = dir.join("marked.fcproj");
    let script = dir.join("marks.jsonl");
    std::fs::write(&script, "{\"id\":\"markers.markIn\",\"params\":{\"frame\":12}}\n{\"id\":\"markers.markOut\",\"params\":{\"frame\":12}}\n").unwrap();
    let setup = cli(&["--demo", "--save-as", project.to_str().unwrap(), "run", script.to_str().unwrap()]);
    assert!(setup.status.success(), "{}", String::from_utf8_lossy(&setup.stderr));
    for (name, bounds, warned) in [
        ("marked.wav", vec![], true),
        ("explicit.wav", vec!["--start", "0", "--end", "0.04"], false),
        ("queued.wav", vec!["--queue"], true),
        ("queued-explicit.wav", vec!["--queue", "--start", "0", "--end", "0.04"], false),
    ] {
        let path = dir.join(name);
        let mut args = vec!["--project", project.to_str().unwrap(), "export", path.to_str().unwrap()];
        args.extend(bounds);
        let output = cli(&args);
        let v = json_out(&output);
        assert_eq!(v["warnings"].as_array().unwrap().len(), usize::from(warned));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(stderr.contains("In/Out range is 1 frame"), warned, "{stderr}");
        if warned {
            assert!(stderr.contains("--range entire"), "{stderr}");
        }
        assert!(path.exists());
    }
    std::fs::remove_dir_all(dir).unwrap();
}

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
    // an image sequence writes numbered stills: the result and the message name the first and
    // the last, not the path given (#4009)
    let stills = dir.join("a.png");
    let o = cli(&[
        "--demo",
        "--data-dir",
        data_s,
        "export",
        stills.to_str().unwrap(),
        "--format",
        "png",
        "--start",
        "0",
        "--end",
        "0.05",
        "--no-audio",
        "--scale",
        "0.125",
    ]);
    let v = json_out(&o);
    let (first, last) = (v["path"].as_str().unwrap().to_string(), v["lastPath"].as_str().unwrap().to_string());
    assert!(first.ends_with("a000.png") && std::path::Path::new(&first).exists() && std::path::Path::new(&last).exists() && !stills.exists(), "{v}");
    assert!(String::from_utf8_lossy(&o.stderr).contains(&format!("exported {first} to {last} in ")), "{}", String::from_utf8_lossy(&o.stderr));
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
    let dir = private_dir("fc-cli-pipe");
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

/// The bundled extensions of this build, by id (the folder names of `extensions-bundled/`).
const ALL_BUNDLED: [&str; 22] = [
    "autoedit",
    "beat-edit",
    "copy-pasta",
    "doublage-voix",
    "export-en-un-clic",
    "lut-premiere",
    "ml-models",
    "nettoyage-voix",
    "person-matte",
    "reframe-vertical",
    "rife-slowmo",
    "rush-search",
    "silence-remover",
    "sous-titres",
    "sous-titres-animes",
    "stabilizer-l1",
    "storyboard-maker",
    "sync-madvision",
    "tics-de-langage",
    "traduction-sous-titres",
    "upscale-video",
    "voice-music",
];

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
            "doublage-voix",
            "export-en-un-clic",
            "lut-premiere",
            "ml-models",
            "nettoyage-voix",
            "person-matte",
            "reframe-vertical",
            "rife-slowmo",
            "rush-search",
            "silence-remover",
            "sous-titres",
            "sous-titres-animes",
            "stabilizer-l1",
            "storyboard-maker",
            "sync-madvision",
            "tics-de-langage",
            "traduction-sous-titres",
            "upscale-video",
            "voice-music"
        ],
        "{r}"
    );
    // the list is sorted by name and carries the entries
    let l = json_out(&cli(&["--data-dir", data_s, "extensions", "list"]));
    let names: Vec<&str> = l.as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "Agrandir la vidéo x2 (SPAN)",
            "AutoEdit",
            "Beat Edit",
            "Copy Pasta",
            "Doublage vocal",
            "Détourage de personne (RVM)",
            "Export en un clic",
            "LUT vers Premiere",
            "Modèles IA optionnels",
            "Nettoyage de la voix",
            "Ralenti et images par seconde (RIFE)",
            "Recherche de rushes",
            "Reframe vertical",
            "Silence Remover",
            "Sous-titres",
            "Sous-titres animés",
            "Stabilisation L1 (trajectoire optimale)",
            "Storyboard Maker",
            "Synchro audio MadVision",
            "Tics de langage",
            "Traduction des sous-titres",
            "Voix et musique (HT Demucs)"
        ]
    );
    let silence = l.as_array().unwrap().iter().find(|x| x["id"] == "silence-remover").unwrap();
    assert_eq!(silence["entries"].as_array().unwrap().len(), 3);
    assert_eq!(silence["entries"][0]["kind"], "commands");
    let story = l.as_array().unwrap().iter().find(|x| x["id"] == "storyboard-maker").unwrap();
    assert_eq!(story["entries"][0]["kind"], "exec");
    // every command the bundled extensions use exists in the engine
    for id in ALL_BUNDLED {
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
    assert_eq!(l.as_array().unwrap().len(), ALL_BUNDLED.len() - 1);
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

/// `render` with a non-numeric or non-finite `--seconds` / `--scale` is a usage error (exit 2) and
/// writes nothing, instead of silently rendering frame 0 at full scale.
#[test]
fn render_rejects_bad_numbers() {
    let dir = private_dir("fc-cli-render-nan");
    let still = dir.join("s.png").to_str().unwrap().to_string();
    for (opt, bad) in
        [("--seconds", "1s"), ("--seconds", "1,5"), ("--seconds", "abc"), ("--seconds", "NaN"), ("--seconds", "inf"), ("--scale", "xyz"), ("--scale", "NaN")]
    {
        let (secs, scale) = if opt == "--seconds" { (bad, "0.1") } else { ("1", bad) };
        let o = cli(&["--demo", "render", "--seconds", secs, "--scale", scale, "--out", &still]);
        let err = String::from_utf8_lossy(&o.stderr);
        assert_eq!(o.status.code(), Some(2), "{opt} {bad}: {err}");
        assert!(err.contains("filmcraft-cli:") && err.contains(opt), "{opt} {bad}: {err}");
        assert!(!std::path::Path::new(&still).exists(), "{opt} {bad} wrote a file");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// #1054: exporting or rendering a project whose media is missing fails (exit 1) and names the
/// offline items on stderr and in the result's `offlineMedia`, unless `--allow-offline` accepts the
/// placeholder frames; then it still warns.
#[test]
fn export_with_missing_media_fails_unless_allowed() {
    let dir = private_dir("fc-cli-1054");
    let f = |n: &str| dir.join(n).to_str().unwrap().to_string();
    let (still, proj) = (f("still.png"), f("p.fcproj"));
    let o = cli(&["--demo", "render", "--seconds", "1", "--scale", "0.1", "--out", &still]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    json_out(&cli(&["exec", "file.newSequence", "name=s", "width=64", "height=36", "fps=24", "--save-as", &proj]));
    let paths = format!("paths=[\"{}\"]", still.replace('\\', "\\\\"));
    let item = json_out(&cli(&["--project", &proj, "--save", "exec", "file.import", &paths]))["items"][0].as_u64().unwrap();
    json_out(&cli(&["--project", &proj, "--save", "exec", "timeline.place", &format!("item={item}"), "track=V1", "frame=0", "audio=false"]));
    let export = |out: &str, extra: &[&str]| {
        let mut args = vec!["--project", proj.as_str(), "--compact", "export", out, "--format", "png", "--start", "0", "--end", "0.05"];
        args.extend_from_slice(extra);
        cli(&args)
    };

    let on = export(&f("on.png"), &[]);
    let v = json_out(&on);
    assert_eq!(v["offlineMedia"], serde_json::json!([]), "{v}");
    assert!(!String::from_utf8_lossy(&on.stderr).contains("warning"), "{}", String::from_utf8_lossy(&on.stderr));

    std::fs::remove_file(&still).unwrap();
    let off = export(&f("off.png"), &[]);
    let err = String::from_utf8_lossy(&off.stderr);
    assert_eq!(off.status.code(), Some(1), "{err}");
    assert!(err.contains("still.png") && err.contains("missing") && err.contains("--allow-offline"), "{err}");
    let v: Value = serde_json::from_slice(&off.stdout).expect("the result JSON is still printed");
    assert_eq!(v["offlineMedia"][0]["item"], item, "{v}");
    assert_eq!(v["offlineMedia"][0]["status"], "missing", "{v}");

    let allowed = export(&f("allowed.png"), &["--allow-offline"]);
    let err = String::from_utf8_lossy(&allowed.stderr);
    assert!(allowed.status.success(), "{err}");
    assert!(err.contains("still.png") && err.contains("missing"), "still warns: {err}");
    assert_eq!(json_out(&allowed)["offlineMedia"][0]["status"], "missing");

    let render = cli(&["--project", &proj, "render", "--seconds", "0", "--out", &f("r.png")]);
    let err = String::from_utf8_lossy(&render.stderr);
    assert_eq!(render.status.code(), Some(1), "{err}");
    assert!(err.contains("still.png") && err.contains("--allow-offline"), "{err}");
    let render = cli(&["--project", &proj, "render", "--seconds", "0", "--out", &f("r.png"), "--allow-offline"]);
    assert!(render.status.success(), "{}", String::from_utf8_lossy(&render.stderr));
    assert!(String::from_utf8_lossy(&render.stderr).contains("still.png"));
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

#[test]
fn match_shot_colour_from_the_shell() {
    let dir = std::env::temp_dir().join(format!("fc-cli-match-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (proj, cube) = (dir.join("p.fcproj"), dir.join("match.cube"));
    let p = proj.to_str().unwrap();
    json_out(&cli(&["--demo", "exec", "file.newBin", "name=x", "--save-as", p]));
    let seq = json_out(&cli(&["--project", p, "exec", "sequence.inspect"]));
    let items = seq["video"][0]["items"].as_array().unwrap();
    let (a, b) = (items[0]["clip"].as_u64().unwrap(), items.last().unwrap()["clip"].as_u64().unwrap());
    let r = json_out(&cli(&[
        "--project",
        p,
        "--save",
        "exec",
        "color.matchShots",
        &format!("referenceClip={a}"),
        &format!("targetClip={b}"),
        "method=histogram",
        "strength=80",
        &format!("path={}", cube.to_str().unwrap()),
    ]));
    assert_eq!(r["method"], "histogram");
    assert_eq!(r["applied"], true);
    assert!(r["lut"].as_str().unwrap().starts_with("lib:"), "{r}");
    let text = std::fs::read_to_string(&cube).unwrap();
    assert!(text.contains("LUT_3D_SIZE 33"));
    // saved: the library holds the LUT and the target plays it
    let luts = json_out(&cli(&["--project", p, "exec", "lut.list"]));
    assert_eq!(luts["library"].as_array().unwrap().len(), 1, "{luts}");
    let seq = json_out(&cli(&["--project", p, "exec", "sequence.inspect"]));
    assert!(seq.to_string().contains(r["lut"].as_str().unwrap()), "the target's look is the LUT");
    // the same clip twice is a failed command (exit 1), nothing saved
    let bad = cli(&["--project", p, "exec", "color.matchShots", &format!("referenceClip={a}"), &format!("targetClip={a}")]);
    assert_eq!(bad.status.code(), Some(1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_previews_makes_the_light_previews_of_a_folder_and_resumes() {
    let d = std::env::temp_dir().join(format!("fc-cli-previews-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let lib = d.join("lib/A");
    std::fs::create_dir_all(&lib).unwrap();
    // two short WAVs (16-bit mono silence + a click)
    for name in ["a.wav", "b.wav"] {
        let n = 8_000u32;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + n * 2).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&16_000u32.to_le_bytes());
        b.extend_from_slice(&32_000u32.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(n * 2).to_le_bytes());
        for i in 0..n {
            b.extend_from_slice(&(if i == 100 { 20_000i16 } else { 0 }).to_le_bytes());
        }
        std::fs::write(lib.join(name), b).unwrap();
    }
    let root = d.join("lib");
    let cache = d.join("cache");
    let (r, c) = (root.to_str().unwrap(), cache.to_str().unwrap());
    let o = cli(&["assets.buildPreviews", "--kind", "sfx", "--root", r, "--cache", c, "--limit", "1"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("\"done\":true") && out.contains("\"made\":1"), "{out}");
    let o = cli(&["assets.buildPreviews", "--kind", "sfx", "--root", r, "--cache", c]);
    let out = String::from_utf8_lossy(&o.stdout);
    let last = out.lines().last().unwrap_or("");
    assert!(last.contains("\"made\":1") && last.contains("\"skipped\":1"), "resumed: {last}");
    let m4a = std::fs::read_dir(cache.join("previews")).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".m4a")).count();
    assert_eq!(m4a, 2);
    assert!(cache.join("peaks/peaks-v1.bin").exists());
    let o = cli(&["assets.buildPreviews", "--kind", "nope"]);
    assert!(!o.status.success());
    let _ = std::fs::remove_dir_all(&d);
}
