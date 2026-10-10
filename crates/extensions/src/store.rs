//! The extensions folder: `<data dir>/extensions/<id>/extension.json` (+ the files of the extension).
//!
//! Listing never fails because of one bad extension: a folder that can't be loaded is reported with
//! its error ([`Listed`]) next to the good ones. Installing stages the package in a hidden folder next
//! to its destination and swaps it in with a rename, so a failed install leaves the previous version
//! untouched.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use serde::{Deserialize, Serialize};

use crate::manifest::{Manifest, check_id};
use crate::{ExtError, Result, pack};

/// A loaded extension: its manifest and the folder it lives in.
#[derive(Clone, Debug, PartialEq)]
pub struct Extension {
    pub manifest: Manifest,
    pub dir: PathBuf,
}

/// One folder of the extensions directory: the extension, or why it could not be loaded.
#[derive(Clone, Debug, PartialEq)]
pub struct Listed {
    pub dir_name: String,
    pub result: std::result::Result<Extension, String>,
}

/// What [`ExtensionStore::install`] did.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallReport {
    pub id: String,
    pub name: String,
    pub version: String,
    /// The version that was replaced, when the extension was already installed.
    pub replaced: Option<String>,
    /// Some entry starts an external program (the user should know before running it).
    pub runs_programs: bool,
}

/// Which bundled extensions were already offered to this user (so a removed one stays removed).
#[derive(Default, Serialize, Deserialize)]
struct Seeded {
    ids: Vec<String>,
}

const SEEDED_FILE: &str = ".bundled-seeded.json";

#[derive(Clone, Debug)]
pub struct ExtensionStore {
    root: PathBuf,
}

fn unique(tag: &str) -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    format!(".{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

impl ExtensionStore {
    /// A store rooted at `root` (usually `<data dir>/extensions`); nothing is created until an install.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        ExtensionStore { root: root.into() }
    }

    /// The extensions folder inside a FilmCraft data directory.
    pub fn in_data_dir(data_dir: &Path) -> Self {
        Self::new(data_dir.join("extensions"))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Every folder of the extensions directory, loaded; sorted by name, case-insensitively.
    pub fn list(&self) -> Vec<Listed> {
        let Ok(rd) = fs::read_dir(&self.root) else { return Vec::new() };
        let mut out: Vec<Listed> = rd
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| {
                let dir_name = e.file_name().to_str()?.to_string();
                if dir_name.starts_with('.') {
                    return None;
                }
                let dir = e.path();
                let result = Manifest::load(&dir)
                    .and_then(|m| {
                        if m.id != dir_name {
                            return Err(ExtError::Manifest(format!("the folder is named `{dir_name}` but extension.json says id `{}`", m.id)));
                        }
                        Ok(Extension { manifest: m, dir: dir.clone() })
                    })
                    .map_err(|e| e.to_string());
                Some(Listed { dir_name, result })
            })
            .collect();
        out.sort_by_key(|l| match &l.result {
            Ok(x) => (x.manifest.name.to_lowercase(), l.dir_name.clone()),
            Err(_) => (l.dir_name.to_lowercase(), l.dir_name.clone()),
        });
        out
    }

    /// The extensions that loaded, in name order.
    pub fn extensions(&self) -> Vec<Extension> {
        self.list().into_iter().filter_map(|l| l.result.ok()).collect()
    }

    pub fn get(&self, id: &str) -> Result<Extension> {
        check_id(id).map_err(ExtError::Manifest)?;
        let dir = self.root.join(id);
        if !dir.is_dir() {
            return Err(ExtError::NotInstalled(id.to_string()));
        }
        let m = Manifest::load(&dir)?;
        if m.id != id {
            return Err(ExtError::Manifest(format!("the folder is named `{id}` but extension.json says id `{}`", m.id)));
        }
        Ok(Extension { manifest: m, dir })
    }

    /// Install (or update) the package at `src`: an extension folder, or a `.zip` / `.fcext` archive.
    pub fn install(&self, src: &Path) -> Result<InstallReport> {
        fs::create_dir_all(&self.root).map_err(|e| ExtError::io(self.root.display(), e))?;
        let staging = self.root.join(unique("staging"));
        fs::create_dir(&staging).map_err(|e| ExtError::io(staging.display(), e))?;
        let result = self.install_staged(src, &staging);
        // a successful install moved `staging` away; this only removes what a failure left
        let _ = fs::remove_dir_all(&staging);
        result
    }

    fn install_staged(&self, src: &Path, staging: &Path) -> Result<InstallReport> {
        let m = pack::stage(src, staging)?;
        let target = self.root.join(&m.id);
        let replaced = if target.exists() { Some(Manifest::load(&target).map(|o| o.version).unwrap_or_else(|_| "unreadable".to_string())) } else { None };
        let backup = self.root.join(unique("old"));
        if replaced.is_some() {
            fs::rename(&target, &backup).map_err(|e| ExtError::io(format!("replacing {}", target.display()), e))?;
        }
        if let Err(e) = fs::rename(staging, &target) {
            if replaced.is_some() {
                let _ = fs::rename(&backup, &target);
            }
            return Err(ExtError::io(format!("installing into {}", target.display()), e));
        }
        if replaced.is_some() {
            let _ = fs::remove_dir_all(&backup);
        }
        Ok(InstallReport { id: m.id.clone(), name: m.name.clone(), version: m.version.clone(), replaced, runs_programs: m.runs_programs() })
    }

    /// Delete the installed extension `id`.
    pub fn remove(&self, id: &str) -> Result<()> {
        check_id(id).map_err(ExtError::Manifest)?;
        let dir = self.root.join(id);
        let meta = fs::symlink_metadata(&dir).map_err(|_| ExtError::NotInstalled(id.to_string()))?;
        if !meta.is_dir() {
            return Err(ExtError::NotInstalled(id.to_string()));
        }
        fs::remove_dir_all(&dir).map_err(|e| ExtError::io(format!("removing {}", dir.display()), e))
    }

    /// Install the extensions found in `bundled` (one sub-folder each) that this user has not been
    /// offered yet and that **only drive FilmCraft's own commands**. An id already installed is left
    /// alone; an id the user removed later is not brought back (the offer is remembered in
    /// `.bundled-seeded.json`). An extension that starts an external program is never installed here:
    /// it is [`offers`](Self::offers)ed, and the user installs it knowingly
    /// ([`install_offer`](Self::install_offer)). Returns the ids installed and the problems met.
    pub fn seed_bundled(&self, bundled: &Path) -> (Vec<String>, Vec<String>) {
        let (mut installed, mut problems) = (Vec::new(), Vec::new());
        let mut seeded = self.seeded();
        let mut changed = false;
        for (dir, loaded) in Self::bundled_dirs(bundled) {
            let m = match loaded {
                Ok(m) => m,
                Err(e) => {
                    problems.push(format!("{}: {e}", dir.display()));
                    continue;
                }
            };
            if seeded.ids.contains(&m.id) || m.runs_programs() {
                continue;
            }
            if !self.root.join(&m.id).exists() {
                match self.install(&dir) {
                    Ok(r) => installed.push(r.id),
                    Err(e) => {
                        problems.push(format!("{}: {e}", m.id));
                        continue;
                    }
                }
            }
            seeded.ids.push(m.id);
            changed = true;
        }
        if changed && let Err(e) = self.save_seeded(&seeded) {
            problems.push(e);
        }
        (installed, problems)
    }

    /// The bundled extensions of `bundled` that are not installed (valid ones only), sorted by id.
    /// `answered` is true when the user already installed, declined or removed it once.
    pub fn offers(&self, bundled: &Path) -> Vec<Offer> {
        let seeded = self.seeded();
        let mut out: Vec<Offer> = Self::bundled_dirs(bundled)
            .into_iter()
            .filter_map(|(dir, m)| m.ok().map(|m| (dir, m)))
            .filter(|(_, m)| !self.root.join(&m.id).exists())
            .map(|(dir, m)| Offer {
                answered: seeded.ids.contains(&m.id),
                runs_programs: m.runs_programs(),
                programs: m.entries.iter().filter_map(|e| e.action.exec.as_ref().map(|x| x.program.clone())).collect(),
                id: m.id,
                name: m.name,
                version: m.version,
                description: m.description,
                dir,
            })
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Install an offered bundled extension (the user said yes) and remember that it was answered.
    pub fn install_offer(&self, dir: &Path) -> Result<InstallReport> {
        let r = self.install(dir)?;
        let mut seeded = self.seeded();
        if !seeded.ids.contains(&r.id) {
            seeded.ids.push(r.id.clone());
            self.save_seeded(&seeded).map_err(ExtError::Io)?;
        }
        Ok(r)
    }

    /// The user said "don't offer it again": remembered, nothing installed.
    pub fn decline_offer(&self, id: &str) -> Result<()> {
        let mut seeded = self.seeded();
        if !seeded.ids.iter().any(|x| x == id) {
            seeded.ids.push(id.to_string());
            self.save_seeded(&seeded).map_err(ExtError::Io)?;
        }
        Ok(())
    }

    fn seeded(&self) -> Seeded {
        fs::read_to_string(self.root.join(SEEDED_FILE)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    fn save_seeded(&self, seeded: &Seeded) -> std::result::Result<(), String> {
        let marker = self.root.join(SEEDED_FILE);
        let text = serde_json::to_string_pretty(seeded).map_err(|e| e.to_string())?;
        fs::create_dir_all(&self.root).map_err(|e| format!("{}: {e}", self.root.display()))?;
        fs::write(&marker, text).map_err(|e| format!("{}: {e}", marker.display()))
    }

    /// (folder, manifest or why not) of each sub-folder of `bundled` that has an `extension.json`, sorted.
    fn bundled_dirs(bundled: &Path) -> Vec<(PathBuf, std::result::Result<Manifest, ExtError>)> {
        let Ok(rd) = fs::read_dir(bundled) else { return Vec::new() };
        let mut dirs: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.join(crate::manifest::MANIFEST_FILE).is_file()).collect();
        dirs.sort();
        dirs.into_iter()
            .map(|d| {
                let m = Manifest::load(&d);
                (d, m)
            })
            .collect()
    }
}

/// A bundled extension that is not installed.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Offer {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    /// It starts external programs when run (the user must confirm before it is installed).
    pub runs_programs: bool,
    /// The programs its entries start (paths inside the extension, or names).
    pub programs: Vec<String>,
    /// Installed, declined or removed once already.
    pub answered: bool,
    /// The folder to install from.
    pub dir: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::tests::{manifest_json, temp, zip_of};

    fn ext_dir(id: &str, version: &str) -> PathBuf {
        let d = temp("extsrc");
        let m = manifest_json(id).replace("1.0.0", version);
        fs::write(d.join("extension.json"), m).unwrap();
        d
    }

    #[test]
    fn install_list_get_remove() {
        let store = ExtensionStore::new(temp("store").join("extensions"));
        assert!(store.list().is_empty(), "a missing folder is an empty list");
        let r = store.install(&ext_dir("zeta", "1.0.0")).unwrap();
        assert_eq!((r.id.as_str(), r.version.as_str(), r.replaced.clone()), ("zeta", "1.0.0", None));
        store.install(&ext_dir("alpha", "1.0.0")).unwrap();
        let names: Vec<_> = store.extensions().iter().map(|e| e.manifest.id.clone()).collect();
        assert_eq!(names, ["alpha", "zeta"]);
        assert_eq!(store.get("zeta").unwrap().manifest.version, "1.0.0");
        assert!(matches!(store.get("nope"), Err(ExtError::NotInstalled(_))));
        store.remove("zeta").unwrap();
        assert_eq!(store.extensions().len(), 1);
        assert!(matches!(store.remove("zeta"), Err(ExtError::NotInstalled(_))));
        // only the real extension is left: no staging or backup folders
        let left: Vec<_> = fs::read_dir(store.root()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
        assert_eq!(left, ["alpha"]);
    }

    #[test]
    fn reinstall_updates_and_a_failed_update_keeps_the_old_version() {
        let store = ExtensionStore::new(temp("store").join("extensions"));
        store.install(&ext_dir("demo", "1.0.0")).unwrap();
        let r = store.install(&ext_dir("demo", "2.0.0")).unwrap();
        assert_eq!(r.replaced.as_deref(), Some("1.0.0"));
        assert_eq!(store.get("demo").unwrap().manifest.version, "2.0.0");
        let bad = temp("bad");
        fs::write(bad.join("extension.json"), "{ nope").unwrap();
        assert!(store.install(&bad).unwrap_err().to_string().contains("extension.json"));
        assert_eq!(store.get("demo").unwrap().manifest.version, "2.0.0");
        assert_eq!(fs::read_dir(store.root()).unwrap().count(), 1, "staging folder cleaned up");
    }

    #[test]
    fn install_from_an_archive_and_remove_by_hostile_ids() {
        let store = ExtensionStore::new(temp("store").join("extensions"));
        let z = temp("z").join("demo.fcext");
        zip_of(&z, &[("extension.json", manifest_json("demo").as_bytes())]);
        assert_eq!(store.install(&z).unwrap().id, "demo");
        for bad in ["../demo", "", "/", "..", "a/b", "Demo", ".", &"x".repeat(100)] {
            assert!(store.remove(bad).is_err(), "{bad}");
            assert!(store.get(bad).is_err(), "{bad}");
        }
        assert!(store.get("demo").is_ok(), "nothing was deleted by the hostile ids");
    }

    #[test]
    fn listing_reports_broken_and_misnamed_folders_without_failing() {
        let store = ExtensionStore::new(temp("store"));
        fs::create_dir(store.root().join("broken")).unwrap();
        fs::write(store.root().join("broken/extension.json"), "{").unwrap();
        fs::create_dir(store.root().join("misnamed")).unwrap();
        fs::write(store.root().join("misnamed/extension.json"), manifest_json("other")).unwrap();
        fs::create_dir(store.root().join(".hidden")).unwrap();
        fs::write(store.root().join("stray.txt"), "x").unwrap();
        store.install(&ext_dir("good", "1.0.0")).unwrap();
        let l = store.list();
        assert_eq!(l.len(), 3);
        assert!(l.iter().find(|x| x.dir_name == "broken").unwrap().result.as_ref().unwrap_err().contains("extension.json"));
        assert!(l.iter().find(|x| x.dir_name == "misnamed").unwrap().result.as_ref().unwrap_err().contains("says id `other`"));
        assert!(l.iter().find(|x| x.dir_name == "good").unwrap().result.is_ok());
        assert_eq!(store.extensions().len(), 1);
    }

    #[test]
    fn bundled_extensions_are_offered_once() {
        let store = ExtensionStore::new(temp("store").join("extensions"));
        let bundled = temp("bundled");
        for id in ["one", "two"] {
            let d = bundled.join(id);
            fs::create_dir(&d).unwrap();
            fs::write(d.join("extension.json"), manifest_json(id)).unwrap();
        }
        fs::create_dir(bundled.join("junk")).unwrap();
        let (done, problems) = store.seed_bundled(&bundled);
        assert_eq!((done.len(), problems.len()), (2, 0));
        store.remove("one").unwrap();
        let (done, _) = store.seed_bundled(&bundled);
        assert!(done.is_empty(), "a removed bundled extension is not brought back");
        assert_eq!(store.extensions().len(), 1);
        assert_eq!(store.seed_bundled(&bundled.join("missing")), (vec![], vec![]));
    }

    /// An extension that starts a program is offered, never installed behind the user's back.
    #[test]
    fn bundled_extensions_that_run_programs_wait_for_a_yes() {
        let store = ExtensionStore::new(temp("store").join("extensions"));
        let bundled = temp("bundled-prog");
        let safe = bundled.join("safe");
        fs::create_dir(&safe).unwrap();
        fs::write(safe.join("extension.json"), manifest_json("safe")).unwrap();
        let risky = bundled.join("risky");
        fs::create_dir_all(risky.join("scripts")).unwrap();
        fs::write(risky.join("scripts/go.sh"), "#!/bin/sh\n").unwrap();
        let m = serde_json::json!({"formatVersion": 1, "id": "risky", "name": "Risky", "version": "1.0.0",
            "entries": [{"label": "Go", "action": {"exec": {"program": "scripts/go.sh"}}}]});
        fs::write(risky.join("extension.json"), m.to_string()).unwrap();
        let (done, problems) = store.seed_bundled(&bundled);
        assert_eq!((done, problems.len()), (vec!["safe".to_string()], 0));
        assert!(store.get("risky").is_err(), "not installed");
        let offers = store.offers(&bundled);
        assert_eq!(offers.len(), 1);
        assert!((offers[0].id.as_str(), offers[0].runs_programs, offers[0].answered) == ("risky", true, false));
        assert_eq!(offers[0].programs, ["scripts/go.sh"]);
        // asking again later (next launch) offers it again until the user answers
        let (done, _) = store.seed_bundled(&bundled);
        assert!(done.is_empty());
        assert!(!store.offers(&bundled)[0].answered);
        store.decline_offer("risky").unwrap();
        assert!(store.offers(&bundled)[0].answered, "declined: still listed for the manager, not asked again");
        let r = store.install_offer(&offers[0].dir).unwrap();
        assert!(r.runs_programs);
        assert!(store.offers(&bundled).is_empty(), "installed");
        store.remove("risky").unwrap();
        assert!(store.offers(&bundled)[0].answered, "a removed extension is not re-offered");
    }
}
