//! `extension.json`: the manifest of an extension, parsed and validated strictly.
//!
//! ```json
//! {
//!   "formatVersion": 1,
//!   "id": "silence-remover",
//!   "name": "Silence Remover",
//!   "version": "1.0.0",
//!   "description": "Cuts the pauses out of the sequence.",
//!   "author": "Someone",
//!   "entries": [
//!     {
//!       "label": "Remove silences",
//!       "requires": ["sequence"],
//!       "action": { "commands": [
//!         { "id": "sequence.transcribe" },
//!         { "id": "transcript.removePauses", "params": { "minSeconds": 0.6 } }
//!       ] }
//!     }
//!   ]
//! }
//! ```
//!
//! An entry's `action` has exactly one of `commands` (inline list), `jsonl` (a `.jsonl` file of
//! `{"id","params"}` lines inside the extension) or `exec` (an external program). Unknown fields are
//! errors, so a typo is reported instead of silently ignored.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ExtError, Result};

/// The manifest's file name.
pub const MANIFEST_FILE: &str = "extension.json";
/// The only manifest format understood so far.
pub const FORMAT_VERSION: u32 = 1;
/// Larger manifests are refused before being parsed.
pub const MAX_MANIFEST_BYTES: u64 = 256 * 1024;
pub const MAX_ENTRIES: usize = 32;
pub const MAX_STEPS: usize = 500;
pub const MAX_ARGS: usize = 32;
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 300;
pub const MAX_TIMEOUT_SECONDS: u64 = 3600;
/// Values of an entry's `requires`.
pub const REQUIREMENTS: &[&str] = &["project", "sequence", "selection"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Manifest {
    #[serde(default = "one")]
    pub format_version: u32,
    /// Folder name under the extensions directory: lower-case letters, digits, `.`, `_`, `-`.
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Entry {
    /// Menu text (when the extension has several entries; a single entry shows the extension's name).
    pub label: String,
    #[serde(default)]
    pub description: String,
    /// What must exist for the entry to be enabled: `project` (saved), `sequence`, `selection`.
    #[serde(default)]
    pub requires: Vec<String>,
    pub action: Action,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Action {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<Vec<Step>>,
    /// A `.jsonl` file inside the extension, one `{"id","params"}` per line (`#` comments allowed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jsonl: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec: Option<ExecSpec>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ActionKind {
    Commands,
    Jsonl,
    Exec,
}

/// One FilmCraft command: the `id` and `params` of `filmcraft-cli exec`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Step {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    /// Go on with the next step when this one fails.
    #[serde(default)]
    pub continue_on_error: bool,
}

/// An external program. `program` is a file inside the extension (`scripts/run.sh`) or a bare command
/// name looked up on `PATH` (`python3`). The editing context arrives as JSON on its standard input.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExecSpec {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

fn one() -> u32 {
    FORMAT_VERSION
}
fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_SECONDS
}

impl Action {
    /// Which of the three kinds this is (a validated manifest has exactly one).
    pub fn kind(&self) -> Option<ActionKind> {
        match (&self.commands, &self.jsonl, &self.exec) {
            (Some(_), None, None) => Some(ActionKind::Commands),
            (None, Some(_), None) => Some(ActionKind::Jsonl),
            (None, None, Some(_)) => Some(ActionKind::Exec),
            _ => None,
        }
    }
}

impl Manifest {
    /// Parse and validate the text of an `extension.json`.
    pub fn parse(text: &str) -> Result<Manifest> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let m: Manifest = serde_json::from_str(text).map_err(|e| ExtError::Manifest(format!("{MANIFEST_FILE}: {e}")))?;
        m.validate()?;
        Ok(m)
    }

    /// Read `<dir>/extension.json`.
    pub fn load(dir: &std::path::Path) -> Result<Manifest> {
        let path = dir.join(MANIFEST_FILE);
        let meta = std::fs::metadata(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ExtError::Manifest(format!("no {MANIFEST_FILE} in {}", dir.display())),
            _ => ExtError::io(path.display(), e),
        })?;
        if meta.len() > MAX_MANIFEST_BYTES {
            return Err(ExtError::Manifest(format!("{MANIFEST_FILE} is larger than {} KiB", MAX_MANIFEST_BYTES / 1024)));
        }
        let bytes = std::fs::read(&path).map_err(|e| ExtError::io(path.display(), e))?;
        let text = String::from_utf8(bytes).map_err(|_| ExtError::Manifest(format!("{MANIFEST_FILE} is not UTF-8 text")))?;
        Manifest::parse(&text)
    }

    /// The structural rules (no file system access): see the module documentation.
    pub fn validate(&self) -> Result<()> {
        let bad = |m: String| Err(ExtError::Manifest(format!("{MANIFEST_FILE}: {m}")));
        if self.format_version != FORMAT_VERSION {
            return bad(format!("formatVersion {} is not supported (this FilmCraft reads {FORMAT_VERSION})", self.format_version));
        }
        if let Err(m) = check_id(&self.id) {
            return bad(format!("id: {m}"));
        }
        if let Err(m) = check_text("name", &self.name, 80, true) {
            return bad(m);
        }
        if let Err(m) = check_version(&self.version) {
            return bad(format!("version: {m}"));
        }
        if let Err(m) = check_text("description", &self.description, 500, false) {
            return bad(m);
        }
        if let Err(m) = check_text("author", &self.author, 120, false) {
            return bad(m);
        }
        if self.entries.is_empty() {
            return bad("`entries` is empty: an extension needs at least one entry".into());
        }
        if self.entries.len() > MAX_ENTRIES {
            return bad(format!("{} entries (at most {MAX_ENTRIES})", self.entries.len()));
        }
        let mut labels = BTreeSet::new();
        for (n, e) in self.entries.iter().enumerate() {
            let at = |m: String| format!("entries[{n}] ({}): {m}", e.label);
            if let Err(m) = check_text("label", &e.label, 80, true) {
                return bad(format!("entries[{n}]: {m}"));
            }
            if !labels.insert(e.label.to_lowercase()) {
                return bad(at("this label is used twice".into()));
            }
            if let Err(m) = check_text("description", &e.description, 500, false) {
                return bad(at(m));
            }
            for r in &e.requires {
                if !REQUIREMENTS.contains(&r.as_str()) {
                    return bad(at(format!("unknown requirement `{r}` (expected {})", REQUIREMENTS.join(", "))));
                }
            }
            if let Err(m) = e.action.validate() {
                return bad(at(m));
            }
        }
        Ok(())
    }

    /// Whether any entry starts an external program (shown to the user before installing).
    pub fn runs_programs(&self) -> bool {
        self.entries.iter().any(|e| e.action.exec.is_some())
    }
}

impl Action {
    fn validate(&self) -> std::result::Result<(), String> {
        let n = self.commands.is_some() as u8 + self.jsonl.is_some() as u8 + self.exec.is_some() as u8;
        if n != 1 {
            return Err(format!("`action` needs exactly one of `commands`, `jsonl`, `exec` (found {n})"));
        }
        if let Some(steps) = &self.commands {
            if steps.is_empty() {
                return Err("`action.commands` is empty".into());
            }
            if steps.len() > MAX_STEPS {
                return Err(format!("`action.commands` has {} steps (at most {MAX_STEPS})", steps.len()));
            }
            for (i, s) in steps.iter().enumerate() {
                check_command_id(&s.id).map_err(|m| format!("commands[{i}]: {m}"))?;
                if let Some(p) = &s.params
                    && !p.is_object()
                {
                    return Err(format!("commands[{i}] ({}): `params` must be an object", s.id));
                }
            }
        }
        if let Some(p) = &self.jsonl {
            check_rel_path(p).map_err(|m| format!("jsonl: {m}"))?;
            if !p.to_lowercase().ends_with(".jsonl") {
                return Err("jsonl: the file name must end in .jsonl".into());
            }
        }
        if let Some(x) = &self.exec {
            check_program(&x.program).map_err(|m| format!("exec.program: {m}"))?;
            if x.args.len() > MAX_ARGS {
                return Err(format!("exec.args has {} items (at most {MAX_ARGS})", x.args.len()));
            }
            if x.args.iter().any(|a| a.len() > 1024 || a.contains('\0')) {
                return Err("exec.args: an argument is longer than 1024 bytes or contains a NUL".into());
            }
            if x.timeout_seconds == 0 || x.timeout_seconds > MAX_TIMEOUT_SECONDS {
                return Err(format!("exec.timeoutSeconds must be between 1 and {MAX_TIMEOUT_SECONDS}"));
            }
        }
        Ok(())
    }
}

/// `[a-z0-9][a-z0-9._-]{0,63}`, no `..`, not ending in a dot: safe as a folder name on every system.
pub fn check_id(id: &str) -> std::result::Result<(), String> {
    let ok_char = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-');
    let first = id.chars().next();
    if id.is_empty() || id.len() > 64 {
        return Err("must be 1 to 64 characters".into());
    }
    if !first.is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit()) || !id.chars().all(ok_char) {
        return Err(format!("`{id}` must start with a lower-case letter or digit and contain only lower-case letters, digits, `.`, `_`, `-`"));
    }
    if id.contains("..") || id.ends_with('.') {
        return Err(format!("`{id}` must not contain `..` or end with a dot"));
    }
    Ok(())
}

fn check_text(field: &str, s: &str, max: usize, required: bool) -> std::result::Result<(), String> {
    if required && s.trim().is_empty() {
        return Err(format!("`{field}` is empty"));
    }
    if s.chars().count() > max {
        return Err(format!("`{field}` is longer than {max} characters"));
    }
    if s.chars().any(char::is_control) {
        return Err(format!("`{field}` contains a control character"));
    }
    Ok(())
}

fn check_version(v: &str) -> std::result::Result<(), String> {
    let ok = !v.is_empty()
        && v.len() <= 32
        && v.starts_with(|c: char| c.is_ascii_digit())
        && v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
    if ok { Ok(()) } else { Err(format!("`{v}` is not a version such as 1.0.0")) }
}

/// A FilmCraft command id such as `transcript.removePauses`.
pub fn check_command_id(id: &str) -> std::result::Result<(), String> {
    let ok = !id.is_empty()
        && id.len() <= 80
        && id.starts_with(|c: char| c.is_ascii_alphabetic())
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_'));
    if ok { Ok(()) } else { Err(format!("`{id}` is not a command id (letters, digits, `.`, `_`)")) }
}

/// A path inside the extension: relative, `/`-separated, no `..`, no empty or `.` parts.
pub fn check_rel_path(p: &str) -> std::result::Result<(), String> {
    if p.is_empty() || p.len() > 200 {
        return Err("a path must be 1 to 200 characters".into());
    }
    if p.chars().any(|c| c.is_control() || c == '\\' || c == ':') || p.starts_with('/') {
        return Err(format!("`{p}` must be a relative path with `/` separators"));
    }
    if p.split('/').any(|c| c.is_empty() || c == "." || c == "..") {
        return Err(format!("`{p}` must not contain empty, `.` or `..` parts"));
    }
    Ok(())
}

fn check_program(p: &str) -> std::result::Result<(), String> {
    if p.contains('/') {
        return check_rel_path(p);
    }
    let ok = !p.is_empty() && p.len() <= 64 && p != "." && p != ".." && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'));
    if ok { Ok(()) } else { Err(format!("`{p}` must be a file inside the extension (`scripts/run.sh`) or a command name such as `python3`")) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base() -> Value {
        json!({
            "id": "demo", "name": "Demo", "version": "1.0.0",
            "entries": [{"label": "Go", "action": {"commands": [{"id": "markers.add", "params": {"name": "x"}}]}}]
        })
    }
    fn parse(v: &Value) -> Result<Manifest> {
        Manifest::parse(&v.to_string())
    }
    fn err(v: &Value) -> String {
        parse(v).unwrap_err().to_string()
    }

    #[test]
    fn a_minimal_manifest_parses() {
        let m = parse(&base()).unwrap();
        assert_eq!(m.id, "demo");
        assert_eq!(m.format_version, 1);
        assert_eq!(m.entries[0].action.kind(), Some(ActionKind::Commands));
        assert!(!m.runs_programs());
    }

    #[test]
    fn the_three_action_kinds() {
        let mut v = base();
        v["entries"][0]["action"] = json!({"jsonl": "steps/run.jsonl"});
        assert_eq!(parse(&v).unwrap().entries[0].action.kind(), Some(ActionKind::Jsonl));
        v["entries"][0]["action"] = json!({"exec": {"program": "scripts/run.sh", "args": ["${project}"], "timeoutSeconds": 10}});
        let m = parse(&v).unwrap();
        assert_eq!(m.entries[0].action.kind(), Some(ActionKind::Exec));
        assert!(m.runs_programs());
        v["entries"][0]["action"] = json!({"exec": {"program": "python3"}});
        assert_eq!(parse(&v).unwrap().entries[0].action.exec.as_ref().unwrap().timeout_seconds, DEFAULT_TIMEOUT_SECONDS);
    }

    #[test]
    fn errors_name_the_field_and_the_entry() {
        let mut v = base();
        v["id"] = json!("Bad Id");
        assert!(err(&v).contains("id:"), "{}", err(&v));
        let mut v = base();
        v["entries"] = json!([]);
        assert!(err(&v).contains("at least one entry"));
        let mut v = base();
        v["entries"][0]["action"] = json!({});
        assert!(err(&v).contains("entries[0] (Go)") && err(&v).contains("exactly one"), "{}", err(&v));
        let mut v = base();
        v["entries"][0]["action"] = json!({"commands": [{"id": "a.b"}], "jsonl": "x.jsonl"});
        assert!(err(&v).contains("found 2"));
        let mut v = base();
        v["entries"][0]["action"] = json!({"commands": []});
        assert!(err(&v).contains("empty"));
        let mut v = base();
        v["entries"][0]["requires"] = json!(["moon"]);
        assert!(err(&v).contains("unknown requirement `moon`"));
    }

    #[test]
    fn unknown_fields_and_wrong_types_are_errors() {
        let mut v = base();
        v["colour"] = json!("red");
        assert!(err(&v).contains("unknown field `colour`"), "{}", err(&v));
        let mut v = base();
        v["version"] = json!(3);
        assert!(err(&v).contains("extension.json"));
        assert!(Manifest::parse("not json").unwrap_err().to_string().contains("extension.json"));
        assert!(Manifest::parse("").is_err());
        assert!(Manifest::parse("[]").is_err());
        let mut v = base();
        v["formatVersion"] = json!(2);
        assert!(err(&v).contains("formatVersion 2"));
    }

    #[test]
    fn ids_labels_versions_and_paths_are_checked() {
        for good in ["a", "silence-remover", "a.b_c-9", "0day"] {
            assert!(check_id(good).is_ok(), "{good}");
        }
        for bad in ["", "A", "-a", ".a", "a/b", "a..b", "a.", "a b", "é", &"a".repeat(65), "../x"] {
            assert!(check_id(bad).is_err(), "{bad}");
        }
        let mut v = base();
        v["version"] = json!("v1");
        assert!(err(&v).contains("version"));
        let mut v = base();
        v["entries"] = json!([
            {"label": "Go", "action": {"commands": [{"id": "a.b"}]}},
            {"label": "go", "action": {"commands": [{"id": "a.b"}]}}
        ]);
        assert!(err(&v).contains("used twice"));
        for bad in ["../x.jsonl", "/etc/x.jsonl", "a//b.jsonl", "a\\b.jsonl", "./a.jsonl", "C:x.jsonl", "", "a/../b.jsonl"] {
            let mut v = base();
            v["entries"][0]["action"] = json!({"jsonl": bad});
            assert!(parse(&v).is_err(), "{bad}");
        }
        let mut v = base();
        v["entries"][0]["action"] = json!({"jsonl": "steps.txt"});
        assert!(err(&v).contains(".jsonl"));
        for bad in ["../evil", "/bin/sh", "a b", ""] {
            let mut v = base();
            v["entries"][0]["action"] = json!({"exec": {"program": bad}});
            assert!(parse(&v).is_err(), "{bad}");
        }
        let mut v = base();
        v["entries"][0]["action"] = json!({"exec": {"program": "sh", "timeoutSeconds": 0}});
        assert!(err(&v).contains("timeoutSeconds"));
        v["entries"][0]["action"] = json!({"exec": {"program": "sh", "timeoutSeconds": 99999}});
        assert!(err(&v).contains("timeoutSeconds"));
        let mut v = base();
        v["entries"][0]["action"] = json!({"commands": [{"id": "no spaces allowed"}]});
        assert!(err(&v).contains("not a command id"));
        v["entries"][0]["action"] = json!({"commands": [{"id": "a.b", "params": [1]}]});
        assert!(err(&v).contains("params"));
    }

    #[test]
    fn hostile_sizes_are_refused_not_crashed_on() {
        let mut v = base();
        v["name"] = json!("x".repeat(10_000));
        assert!(err(&v).contains("name"));
        let mut v = base();
        v["name"] = json!("bell\u{7}");
        assert!(err(&v).contains("control"));
        let mut v = base();
        v["entries"] = Value::Array((0..40).map(|i| json!({"label": format!("e{i}"), "action": {"commands": [{"id": "a.b"}]}})).collect());
        assert!(err(&v).contains("at most 32"));
        let mut v = base();
        v["entries"][0]["action"] = json!({"commands": (0..501).map(|_| json!({"id": "a.b"})).collect::<Vec<_>>()});
        assert!(err(&v).contains("at most 500"));
        // deeply nested JSON is rejected by the parser's own recursion limit
        let deep = format!("{}{}", "[".repeat(5000), "]".repeat(5000));
        assert!(Manifest::parse(&deep).is_err());
        // a BOM is tolerated
        assert!(Manifest::parse(&format!("\u{feff}{}", base())).is_ok());
    }

    #[test]
    fn load_reads_the_file_and_reports_a_missing_one() {
        let dir = std::env::temp_dir().join(format!("fcext-manifest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(Manifest::load(&dir).unwrap_err().to_string().contains("no extension.json"));
        std::fs::write(dir.join(MANIFEST_FILE), base().to_string()).unwrap();
        assert_eq!(Manifest::load(&dir).unwrap().id, "demo");
        std::fs::write(dir.join(MANIFEST_FILE), vec![b' '; (MAX_MANIFEST_BYTES + 1) as usize]).unwrap();
        assert!(Manifest::load(&dir).unwrap_err().to_string().contains("larger than"));
        std::fs::write(dir.join(MANIFEST_FILE), [0xff, 0xfe, 0x00]).unwrap();
        assert!(Manifest::load(&dir).unwrap_err().to_string().contains("UTF-8"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
