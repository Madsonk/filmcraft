//! Extension packages: a folder, or a `.zip` / `.fcext` archive (a zip with `extension.json` at its
//! root, or inside one top-level folder). [`stage`] copies or extracts a package into an empty
//! directory with every limit and path check applied; the store then moves it into place.
//!
//! Hostile archives are expected: paths are checked component by component (no `..`, no absolute
//! paths, no drive letters), symbolic links are refused, and the number of files and the bytes
//! written are capped however the archive declares its sizes.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::manifest::{MANIFEST_FILE, Manifest};
use crate::{ExtError, Result};

pub const MAX_FILES: usize = 2000;
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
const MAX_DEPTH: usize = 16;
/// Archive extensions accepted by [`stage`].
pub const ARCHIVE_EXTENSIONS: &[&str] = &["zip", "fcext"];

#[derive(Default)]
struct Budget {
    files: usize,
    bytes: u64,
}

impl Budget {
    fn file(&mut self, what: &str) -> Result<()> {
        self.files += 1;
        if self.files > MAX_FILES {
            return Err(ExtError::Package(format!("the package has more than {MAX_FILES} files ({what})")));
        }
        Ok(())
    }
    fn bytes(&mut self, n: u64, what: &str) -> Result<()> {
        self.bytes = self.bytes.saturating_add(n);
        if n > MAX_FILE_BYTES || self.bytes > MAX_TOTAL_BYTES {
            return Err(ExtError::Package(format!("the package is too large ({what}; at most {} MiB in total)", MAX_TOTAL_BYTES / (1024 * 1024))));
        }
        Ok(())
    }
}

/// Whether `p` looks like an archive FilmCraft can unpack.
pub fn is_archive(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| ARCHIVE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Copy or extract the package at `src` into the existing, empty directory `dest`, then load and
/// validate its manifest and the files it names.
pub fn stage(src: &Path, dest: &Path) -> Result<Manifest> {
    let meta = fs::metadata(src).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => ExtError::Package(format!("{} does not exist", src.display())),
        _ => ExtError::io(src.display(), e),
    })?;
    let mut budget = Budget::default();
    if meta.is_dir() {
        copy_dir(src, dest, 0, &mut budget)?;
    } else if is_archive(src) {
        unzip(src, dest, &mut budget)?;
    } else {
        return Err(ExtError::Package(format!("{} is neither a folder nor a .zip / .fcext archive", src.display())));
    }
    let manifest = Manifest::load(dest).map_err(|e| match e {
        ExtError::Manifest(m) if m.starts_with("no extension.json") => ExtError::Package(format!("{} has no extension.json at its top level", src.display())),
        other => other,
    })?;
    check_files(&manifest, dest)?;
    Ok(manifest)
}

/// The files an entry names (`jsonl`, a program inside the extension) must exist in the package, and
/// a `.jsonl` file must be well-formed.
pub fn check_files(m: &Manifest, dir: &Path) -> Result<()> {
    for e in &m.entries {
        let at = |what: String| ExtError::Package(format!("entry `{}`: {what}", e.label));
        if let Some(p) = &e.action.jsonl {
            let path = inside(dir, p)?;
            let text = fs::read_to_string(&path).map_err(|err| at(format!("{p}: {err}")))?;
            crate::run::parse_jsonl(&text).map_err(|err| at(format!("{p}: {err}")))?;
        }
        if let Some(x) = &e.action.exec
            && x.program.contains('/')
        {
            let path = inside(dir, &x.program)?;
            if !path.is_file() {
                return Err(at(format!("the program {} is not in the package", x.program)));
            }
        }
    }
    Ok(())
}

/// `dir/rel`, refusing anything that does not stay inside `dir` (the path was already checked
/// syntactically by the manifest; this also follows symbolic links).
pub fn inside(dir: &Path, rel: &str) -> Result<PathBuf> {
    let joined = dir.join(rel);
    let real_dir = fs::canonicalize(dir).map_err(|e| ExtError::io(dir.display(), e))?;
    let real = fs::canonicalize(&joined).map_err(|e| ExtError::Package(format!("{rel}: {e}")))?;
    if real.starts_with(&real_dir) { Ok(real) } else { Err(ExtError::Package(format!("{rel} points outside the extension"))) }
}

fn skipped(name: &str) -> bool {
    matches!(name, ".git" | ".DS_Store" | "__MACOSX" | "Thumbs.db")
}

fn copy_dir(src: &Path, dest: &Path, depth: usize, budget: &mut Budget) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(ExtError::Package(format!("folders are nested deeper than {MAX_DEPTH} levels at {}", src.display())));
    }
    let mut entries: Vec<_> = fs::read_dir(src).map_err(|e| ExtError::io(src.display(), e))?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name();
        let Some(name_str) = name.to_str() else {
            return Err(ExtError::Package(format!("{} has a file name that is not valid text", src.display())));
        };
        if skipped(name_str) {
            continue;
        }
        let from = e.path();
        let to = dest.join(&name);
        let ft = e.file_type().map_err(|err| ExtError::io(from.display(), err))?;
        if ft.is_symlink() {
            return Err(ExtError::Package(format!("{} is a symbolic link: extensions can't contain links", from.display())));
        } else if ft.is_dir() {
            fs::create_dir(&to).map_err(|err| ExtError::io(to.display(), err))?;
            copy_dir(&from, &to, depth + 1, budget)?;
        } else if ft.is_file() {
            budget.file(name_str)?;
            let len = e.metadata().map_err(|err| ExtError::io(from.display(), err))?.len();
            budget.bytes(len, name_str)?;
            fs::copy(&from, &to).map_err(|err| ExtError::io(from.display(), err))?;
        } else {
            return Err(ExtError::Package(format!("{} is not a regular file or folder", from.display())));
        }
    }
    Ok(())
}

fn unzip(src: &Path, dest: &Path, budget: &mut Budget) -> Result<()> {
    let file = fs::File::open(src).map_err(|e| ExtError::io(src.display(), e))?;
    let mut archive =
        zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| ExtError::Package(format!("{} is not a valid archive: {e}", src.display())))?;
    if archive.len() > MAX_FILES * 2 {
        return Err(ExtError::Package(format!("the archive has {} entries (at most {})", archive.len(), MAX_FILES * 2)));
    }
    // Where the manifest is: the root, or the single top-level folder every entry lives in.
    let mut names = Vec::new();
    for i in 0..archive.len() {
        let f = archive.by_index(i).map_err(|e| ExtError::Package(format!("entry {i} of the archive: {e}")))?;
        names.push(f.name().replace('\\', "/"));
    }
    let relevant = |n: &String| !n.starts_with("__MACOSX/") && !n.ends_with(".DS_Store");
    let prefix = if names.iter().any(|n| n == MANIFEST_FILE) {
        String::new()
    } else {
        let tops: std::collections::BTreeSet<&str> = names.iter().filter(|n| relevant(n)).filter_map(|n| n.split('/').next()).collect();
        match (tops.len(), tops.iter().next()) {
            (1, Some(top)) if names.iter().any(|n| *n == format!("{top}/{MANIFEST_FILE}")) => format!("{top}/"),
            _ => String::new(),
        }
    };
    for i in 0..archive.len() {
        let mut f = archive.by_index(i).map_err(|e| ExtError::Package(format!("entry {i} of the archive: {e}")))?;
        let raw = f.name().replace('\\', "/");
        if !relevant(&raw) {
            continue;
        }
        let Some(rel) = raw.strip_prefix(prefix.as_str()) else { continue };
        if rel.is_empty() {
            continue;
        }
        if f.is_symlink() {
            return Err(ExtError::Package(format!("{raw} is a symbolic link: extensions can't contain links")));
        }
        if rel.starts_with('/') || rel.contains(':') {
            return Err(ExtError::Package(format!("{raw} has an unsafe path")));
        }
        let rel_trim = rel.trim_end_matches('/');
        let target = rel_trim.split('/').try_fold(dest.to_path_buf(), |acc, part| {
            if part.is_empty() || part == "." || part == ".." || skipped(part) { None } else { Some(acc.join(part)) }
        });
        let Some(target) = target else { return Err(ExtError::Package(format!("{raw} has an unsafe path"))) };
        if f.is_dir() {
            fs::create_dir_all(&target).map_err(|e| ExtError::io(target.display(), e))?;
            continue;
        }
        budget.file(&raw)?;
        budget.bytes(f.size(), &raw)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| ExtError::io(parent.display(), e))?;
        }
        // the declared size can lie: count what is really written
        let mut out = fs::File::create(&target).map_err(|e| ExtError::io(target.display(), e))?;
        let written = std::io::copy(&mut (&mut f).take(MAX_FILE_BYTES + 1), &mut out).map_err(|e| ExtError::Package(format!("{raw}: {e}")))?;
        if written > MAX_FILE_BYTES {
            return Err(ExtError::Package(format!("{raw} is larger than {} MiB", MAX_FILE_BYTES / (1024 * 1024))));
        }
        if written > f.size() {
            budget.bytes(written - f.size(), &raw)?;
        }
        drop(out);
        set_mode(&target, f.unix_mode());
    }
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: Option<u32>) {
    use std::os::unix::fs::PermissionsExt;
    let executable = mode.is_some_and(|m| m & 0o111 != 0);
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }));
}
#[cfg(not(unix))]
fn set_mode(_: &Path, _: Option<u32>) {}

/// Pack the extension folder `dir` into the archive `out` (`.fcext` / `.zip`), for sharing.
pub fn pack_dir(dir: &Path, out: &Path) -> Result<Manifest> {
    let manifest = Manifest::load(dir)?;
    check_files(&manifest, dir)?;
    if let (Ok(a), Some(parent)) =
        (fs::canonicalize(dir), out.parent().filter(|p| !p.as_os_str().is_empty()).map(|p| p.to_path_buf()).or_else(|| Some(PathBuf::from("."))))
    {
        let out_dir = fs::canonicalize(&parent).unwrap_or(parent);
        if out_dir.starts_with(&a) {
            return Err(ExtError::Package("write the archive outside the extension folder".into()));
        }
    }
    let file = fs::File::create(out).map_err(|e| ExtError::io(out.display(), e))?;
    let mut zw = zip::ZipWriter::new(file);
    let mut budget = Budget::default();
    add_dir(&mut zw, dir, "", 0, &mut budget)?;
    zw.finish().map_err(|e| ExtError::Package(format!("{}: {e}", out.display())))?;
    Ok(manifest)
}

fn add_dir(zw: &mut zip::ZipWriter<fs::File>, dir: &Path, prefix: &str, depth: usize, budget: &mut Budget) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(ExtError::Package(format!("folders are nested deeper than {MAX_DEPTH} levels at {}", dir.display())));
    }
    let mut entries: Vec<_> = fs::read_dir(dir).map_err(|e| ExtError::io(dir.display(), e))?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let Some(name) = e.file_name().to_str().map(str::to_string) else {
            return Err(ExtError::Package(format!("{} has a file name that is not valid text", dir.display())));
        };
        if skipped(&name) {
            continue;
        }
        let path = e.path();
        let ft = e.file_type().map_err(|err| ExtError::io(path.display(), err))?;
        let rel = format!("{prefix}{name}");
        if ft.is_symlink() {
            return Err(ExtError::Package(format!("{} is a symbolic link: extensions can't contain links", path.display())));
        }
        if ft.is_dir() {
            add_dir(zw, &path, &format!("{rel}/"), depth + 1, budget)?;
        } else if ft.is_file() {
            budget.file(&name)?;
            let bytes = fs::read(&path).map_err(|err| ExtError::io(path.display(), err))?;
            budget.bytes(bytes.len() as u64, &name)?;
            let mode = executable_mode(&path);
            let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).unix_permissions(mode);
            zw.start_file(rel.as_str(), opts).map_err(|err| ExtError::Package(format!("{rel}: {err}")))?;
            zw.write_all(&bytes).map_err(|err| ExtError::Package(format!("{rel}: {err}")))?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn executable_mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    if fs::metadata(path).map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false) { 0o755 } else { 0o644 }
}
#[cfg(not(unix))]
fn executable_mode(_: &Path) -> u32 {
    0o644
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn temp(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!("fcext-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    pub(crate) fn manifest_json(id: &str) -> String {
        json!({"id": id, "name": "Demo", "version": "1.0.0", "entries": [{"label": "Go", "action": {"commands": [{"id": "markers.add"}]}}]}).to_string()
    }

    pub(crate) fn zip_of(path: &Path, files: &[(&str, &[u8])]) {
        let mut zw = zip::ZipWriter::new(fs::File::create(path).unwrap());
        for (name, data) in files {
            let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            zw.start_file(*name, opts).unwrap();
            zw.write_all(data).unwrap();
        }
        zw.finish().unwrap();
    }

    fn stage_into(src: &Path) -> (Result<Manifest>, PathBuf) {
        let dest = temp("stage");
        (stage(src, &dest), dest)
    }

    #[test]
    fn stages_a_folder() {
        let src = temp("src");
        fs::write(src.join("extension.json"), manifest_json("demo")).unwrap();
        fs::create_dir(src.join("scripts")).unwrap();
        fs::write(src.join("scripts/a.sh"), "#!/bin/sh\n").unwrap();
        fs::write(src.join(".DS_Store"), "junk").unwrap();
        let (m, dest) = stage_into(&src);
        assert_eq!(m.unwrap().id, "demo");
        assert!(dest.join("scripts/a.sh").is_file());
        assert!(!dest.join(".DS_Store").exists());
    }

    #[test]
    fn a_folder_without_manifest_or_with_a_link_is_refused() {
        let src = temp("src");
        let (m, _) = stage_into(&src);
        assert!(m.unwrap_err().to_string().contains("no extension.json"));
        #[cfg(unix)]
        {
            fs::write(src.join("extension.json"), manifest_json("demo")).unwrap();
            std::os::unix::fs::symlink("/etc/passwd", src.join("link")).unwrap();
            let (m, _) = stage_into(&src);
            assert!(m.unwrap_err().to_string().contains("symbolic link"));
        }
        let (m, _) = stage_into(&src.join("missing"));
        assert!(m.unwrap_err().to_string().contains("does not exist"));
        fs::write(src.join("notes.txt"), "x").unwrap();
        let (m, _) = stage_into(&src.join("notes.txt"));
        assert!(m.unwrap_err().to_string().contains("neither a folder nor"));
    }

    #[test]
    fn unzips_with_the_manifest_at_the_root_or_in_one_folder() {
        let dir = temp("zip");
        let m = manifest_json("demo");
        zip_of(&dir.join("a.fcext"), &[("extension.json", m.as_bytes()), ("scripts/run.sh", b"echo hi")]);
        let (r, dest) = stage_into(&dir.join("a.fcext"));
        assert_eq!(r.unwrap().id, "demo");
        assert!(dest.join("scripts/run.sh").is_file());
        zip_of(&dir.join("b.zip"), &[("demo/extension.json", m.as_bytes()), ("demo/x.txt", b"x"), ("__MACOSX/demo/._x", b"junk")]);
        let (r, dest) = stage_into(&dir.join("b.zip"));
        assert_eq!(r.unwrap().id, "demo");
        assert!(dest.join("x.txt").is_file() && !dest.join("__MACOSX").exists());
    }

    #[test]
    fn hostile_archives_are_refused() {
        let dir = temp("evil");
        let m = manifest_json("demo");
        for (name, bad) in [("../evil.txt", "unsafe"), ("/abs.txt", "unsafe"), ("a/../../b.txt", "unsafe"), ("C:/x.txt", "unsafe")] {
            zip_of(&dir.join("e.zip"), &[("extension.json", m.as_bytes()), (name, b"x")]);
            let (r, dest) = stage_into(&dir.join("e.zip"));
            let e = r.unwrap_err().to_string();
            assert!(e.contains(bad), "{name}: {e}");
            assert!(!dest.parent().unwrap().join("evil.txt").exists());
        }
        // not an archive at all
        fs::write(dir.join("f.fcext"), b"this is not a zip").unwrap();
        let (r, _) = stage_into(&dir.join("f.fcext"));
        assert!(r.unwrap_err().to_string().contains("not a valid archive"));
        // truncated and bit-flipped archives never panic
        zip_of(&dir.join("g.zip"), &[("extension.json", m.as_bytes()), ("x.txt", &[7u8; 5000])]);
        let good = fs::read(dir.join("g.zip")).unwrap();
        for cut in [0, 10, good.len() / 2, good.len() - 5] {
            fs::write(dir.join("h.zip"), &good[..cut]).unwrap();
            let _ = stage_into(&dir.join("h.zip")).0;
        }
        for at in (0..good.len()).step_by(7) {
            let mut b = good.clone();
            b[at] ^= 0xa5;
            fs::write(dir.join("h.zip"), &b).unwrap();
            let r = std::panic::catch_unwind(|| stage_into(&dir.join("h.zip")).0.is_ok());
            assert!(r.is_ok(), "panic with a flipped byte at {at}");
        }
    }

    #[test]
    fn a_zip_bomb_is_stopped_by_the_size_caps() {
        let dir = temp("bomb");
        let m = manifest_json("demo");
        let big = vec![0u8; (MAX_FILE_BYTES + 1) as usize];
        zip_of(&dir.join("bomb.zip"), &[("extension.json", m.as_bytes()), ("zeros.bin", &big)]);
        let (r, _) = stage_into(&dir.join("bomb.zip"));
        assert!(r.unwrap_err().to_string().contains("too large"));
    }

    #[test]
    fn the_files_an_entry_names_must_exist() {
        let src = temp("files");
        let m = json!({"id": "demo", "name": "Demo", "version": "1", "entries": [
            {"label": "A", "action": {"jsonl": "steps.jsonl"}},
            {"label": "B", "action": {"exec": {"program": "scripts/run.sh"}}}
        ]});
        fs::write(src.join("extension.json"), m.to_string()).unwrap();
        let (r, _) = stage_into(&src);
        assert!(r.unwrap_err().to_string().contains("steps.jsonl"));
        fs::write(src.join("steps.jsonl"), "{\"id\": \"markers.add\"}\n").unwrap();
        let (r, _) = stage_into(&src);
        assert!(r.unwrap_err().to_string().contains("scripts/run.sh"));
        fs::create_dir(src.join("scripts")).unwrap();
        fs::write(src.join("scripts/run.sh"), "").unwrap();
        assert!(stage_into(&src).0.is_ok());
        fs::write(src.join("steps.jsonl"), "{not json}\n").unwrap();
        assert!(stage_into(&src).0.unwrap_err().to_string().contains("line 1"));
    }

    #[test]
    fn pack_then_stage_round_trips_and_keeps_the_executable_bit() {
        let src = temp("pack");
        let m = json!({"id": "demo", "name": "Demo", "version": "1", "entries": [{"label": "B", "action": {"exec": {"program": "scripts/run.sh"}}}]});
        fs::write(src.join("extension.json"), m.to_string()).unwrap();
        fs::create_dir(src.join("scripts")).unwrap();
        fs::write(src.join("scripts/run.sh"), "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(src.join("scripts/run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let out = temp("packout").join("demo.fcext");
        assert_eq!(pack_dir(&src, &out).unwrap().id, "demo");
        let (r, dest) = stage_into(&out);
        assert_eq!(r.unwrap().id, "demo");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(dest.join("scripts/run.sh")).unwrap().permissions().mode() & 0o111, 0o111);
        }
        assert!(dest.join("extension.json").is_file());
        assert!(pack_dir(&src, &src.join("inside.fcext")).is_err(), "output inside the folder");
    }
}
