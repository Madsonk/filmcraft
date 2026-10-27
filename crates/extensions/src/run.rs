//! Running an entry.
//!
//! * **Commands / jsonl.** Each step is a FilmCraft command; `${variables}` in its `params` are
//!   replaced from the editing context first (see [`RunContext::lookup`]).
//! * **Exec.** The program gets the context as one JSON object on its standard input and runs in the
//!   extension folder. Every line it prints that is a JSON object with an `"id"` is a command to run
//!   (`{"id":"markers.add","params":{"name":"x"}}`); `{"log":"text"}` and any other line are shown to
//!   the user. Commands run after the program has exited, so a program can plan edits but not read
//!   their results; it is killed after `timeoutSeconds`.
//!
//! The host supplies the closure that really runs a command, so this module is the same in the desktop
//! app (where the closure also handles UI commands) and in the headless CLI.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::manifest::{ActionKind, ExecSpec, MAX_STEPS, Step, check_command_id};
use crate::store::Extension;
use crate::{ExtError, Result, pack};

/// A program's standard output is read up to this size.
pub const MAX_PROGRAM_OUTPUT: usize = 8 * 1024 * 1024;
const MAX_PROGRAM_STDERR: usize = 64 * 1024;
/// Commands a program (or a `.jsonl` file) may hand back.
pub const MAX_RETURNED_STEPS: usize = 5000;
const MAX_JSONL_BYTES: u64 = 4 * 1024 * 1024;
const MAX_VALUE_DEPTH: usize = 32;

/// Where the entry runs: it decides `${blocking}` (export commands wait for their file in the CLI,
/// but start a background job in the window).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Host {
    Cli,
    App,
}

/// The editing context an entry runs in (see `${project}` and friends in the documentation).
#[derive(Clone, Debug)]
pub struct RunContext {
    pub host: Host,
    /// The project file, when it has been saved.
    pub project: Option<String>,
    /// Selected timeline clips (ids).
    pub selection: Vec<u64>,
    pub playhead_ticks: i64,
    pub playhead_seconds: f64,
    pub has_sequence: bool,
    pub data_dir: Option<PathBuf>,
    /// Caller-supplied values, `${var.name}` (CLI `--var name=value`).
    pub vars: BTreeMap<String, Value>,
}

impl RunContext {
    pub fn new(host: Host) -> Self {
        RunContext {
            host,
            project: None,
            selection: Vec::new(),
            playhead_ticks: 0,
            playhead_seconds: 0.0,
            has_sequence: false,
            data_dir: None,
            vars: BTreeMap::new(),
        }
    }

    /// Whether an entry's `requires` are met; the error says what is missing.
    pub fn satisfies(&self, requires: &[String]) -> std::result::Result<(), String> {
        for r in requires {
            match r.as_str() {
                "project" if self.project.is_none() => return Err("save the project first".into()),
                "sequence" if !self.has_sequence => return Err("open a sequence first".into()),
                "selection" if self.selection.is_empty() => return Err("select clips in the timeline first".into()),
                _ => {}
            }
        }
        Ok(())
    }

    /// The project path made absolute (a relative `--project p.fcproj` still has a folder), without
    /// touching the file system.
    fn project_path(&self) -> Option<PathBuf> {
        self.project.as_deref().filter(|p| !p.is_empty()).map(|p| std::path::absolute(p).unwrap_or_else(|_| PathBuf::from(p)))
    }

    /// The value of the variable `name`.
    fn lookup(&self, name: &str, ext_dir: &Path) -> Lookup {
        let project_path = self.project_path();
        let need_project = || Lookup::Unavailable("it needs a saved project (save the project first)".into());
        match name {
            "project" => project_path.as_deref().and_then(Path::to_str).map_or_else(need_project, |p| Lookup::Found(json!(p))),
            "project_dir" => project_path.as_deref().and_then(Path::parent).and_then(Path::to_str).map_or_else(need_project, |p| Lookup::Found(json!(p))),
            "project_name" => project_path.as_deref().and_then(Path::file_stem).and_then(|s| s.to_str()).map_or_else(need_project, |p| Lookup::Found(json!(p))),
            "selection" => Lookup::Found(json!(self.selection)),
            "selection_count" => Lookup::Found(json!(self.selection.len())),
            "selection_first" => self.selection.first().map_or(Lookup::Unavailable("select a clip first".into()), |c| Lookup::Found(json!(c))),
            "selection_rest" if self.selection.len() < 2 => Lookup::Unavailable("select at least two clips (the first is the source)".into()),
            "selection_rest" => Lookup::Found(json!(self.selection[1..])),
            "playhead" => Lookup::Found(json!(self.playhead_seconds)),
            "playhead_ticks" => Lookup::Found(json!(self.playhead_ticks)),
            "extension_dir" => Lookup::Found(json!(ext_dir.to_string_lossy())),
            "data_dir" => {
                self.data_dir.as_ref().map_or(Lookup::Unavailable("there is no data directory".into()), |d| Lookup::Found(json!(d.to_string_lossy())))
            }
            "home" => home_dir().map_or(Lookup::Unavailable("the home directory is unknown".into()), |h| Lookup::Found(json!(h.to_string_lossy()))),
            "temp" => Lookup::Found(json!(std::env::temp_dir().to_string_lossy())),
            "blocking" => Lookup::Found(json!(self.host == Host::Cli)),
            "host" => Lookup::Found(json!(match self.host {
                Host::Cli => "cli",
                Host::App => "app",
            })),
            other => match other.strip_prefix("var.") {
                Some(k) => self.vars.get(k).cloned().map_or(Lookup::Unavailable(format!("no value was given for `{k}`")), Lookup::Found),
                None => Lookup::Unknown,
            },
        }
    }

    /// The JSON an `exec` program reads on its standard input.
    pub fn to_json(&self, ext: &Extension, entry: usize) -> Value {
        let project_path = self.project_path();
        let project_path = project_path.as_deref();
        json!({
            "formatVersion": 1,
            "host": match self.host { Host::Cli => "cli", Host::App => "app" },
            "extension": {"id": ext.manifest.id, "version": ext.manifest.version, "dir": ext.dir.to_string_lossy()},
            "entry": {"index": entry, "label": ext.manifest.entries.get(entry).map(|e| e.label.as_str())},
            "project": project_path.and_then(Path::to_str),
            "projectDir": project_path.and_then(Path::parent).and_then(Path::to_str),
            "projectName": project_path.and_then(Path::file_stem).and_then(|s| s.to_str()),
            "selection": self.selection,
            "playheadSeconds": self.playhead_seconds,
            "playheadTicks": self.playhead_ticks,
            "hasSequence": self.has_sequence,
            "dataDir": self.data_dir.as_ref().map(|d| d.to_string_lossy().to_string()),
            "vars": self.vars,
        })
    }
}

fn home_dir() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"].iter().find_map(|k| std::env::var_os(k).filter(|v| !v.is_empty())).map(PathBuf::from)
}

enum Lookup {
    Found(Value),
    Unavailable(String),
    Unknown,
}

// ------------------------------------------------------------------------------------ substitution

/// `${a|b|"literal"}`: the first alternative that has a value.
fn resolve(expr: &str, lk: &dyn Fn(&str) -> Lookup) -> std::result::Result<Value, String> {
    let mut why = String::new();
    for alt in expr.split('|') {
        let alt = alt.trim();
        if let Some(lit) = alt.strip_prefix('"').and_then(|a| a.strip_suffix('"')) {
            return Ok(Value::String(lit.to_string()));
        }
        // a number or true/false is a literal of that type: ${var.min|0.5}
        if let Ok(v @ (Value::Number(_) | Value::Bool(_))) = serde_json::from_str::<Value>(alt) {
            return Ok(v);
        }
        match lk(alt) {
            Lookup::Found(v) => return Ok(v),
            Lookup::Unavailable(m) => {
                if why.is_empty() {
                    why = format!("${{{alt}}} is not available: {m}");
                }
            }
            Lookup::Unknown => return Err(format!("unknown variable ${{{alt}}}")),
        }
    }
    Err(why)
}

fn as_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A string with variables: exactly one `${x}` keeps the value's type (a number stays a number, the
/// selection stays an array); otherwise the values are written into the text. `$${` is a literal `${`.
fn subst_str(s: &str, lk: &dyn Fn(&str) -> Lookup) -> std::result::Result<Value, String> {
    if let Some(inner) = s.strip_prefix("${").and_then(|r| r.strip_suffix('}'))
        && !inner.contains("${")
        && !inner.contains('}')
    {
        return resolve(inner, lk);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find("$") {
        out.push_str(rest.get(..at).unwrap_or_default());
        let tail = rest.get(at..).unwrap_or_default();
        if let Some(after) = tail.strip_prefix("$${") {
            out.push_str("${");
            rest = after;
        } else if let Some(after) = tail.strip_prefix("${") {
            let Some(end) = after.find('}') else { return Err(format!("`${{` is never closed in `{s}`")) };
            let expr = after.get(..end).unwrap_or_default();
            out.push_str(&as_text(&resolve(expr, lk)?));
            rest = after.get(end + 1..).unwrap_or_default();
        } else {
            out.push('$');
            rest = tail.get(1..).unwrap_or_default();
        }
    }
    out.push_str(rest);
    Ok(Value::String(out))
}

fn subst_value(v: &Value, lk: &dyn Fn(&str) -> Lookup, depth: usize) -> std::result::Result<Value, String> {
    if depth > MAX_VALUE_DEPTH {
        return Err("parameters are nested too deeply".into());
    }
    Ok(match v {
        Value::String(s) => subst_str(s, lk)?,
        Value::Array(a) => Value::Array(a.iter().map(|x| subst_value(x, lk, depth + 1)).collect::<std::result::Result<_, _>>()?),
        Value::Object(m) => {
            let mut out = Map::new();
            for (k, x) in m {
                out.insert(k.clone(), subst_value(x, lk, depth + 1)?);
            }
            Value::Object(out)
        }
        other => other.clone(),
    })
}

// ------------------------------------------------------------------------------------ jsonl / program output

/// Parse a `.jsonl` command file: one `{"id","params"}` per line; blank lines and `#` comments skipped.
pub fn parse_jsonl(text: &str) -> std::result::Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let step: Step = serde_json::from_str(line).map_err(|e| format!("line {}: {e}", n + 1))?;
        check_command_id(&step.id).map_err(|e| format!("line {}: {e}", n + 1))?;
        if step.params.as_ref().is_some_and(|p| !p.is_object()) {
            return Err(format!("line {}: `params` must be an object", n + 1));
        }
        steps.push(step);
        if steps.len() > MAX_RETURNED_STEPS {
            return Err(format!("more than {MAX_RETURNED_STEPS} commands"));
        }
    }
    Ok(steps)
}

/// What a program printed: commands to run, and text to show.
fn parse_program_output(text: &str) -> std::result::Result<(Vec<Step>, Vec<String>), String> {
    let (mut steps, mut log) = (Vec::new(), Vec::new());
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(line) else {
            log.push(line.to_string());
            continue;
        };
        if let Some(Value::String(id)) = obj.get("id") {
            check_command_id(id).map_err(|e| format!("the program printed a bad command: {e}"))?;
            let params = match obj.get("params") {
                None | Some(Value::Null) => None,
                Some(p) if p.is_object() => Some(p.clone()),
                Some(_) => return Err(format!("the program printed `{id}` with `params` that is not an object")),
            };
            steps.push(Step { id: id.clone(), params, continue_on_error: obj.get("continueOnError").and_then(Value::as_bool).unwrap_or(false) });
            if steps.len() > MAX_RETURNED_STEPS {
                return Err(format!("the program printed more than {MAX_RETURNED_STEPS} commands"));
            }
        } else if let Some(Value::String(m)) = obj.get("log") {
            log.push(m.clone());
        } else {
            log.push(line.to_string());
        }
    }
    Ok((steps, log))
}

// ------------------------------------------------------------------------------------ report

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepReport {
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub result: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunReport {
    pub extension: String,
    pub entry: String,
    pub kind: ActionKind,
    pub steps: Vec<StepReport>,
    /// Text the program printed, and its notes.
    pub log: Vec<String>,
    /// The first failure that stopped the run (None = everything ran).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RunReport {
    pub fn ok(&self) -> bool {
        self.error.is_none()
    }

    /// One line for a status bar: what the commands report about their work (`summary` in their
    /// results: « 63 marqueurs posés, 120 BPM »), joined with " · "; the failing step's message when the
    /// run stopped; the number of commands when none has anything to say.
    pub fn summary(&self) -> String {
        match &self.error {
            Some(e) => match self.steps.last() {
                Some(StepReport { ok: false, error: Some(m), .. }) => format!("{}: {m}", self.entry),
                _ => format!("{}: {e}", self.entry),
            },
            None => {
                let lines = self.step_summaries();
                if lines.is_empty() {
                    let tail = self.log.last().map(|l| format!(" ({l})")).unwrap_or_default();
                    format!("{}: {} command{} done{tail}", self.entry, self.steps.len(), if self.steps.len() == 1 { "" } else { "s" })
                } else {
                    format!("{}: {}", self.entry, lines.join(" · "))
                }
            }
        }
    }

    /// The `summary` line of each step that has one, in order.
    pub fn step_summaries(&self) -> Vec<&str> {
        self.steps.iter().filter(|s| s.ok).filter_map(|s| s.result.get("summary").and_then(Value::as_str)).collect()
    }
}

// ------------------------------------------------------------------------------------ running

/// Run entry number `entry` of `ext`. `exec` runs one FilmCraft command (`id`, `params`) for the host.
///
/// `Err` means the entry could not even start (unknown entry, a `requires` not met, the `.jsonl` file or
/// the program unusable, a variable with no value); once commands are running, a failure is reported in
/// [`RunReport::error`] together with the steps done so far.
pub fn run_entry(
    ext: &Extension,
    entry: usize,
    ctx: &RunContext,
    exec: &mut dyn FnMut(&str, Value) -> std::result::Result<Value, String>,
) -> Result<RunReport> {
    let e = ext.manifest.entries.get(entry).ok_or_else(|| ExtError::Run(format!("`{}` has no entry number {entry}", ext.manifest.id)))?;
    ctx.satisfies(&e.requires).map_err(|m| ExtError::Run(format!("{}: {m}", e.label)))?;
    let kind = e.action.kind().ok_or_else(|| ExtError::Run(format!("{}: the entry has no action", e.label)))?;
    let lk = |name: &str| ctx.lookup(name, &ext.dir);
    let err = |m: String| ExtError::Run(format!("{}: {m}", e.label));
    let mut log = Vec::new();

    // (step, substitute?) — steps written by the extension get their variables replaced; commands a
    // program printed are taken as they are.
    let (steps, substitute): (Vec<Step>, bool) = match kind {
        ActionKind::Commands => (e.action.commands.clone().unwrap_or_default(), true),
        ActionKind::Jsonl => {
            let rel = e.action.jsonl.as_deref().unwrap_or_default();
            let path = pack::inside(&ext.dir, rel).map_err(|x| err(x.to_string()))?;
            let size = std::fs::metadata(&path).map_err(|x| err(format!("{rel}: {x}")))?.len();
            if size > MAX_JSONL_BYTES {
                return Err(err(format!("{rel} is larger than {} MiB", MAX_JSONL_BYTES / (1024 * 1024))));
            }
            let text = std::fs::read_to_string(&path).map_err(|x| err(format!("{rel}: {x}")))?;
            (parse_jsonl(&text).map_err(|m| err(format!("{rel}: {m}")))?, true)
        }
        ActionKind::Exec => {
            let spec = e.action.exec.as_ref().ok_or_else(|| err("no program".into()))?;
            let (steps, out) = run_program(ext, spec, &ctx.to_json(ext, entry), &lk).map_err(err)?;
            log = out;
            (steps, false)
        }
    };

    let mut report = RunReport { extension: ext.manifest.id.clone(), entry: e.label.clone(), kind, steps: Vec::new(), log, error: None };
    if steps.len() > MAX_RETURNED_STEPS.max(MAX_STEPS) {
        return Err(err(format!("{} commands (at most {})", steps.len(), MAX_RETURNED_STEPS.max(MAX_STEPS))));
    }
    for (n, step) in steps.iter().enumerate() {
        let params = match (&step.params, substitute) {
            (None, _) => json!({}),
            (Some(p), true) => match subst_value(p, &lk, 0) {
                Ok(v) => v,
                Err(m) => {
                    let msg = format!("step {} ({}): {m}", n + 1, step.id);
                    report.steps.push(StepReport { id: step.id.clone(), ok: false, result: Value::Null, error: Some(msg.clone()) });
                    report.error = Some(msg);
                    return Ok(report);
                }
            },
            (Some(p), false) => p.clone(),
        };
        let outcome = catch_unwind(AssertUnwindSafe(|| exec(&step.id, params))).unwrap_or_else(|p| {
            Err(format!(
                "internal error: {}",
                p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "panic".into())
            ))
        });
        match outcome {
            Ok(result) => {
                if let Some(line) = result.get("summary").and_then(Value::as_str) {
                    report.log.push(format!("{}: {line}", step.id));
                }
                report.steps.push(StepReport { id: step.id.clone(), ok: true, result, error: None });
            }
            Err(m) => {
                report.steps.push(StepReport { id: step.id.clone(), ok: false, result: Value::Null, error: Some(m.clone()) });
                if !step.continue_on_error {
                    report.error = Some(format!("step {} ({}) failed: {m}", n + 1, step.id));
                    return Ok(report);
                }
                report.log.push(format!("step {} ({}) failed, continuing: {m}", n + 1, step.id));
            }
        }
    }
    Ok(report)
}

/// A child's output pipe read by its own thread into a shared buffer (up to `cap` bytes; the rest is
/// drained and dropped). The reader is not joined: a program that leaves a background process holding
/// the pipe open can't keep FilmCraft waiting, whatever was read in time is used.
struct Pipe {
    buf: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    over: std::sync::Arc<std::sync::atomic::AtomicBool>,
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Pipe {
    fn start(mut r: impl Read + Send + 'static, cap: usize) -> Pipe {
        use std::sync::atomic::Ordering::Relaxed;
        use std::sync::{Arc, Mutex, PoisonError};
        let p = Pipe { buf: Arc::new(Mutex::new(Vec::new())), over: Arc::default(), done: Arc::default() };
        let (buf, over, done) = (p.buf.clone(), p.over.clone(), p.done.clone());
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            while let Ok(n) = r.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                let mut b = buf.lock().unwrap_or_else(PoisonError::into_inner);
                let room = cap.saturating_sub(b.len());
                b.extend_from_slice(chunk.get(..n.min(room)).unwrap_or_default());
                if n > room {
                    over.store(true, Relaxed);
                }
            }
            done.store(true, Relaxed);
        });
        p
    }

    /// Wait a moment for the end of the stream, then take what was read: (bytes, overflowed).
    fn finish(&self) -> (Vec<u8>, bool) {
        use std::sync::atomic::Ordering::Relaxed;
        let until = Instant::now() + Duration::from_secs(2);
        while !self.done.load(Relaxed) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        let b = self.buf.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        (b, self.over.load(Relaxed))
    }
}

fn run_program(ext: &Extension, spec: &ExecSpec, context: &Value, lk: &dyn Fn(&str) -> Lookup) -> std::result::Result<(Vec<Step>, Vec<String>), String> {
    let mut args = Vec::new();
    for a in &spec.args {
        args.push(as_text(&subst_str(a, lk)?));
    }
    let program: PathBuf = if spec.program.contains('/') {
        let p = pack::inside(&ext.dir, &spec.program).map_err(|_| format!("the program {} is not in the extension (or points outside it)", spec.program))?;
        if !p.is_file() {
            return Err(format!("the program {} is not in the extension", spec.program));
        }
        p
    } else {
        PathBuf::from(&spec.program)
    };
    let mut cmd = Command::new(&program);
    cmd.args(&args)
        .current_dir(&ext.dir)
        .env("FILMCRAFT_EXTENSION_DIR", &ext.dir)
        .env("FILMCRAFT_EXTENSION_ID", &ext.manifest.id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // its own process group, so a timeout can stop the helpers it started too
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("the program `{}` was not found", spec.program),
        std::io::ErrorKind::PermissionDenied => format!("the program `{}` is not executable (chmod +x it, or call it through `sh`)", spec.program),
        _ => format!("can't start `{}`: {e}", spec.program),
    })?;
    // the three pipes are served by their own threads, so a program that never reads its input, or
    // fills its output, can't block FilmCraft
    let stdin_thread = child.stdin.take().map(|mut si| {
        let text = format!("{context}\n");
        std::thread::spawn(move || {
            let _ = si.write_all(text.as_bytes());
        })
    });
    let out_pipe = child.stdout.take().map(|so| Pipe::start(so, MAX_PROGRAM_OUTPUT));
    let err_pipe = child.stderr.take().map(|se| Pipe::start(se, MAX_PROGRAM_STDERR));
    let deadline = Instant::now() + Duration::from_secs(spec.timeout_seconds);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Ok(s),
            Ok(None) if Instant::now() >= deadline => {
                #[cfg(unix)]
                let _ = Command::new("kill").args(["-s", "KILL", "--"]).arg(format!("-{}", child.id())).stdout(Stdio::null()).stderr(Stdio::null()).status();
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("`{}` did not finish within {} s and was stopped", spec.program, spec.timeout_seconds));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => break Err(format!("waiting for `{}`: {e}", spec.program)),
        }
    };
    drop(stdin_thread);
    let (stdout, over) = out_pipe.map(|p| p.finish()).unwrap_or_default();
    let (stderr, _) = err_pipe.map(|p| p.finish()).unwrap_or_default();
    let stderr = String::from_utf8_lossy(&stderr).trim().to_string();
    let status = status?;
    if !status.success() {
        let why = if stderr.is_empty() {
            String::new()
        } else {
            format!(": {}", stderr.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" / "))
        };
        return Err(format!("`{}` failed ({status}){why}", spec.program));
    }
    if over {
        return Err(format!("`{}` printed more than {} MiB", spec.program, MAX_PROGRAM_OUTPUT / (1024 * 1024)));
    }
    let (steps, mut log) = parse_program_output(&String::from_utf8_lossy(&stdout))?;
    if !stderr.is_empty() {
        log.push(format!("stderr: {}", stderr.lines().next().unwrap_or_default()));
    }
    Ok((steps, log))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;
    use crate::pack::tests::temp;
    use std::fs;

    fn ext_with(dir: &Path, entries: Value) -> Extension {
        let m = json!({"id": "demo", "name": "Demo", "version": "1.0.0", "entries": entries});
        fs::write(dir.join("extension.json"), m.to_string()).unwrap();
        Extension { manifest: Manifest::load(dir).unwrap(), dir: dir.to_path_buf() }
    }

    fn ctx() -> RunContext {
        let mut c = RunContext::new(Host::Cli);
        c.project = Some("/Users/me/Movies/My Film.fcproj".into());
        c.selection = vec![4, 9];
        c.playhead_seconds = 12.5;
        c.playhead_ticks = 12;
        c.has_sequence = true;
        c.vars.insert("lang".into(), json!("fr"));
        c
    }

    /// Record every command; fail the ones named `fail.*`.
    fn recorder(log: &mut Vec<(String, Value)>) -> impl FnMut(&str, Value) -> std::result::Result<Value, String> + '_ {
        move |id, p| {
            log.push((id.to_string(), p));
            if id.starts_with("fail.") { Err("boom".into()) } else { Ok(json!({"done": id})) }
        }
    }

    #[test]
    fn variables_keep_their_type_or_are_written_into_text() {
        let dir = temp("vars");
        let e = ext_with(
            &dir,
            json!([{"label": "Go", "action": {"commands": [{"id": "a.b", "params": {
                "sel": "${selection}", "t": "${playhead}", "path": "${project_dir}/${project_name}.srt",
                "mix": ["${blocking}", "x${var.lang}y"], "lit": "$${project}", "n": 3,
                "alt": "${var.missing|project_name}", "dflt": "${var.missing|\"none\"}"
            }}]}}]),
        );
        let mut log = Vec::new();
        let r = run_entry(&e, 0, &ctx(), &mut recorder(&mut log)).unwrap();
        assert!(r.ok(), "{:?}", r.error);
        let p = &log[0].1;
        assert_eq!(p["sel"], json!([4, 9]));
        assert_eq!(subst_str("${selection_first}", &|n| ctx().lookup(n, Path::new("/"))).unwrap(), json!(4));
        assert_eq!(subst_str("${selection_rest}", &|n| ctx().lookup(n, Path::new("/"))).unwrap(), json!([9]));
        let mut one = ctx();
        one.selection = vec![4];
        assert!(subst_str("${selection_rest}", &|n| one.lookup(n, Path::new("/"))).is_err());
        assert_eq!(p["t"], json!(12.5));
        assert_eq!(p["path"], json!("/Users/me/Movies/My Film.srt"));
        assert_eq!(p["mix"], json!([true, "xfry"]));
        assert_eq!(p["lit"], json!("${project}"));
        assert_eq!(p["n"], json!(3));
        assert_eq!(p["alt"], json!("My Film"));
        assert_eq!(p["dflt"], json!("none"));
        assert_eq!(subst_str("${var.min|0.5}", &|n| ctx().lookup(n, Path::new("/"))).unwrap(), json!(0.5));
        assert_eq!(subst_str("${var.lang|0.5}", &|n| ctx().lookup(n, Path::new("/"))).unwrap(), json!("fr"));
        assert_eq!(subst_str("x${var.min|2}", &|n| ctx().lookup(n, Path::new("/"))).unwrap(), json!("x2"));
        assert_eq!(subst_str("${var.flag|true}", &|n| ctx().lookup(n, Path::new("/"))).unwrap(), json!(true));
    }

    /// `--project p.fcproj` (relative) must not turn `${project_dir}` into an empty string, which
    /// `"${project_dir}/"` would make the root of the file system.
    #[test]
    fn a_relative_project_path_gets_an_absolute_folder() {
        let dir = temp("relproj");
        let e = ext_with(
            &dir,
            json!([{"label": "Go", "action": {"commands": [{"id": "a.b", "params": {"dir": "${project_dir}/", "name": "${project_name}", "p": "${project}"}}]}}]),
        );
        let mut c = ctx();
        c.project = Some("film.fcproj".into());
        let mut log = Vec::new();
        run_entry(&e, 0, &c, &mut recorder(&mut log)).unwrap();
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(log[0].1["dir"], json!(format!("{}/", cwd.display())));
        assert_eq!(log[0].1["name"], json!("film"));
        assert_eq!(log[0].1["p"], json!(cwd.join("film.fcproj").to_string_lossy()));
        c.project = Some(String::new());
        let r = run_entry(&e, 0, &c, &mut recorder(&mut log)).unwrap();
        assert!(r.error.unwrap().contains("save the project"), "an empty path is no project");
    }

    #[test]
    fn missing_and_unknown_variables_stop_the_step_with_a_clear_message() {
        let dir = temp("novars");
        let e = ext_with(&dir, json!([{"label": "Go", "action": {"commands": [{"id": "a.b", "params": {"p": "${project}"}}, {"id": "c.d"}]}}]));
        let mut log = Vec::new();
        let r = run_entry(&e, 0, &RunContext::new(Host::App), &mut recorder(&mut log)).unwrap();
        assert!(log.is_empty());
        assert!(r.error.as_ref().unwrap().contains("${project} is not available") && r.error.as_ref().unwrap().contains("save the project"), "{:?}", r.error);
        let e = ext_with(&dir, json!([{"label": "Go", "action": {"commands": [{"id": "a.b", "params": {"p": "${nope}"}}]}}]));
        let r = run_entry(&e, 0, &ctx(), &mut recorder(&mut log)).unwrap();
        assert!(r.error.unwrap().contains("unknown variable ${nope}"));
        let e = ext_with(&dir, json!([{"label": "Go", "action": {"commands": [{"id": "a.b", "params": {"p": "${oops"}}]}}]));
        assert!(run_entry(&e, 0, &ctx(), &mut recorder(&mut log)).unwrap().error.unwrap().contains("never closed"));
    }

    #[test]
    fn steps_stop_at_the_first_failure_unless_told_to_continue() {
        let dir = temp("fail");
        let e = ext_with(
            &dir,
            json!([
                {"label": "Stops", "action": {"commands": [{"id": "a.one"}, {"id": "fail.two"}, {"id": "a.three"}]}},
                {"label": "Goes on", "action": {"commands": [{"id": "fail.one", "continueOnError": true}, {"id": "a.two"}]}}
            ]),
        );
        let mut log = Vec::new();
        let r = run_entry(&e, 0, &ctx(), &mut recorder(&mut log)).unwrap();
        assert_eq!(log.len(), 2);
        assert!(r.error.as_ref().unwrap().contains("step 2 (fail.two) failed: boom"));
        assert_eq!(r.steps.len(), 2);
        let mut log = Vec::new();
        let r = run_entry(&e, 1, &ctx(), &mut recorder(&mut log)).unwrap();
        assert!(r.ok());
        assert_eq!(r.steps.iter().map(|s| s.ok).collect::<Vec<_>>(), [false, true]);
        assert!(r.summary().contains("2 commands done"));
        // a panicking host command is a failed step, not a crash
        let r = run_entry(&e, 0, &ctx(), &mut |_, _| -> std::result::Result<Value, String> { panic!("host bug") }).unwrap();
        assert!(r.error.unwrap().contains("internal error: host bug"));
    }

    #[test]
    fn requirements_and_bad_entries_are_reported() {
        let dir = temp("req");
        let e = ext_with(&dir, json!([{"label": "Needs", "requires": ["selection", "project"], "action": {"commands": [{"id": "a.b"}]}}]));
        let mut log = Vec::new();
        let mut c = ctx();
        c.selection.clear();
        assert!(run_entry(&e, 0, &c, &mut recorder(&mut log)).unwrap_err().to_string().contains("select clips"));
        assert!(run_entry(&e, 7, &ctx(), &mut recorder(&mut log)).unwrap_err().to_string().contains("no entry number 7"));
        assert!(ctx().satisfies(&["sequence".into()]).is_ok());
        assert!(RunContext::new(Host::App).satisfies(&["sequence".into()]).is_err());
    }

    #[test]
    fn jsonl_files_run_with_variables() {
        let dir = temp("jsonl");
        fs::write(dir.join("steps.jsonl"), "# a comment\n\n{\"id\": \"markers.add\", \"params\": {\"name\": \"${var.lang}\"}}\n{\"id\": \"edit.undo\"}\n")
            .unwrap();
        let e = ext_with(&dir, json!([{"label": "J", "action": {"jsonl": "steps.jsonl"}}]));
        let mut log = Vec::new();
        let r = run_entry(&e, 0, &ctx(), &mut recorder(&mut log)).unwrap();
        assert!(r.ok());
        assert_eq!(log[0], ("markers.add".to_string(), json!({"name": "fr"})));
        assert_eq!(log[1].1, json!({}));
        fs::write(dir.join("steps.jsonl"), "{\"id\": \"a.b\"}\nnot json\n").unwrap();
        assert!(run_entry(&e, 0, &ctx(), &mut recorder(&mut log)).unwrap_err().to_string().contains("line 2"));
        assert!(parse_jsonl("{\"id\": \"a.b\", \"params\": 3}").unwrap_err().contains("object"));
        assert!(parse_jsonl("{\"id\": \"a b\"}").is_err());
        assert!(parse_jsonl("{\"id\": \"a.b\", \"extra\": 1}").is_err());
    }

    #[test]
    fn jsonl_cannot_escape_the_extension_folder() {
        let dir = temp("escape");
        let outside = temp("outside");
        fs::write(outside.join("x.jsonl"), "{\"id\": \"a.b\"}\n").unwrap();
        let e = ext_with(&dir, json!([{"label": "J", "action": {"jsonl": "x.jsonl"}}]));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.join("x.jsonl"), dir.join("x.jsonl")).unwrap();
            let mut log = Vec::new();
            let e2 = run_entry(&e, 0, &ctx(), &mut recorder(&mut log)).unwrap_err().to_string();
            assert!(e2.contains("outside the extension"), "{e2}");
        }
    }

    #[cfg(unix)]
    fn script(dir: &Path, name: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        fs::write(dir.join(name), format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(dir.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_program_gets_the_context_and_hands_commands_back() {
        let dir = temp("prog");
        fs::create_dir_all(dir.join("scripts")).unwrap();
        script(
            &dir,
            "scripts/run.sh",
            r#"input=$(cat)
echo "{\"log\": \"saw $FILMCRAFT_EXTENSION_ID $1\"}"
echo "plain text line"
echo "{\"id\": \"markers.add\", \"params\": {\"ctx\": $input}}"
echo "oops" >&2"#,
        );
        let e = ext_with(&dir, json!([{"label": "P", "action": {"exec": {"program": "scripts/run.sh", "args": ["${project_name}"]}}}]));
        let mut log = Vec::new();
        let r = run_entry(&e, 0, &ctx(), &mut recorder(&mut log)).unwrap();
        assert!(r.ok(), "{:?}", r.error);
        assert_eq!(log.len(), 1);
        let c = &log[0].1["ctx"];
        assert_eq!(c["project"], json!("/Users/me/Movies/My Film.fcproj"));
        assert_eq!(c["selection"], json!([4, 9]));
        assert_eq!(c["playheadSeconds"], json!(12.5));
        assert_eq!(c["host"], json!("cli"));
        assert_eq!(c["vars"]["lang"], json!("fr"));
        assert!(r.log.iter().any(|l| l == "saw demo My Film"), "{:?}", r.log);
        assert!(r.log.iter().any(|l| l == "plain text line"));
        assert!(r.log.iter().any(|l| l.starts_with("stderr: oops")));
    }

    #[cfg(unix)]
    #[test]
    fn program_failures_are_explained() {
        let dir = temp("progfail");
        fs::create_dir_all(dir.join("s")).unwrap();
        script(&dir, "s/fail.sh", "echo 'it broke' >&2\nexit 3");
        script(&dir, "s/slow.sh", "sleep 30");
        script(&dir, "s/spam.sh", "yes | head -c 9000000");
        script(&dir, "s/bad.sh", "echo '{\"id\": \"ok.cmd\", \"params\": 5}'");
        script(&dir, "s/deaf.sh", "echo '{\"log\": \"never read stdin\"}'");
        let e = ext_with(
            &dir,
            json!([
                {"label": "Fail", "action": {"exec": {"program": "s/fail.sh"}}},
                {"label": "Slow", "action": {"exec": {"program": "s/slow.sh", "timeoutSeconds": 1}}},
                {"label": "Spam", "action": {"exec": {"program": "s/spam.sh"}}},
                {"label": "Bad", "action": {"exec": {"program": "s/bad.sh"}}},
                {"label": "Deaf", "action": {"exec": {"program": "s/deaf.sh"}}},
                {"label": "Missing", "action": {"exec": {"program": "s/none.sh"}}},
                {"label": "NoExec", "action": {"exec": {"program": "definitely-not-a-program-xyz"}}}
            ]),
        );
        let msg = |n: usize| run_entry(&e, n, &ctx(), &mut |_, _| Ok(Value::Null)).map(|r| r.log.join("|")).unwrap_err().to_string();
        assert!(msg(0).contains("failed") && msg(0).contains("it broke"), "{}", msg(0));
        let t0 = Instant::now();
        assert!(msg(1).contains("did not finish within 1 s"), "{}", msg(1));
        assert!(t0.elapsed() < Duration::from_secs(10));
        assert!(msg(2).contains("more than 8 MiB"), "{}", msg(2));
        assert!(msg(3).contains("not an object"), "{}", msg(3));
        let r = run_entry(&e, 4, &ctx(), &mut |_, _| Ok(Value::Null)).unwrap();
        assert!(r.ok() && r.log == ["never read stdin"], "a program that ignores its input is fine");
        assert!(msg(5).contains("is not in the extension"), "{}", msg(5));
        assert!(msg(6).contains("not found"), "{}", msg(6));
    }

    #[test]
    fn output_parsing_is_forgiving_about_text_and_strict_about_commands() {
        let (steps, log) = parse_program_output("hello\n{\"log\": \"hi\"}\n{\"other\": 1}\n\n# c\n{\"id\": \"a.b\"}\n[1,2]\n").unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(log, ["hello", "hi", "{\"other\": 1}", "[1,2]"]);
        assert!(parse_program_output("{\"id\": \"no good\"}").is_err());
        assert!(parse_program_output("{\"id\": 5}").unwrap().0.is_empty());
    }

    #[test]
    fn substitution_is_bounded_and_never_panics() {
        let lk = |n: &str| if n == "x" { Lookup::Found(json!("v")) } else { Lookup::Unknown };
        let mut deep = json!("${x}");
        for _ in 0..100 {
            deep = json!([deep]);
        }
        assert!(subst_value(&deep, &lk, 0).unwrap_err().contains("nested"));
        for s in ["", "$", "$$", "${", "${}", "$${", "a$b", "${x}${x}", "é${x}é", "${x", "}${x}{", "$${x}"] {
            let _ = subst_str(s, &lk);
        }
        assert_eq!(subst_str("é${x}é", &lk).unwrap(), json!("évé"));
        assert_eq!(subst_str("a$b", &lk).unwrap(), json!("a$b"));
    }

    /// The status line is what the commands say about their work, step by step; a failed step shows its own
    /// message; without any `summary` the count of commands remains.
    #[test]
    fn the_summary_is_what_the_commands_report() {
        let dir = temp("summary");
        let e = ext_with(
            &dir,
            json!([
                {"label": "Montage", "action": {"commands": [{"id": "a.one"}, {"id": "a.two"}, {"id": "a.plain"}]}},
                {"label": "Fails", "action": {"commands": [{"id": "a.one"}, {"id": "fail.two"}]}}
            ]),
        );
        let mut talk = |id: &str, _p: Value| -> std::result::Result<Value, String> {
            match id {
                "a.one" => Ok(json!({"summary": "28 mots transcrits"})),
                "a.two" => Ok(json!({"summary": "2 hésitations supprimées"})),
                "fail.two" => Err("0 beat détecté dans ce clip".into()),
                _ => Ok(json!({})),
            }
        };
        let r = run_entry(&e, 0, &ctx(), &mut talk).unwrap();
        assert_eq!(r.summary(), "Montage: 28 mots transcrits · 2 hésitations supprimées");
        assert_eq!(r.log, ["a.one: 28 mots transcrits", "a.two: 2 hésitations supprimées"]);
        let r = run_entry(&e, 1, &ctx(), &mut talk).unwrap();
        assert!(!r.ok());
        assert_eq!(r.summary(), "Fails: 0 beat détecté dans ce clip");
        let mut mute = |_: &str, _: Value| -> std::result::Result<Value, String> { Ok(json!({})) };
        assert_eq!(run_entry(&e, 0, &ctx(), &mut mute).unwrap().summary(), "Montage: 3 commands done");
    }
}
