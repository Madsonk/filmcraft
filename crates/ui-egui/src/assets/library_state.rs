//! Favourites and recently used resources (ressources-v2, L3): `<data>/assets/library-state.json`.
//! By path, so they survive a re-index. "Recent" = placed on the timeline (dropped or applied),
//! not merely hovered; the 50 latest, most recent first.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const RECENTS_MAX: usize = 50;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryState {
    pub favorites: HashSet<String>,
    /// Most recent first.
    pub recents: Vec<String>,
}

impl LibraryState {
    pub fn load(path: &Path) -> LibraryState {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) {
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let tmp = path.with_extension("json.tmp");
        if let Ok(b) = serde_json::to_vec_pretty(self)
            && std::fs::write(&tmp, b).is_ok()
        {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    /// Star / unstar; returns whether it is a favourite now.
    pub fn toggle_favorite(&mut self, path: &str) -> bool {
        if self.favorites.remove(path) {
            false
        } else {
            self.favorites.insert(path.to_string());
            true
        }
    }

    /// `path` was just used.
    pub fn used(&mut self, path: &str) {
        self.recents.retain(|p| p != path);
        self.recents.insert(0, path.to_string());
        self.recents.truncate(RECENTS_MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recents_move_to_the_front_and_keep_fifty() {
        let mut s = LibraryState::default();
        for i in 0..60 {
            s.used(&format!("/s/{i}.wav"));
        }
        s.used("/s/10.wav");
        assert_eq!(s.recents.len(), RECENTS_MAX);
        assert_eq!(s.recents[0], "/s/10.wav");
        assert_eq!(s.recents.iter().filter(|p| *p == "/s/10.wav").count(), 1);
        assert!(s.toggle_favorite("/s/1.wav") && !s.toggle_favorite("/s/1.wav"));
        let d = std::env::temp_dir().join(format!("fc-libstate-{}", std::process::id()));
        let p = d.join("library-state.json");
        s.toggle_favorite("/s/2.wav");
        s.save(&p);
        assert_eq!(LibraryState::load(&p), s);
    }
}
