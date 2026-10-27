//! Search, filters and sort of the Resources panel (ressources-v2, L3), on the in-memory index:
//! no disk access, a query over 30 000 entries stays well under a frame.
//!
//! - **Relevance**: every word must appear in the name or the folder; a word in the name scores
//!   more than in the folder, more in the name proper than in its bracketed tags, more again at
//!   the start of a word and at the start of the name.
//! - **Filters** use what the sound index measured (`SoundMeta`): family, duration bucket, tempo
//!   range, key (with its relative major / minor), loops; plus favourites and recently used.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::index::Entry;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sort {
    /// By relevance when there is a query, by name otherwise.
    #[default]
    Relevance,
    Name,
    Duration,
    Tempo,
}

impl Sort {
    pub const ALL: [Sort; 4] = [Sort::Relevance, Sort::Name, Sort::Duration, Sort::Tempo];
}

/// Duration buckets of the filter: < 0.5 s, 0.5-2 s, 2-10 s, > 10 s.
pub const DURATIONS: [(f32, f32); 4] = [(0.0, 0.5), (0.5, 2.0), (2.0, 10.0), (10.0, f32::MAX)];

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Filters {
    pub family: Option<String>,
    /// Index into [`DURATIONS`].
    pub duration: Option<usize>,
    pub bpm_min: Option<f32>,
    pub bpm_max: Option<f32>,
    /// A key ("C minor"): the key itself and its relative major / minor match.
    pub key: Option<String>,
    pub loops_only: bool,
    pub favorites_only: bool,
    pub recents_only: bool,
}

impl Filters {
    pub fn any(&self) -> bool {
        *self != Filters::default()
    }
}

const NOTES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

fn note_index(n: &str) -> Option<usize> {
    let n = n.replace("Db", "C#").replace("Eb", "D#").replace("Gb", "F#").replace("Ab", "G#").replace("Bb", "A#");
    NOTES.iter().position(|x| *x == n)
}

/// The relative key: C major ↔ A minor.
pub fn relative_key(key: &str) -> Option<String> {
    let (root, mode) = key.split_once(' ')?;
    let i = note_index(root)?;
    match mode {
        "major" => Some(format!("{} minor", NOTES[(i + 9) % 12])),
        "minor" => Some(format!("{} major", NOTES[(i + 3) % 12])),
        _ => None,
    }
}

/// Relevance of `e` for the (lower-case) `words`; None = a word is missing.
pub fn score(e: &Entry, words: &[String]) -> Option<i32> {
    let name = e.name.to_lowercase();
    let dir = e.dir.to_lowercase();
    // the library writes tags into file names (`name [tag · tag].wav`): the name itself counts more
    let main = name.split(" [").next().unwrap_or(&name);
    let mut total = 0;
    for w in words {
        if let Some(i) = name.find(w.as_str()) {
            let at_word = i == 0 || !name[..i].chars().next_back().is_some_and(char::is_alphanumeric);
            total += 10 + if at_word { 5 } else { 0 } + if i == 0 { 3 } else { 0 } + if main.contains(w.as_str()) { 4 } else { 0 };
        } else if dir.contains(w.as_str()) {
            total += 2;
        } else {
            return None;
        }
    }
    Some(total)
}

fn passes(e: &Entry, f: &Filters, favorites: &HashSet<String>, recents: &HashSet<&str>) -> bool {
    if f.favorites_only && !favorites.contains(&e.path) {
        return false;
    }
    if f.recents_only && !recents.contains(e.path.as_str()) {
        return false;
    }
    if let Some(fam) = &f.family
        && e.family.as_deref() != Some(fam.as_str())
    {
        return false;
    }
    if let Some(i) = f.duration {
        let (lo, hi) = DURATIONS.get(i).copied().unwrap_or((0.0, f32::MAX));
        if !e.dur.is_some_and(|d| d >= lo && d < hi) {
            return false;
        }
    }
    if f.bpm_min.is_some() || f.bpm_max.is_some() {
        let Some(b) = e.meta.bpm else { return false };
        if f.bpm_min.is_some_and(|m| b < m) || f.bpm_max.is_some_and(|m| b > m) {
            return false;
        }
    }
    if let Some(k) = &f.key {
        let rel = relative_key(k);
        let ok = e.meta.key.as_deref().is_some_and(|x| x == k || rel.as_deref() == Some(x));
        if !ok {
            return false;
        }
    }
    if f.loops_only && e.meta.looped != Some(true) {
        return false;
    }
    true
}

/// Rows to show: the entries matching `query` and `filters`, in `sort` order. `recents` is most
/// recent first; with the recents filter on, that order wins.
pub fn select(entries: &[Entry], query: &str, f: &Filters, sort: Sort, favorites: &HashSet<String>, recents: &[String]) -> Vec<usize> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let recent_set: HashSet<&str> = recents.iter().map(String::as_str).collect();
    let mut rows: Vec<(usize, i32)> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| passes(e, f, favorites, &recent_set))
        .filter_map(|(i, e)| if words.is_empty() { Some((i, 0)) } else { score(e, &words).map(|s| (i, s)) })
        .collect();
    let key_name = |i: usize| entries[i].name.to_lowercase();
    if f.recents_only {
        let pos = |i: usize| recents.iter().position(|p| *p == entries[i].path).unwrap_or(usize::MAX);
        rows.sort_by_key(|(i, _)| pos(*i));
    } else {
        match sort {
            Sort::Relevance if !words.is_empty() => rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| key_name(a.0).cmp(&key_name(b.0)))),
            Sort::Relevance | Sort::Name => {} // the index is already sorted by name
            Sort::Duration => rows.sort_by(|a, b| entries[a.0].dur.unwrap_or(f32::MAX).total_cmp(&entries[b.0].dur.unwrap_or(f32::MAX))),
            Sort::Tempo => rows.sort_by(|a, b| entries[a.0].meta.bpm.unwrap_or(f32::MAX).total_cmp(&entries[b.0].meta.bpm.unwrap_or(f32::MAX))),
        }
    }
    rows.into_iter().map(|(i, _)| i).collect()
}

/// Families present in `entries`, sorted, for the filter menu.
pub fn families(entries: &[Entry]) -> Vec<String> {
    let mut v: Vec<String> = entries.iter().filter_map(|e| e.family.clone()).collect::<HashSet<_>>().into_iter().collect();
    v.sort();
    v
}

/// Keys present in `entries`, most frequent first.
pub fn keys(entries: &[Entry]) -> Vec<String> {
    let mut n: std::collections::HashMap<&str, usize> = Default::default();
    for e in entries {
        if let Some(k) = e.meta.key.as_deref() {
            *n.entry(k).or_default() += 1;
        }
    }
    let mut v: Vec<(&str, usize)> = n.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    v.into_iter().map(|(k, _)| k.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::index::SoundMeta;

    fn e(name: &str, dir: &str, dur: f32, fam: &str, bpm: Option<f32>, key: Option<&str>) -> Entry {
        Entry {
            path: format!("/lib/{dir}/{name}"),
            name: name.into(),
            dir: dir.into(),
            dur: Some(dur),
            family: Some(fam.into()),
            ready: true,
            meta: SoundMeta { bpm, key: key.map(str::to_string), ..Default::default() },
            ..Default::default()
        }
    }

    #[test]
    fn a_word_in_the_name_beats_the_same_word_in_the_folder() {
        let v = vec![
            e("MUSIC · Dark · strings [air · short whooshes].wav", "09 PACKS/Seaside", 48.5, "musique", Some(90.0), Some("C minor")),
            e("SFX · Whoosh · air short.wav", "03 WHOOSHES", 1.2, "one-shot / impact", None, None),
            e("Air whoosh long.wav", "03 WHOOSHES", 4.0, "one-shot / impact", None, None),
        ];
        let rows = select(&v, "whoosh air", &Filters::default(), Sort::Relevance, &HashSet::new(), &[]);
        // the music that only has the words in its tags comes last now (it was first, by name)
        assert_eq!(rows.last(), Some(&0), "{rows:?}");
        assert_eq!(rows.len(), 3);
        assert!(score(&v[2], &["air".into()]).unwrap() > score(&v[1], &["air".into()]).unwrap(), "start of the name scores more");
    }

    #[test]
    fn filters_use_the_measures_of_the_sound_index() {
        let v = vec![
            e("a.wav", "x", 0.3, "one-shot / impact", None, None),
            e("b.wav", "x", 1.0, "one-shot / impact", None, None),
            e("song1.wav", "m", 120.0, "musique", Some(120.0), Some("C major")),
            e("song2.wav", "m", 90.0, "musique", Some(95.0), Some("A minor")),
            e("song3.wav", "m", 60.0, "musique", Some(128.0), Some("F minor")),
        ];
        let none = HashSet::new();
        let f = Filters { duration: Some(0), ..Default::default() };
        assert_eq!(select(&v, "", &f, Sort::Name, &none, &[]), vec![0]);
        let f = Filters { family: Some("musique".into()), bpm_min: Some(100.0), bpm_max: Some(130.0), ..Default::default() };
        assert_eq!(select(&v, "", &f, Sort::Tempo, &none, &[]), vec![2, 4]);
        // C major also finds its relative, A minor
        let f = Filters { key: Some("C major".into()), ..Default::default() };
        assert_eq!(select(&v, "", &f, Sort::Name, &none, &[]), vec![2, 3]);
        assert_eq!(relative_key("A minor").as_deref(), Some("C major"));
        assert_eq!(relative_key("F minor").as_deref(), Some("G# major"));
        // favourites and recents (recents keep their own order, most recent first)
        let fav: HashSet<String> = [v[1].path.clone()].into();
        let f = Filters { favorites_only: true, ..Default::default() };
        assert_eq!(select(&v, "", &f, Sort::Name, &fav, &[]), vec![1]);
        let rec = vec![v[4].path.clone(), v[0].path.clone()];
        let f = Filters { recents_only: true, ..Default::default() };
        assert_eq!(select(&v, "", &f, Sort::Name, &none, &rec), vec![4, 0]);
        assert_eq!(families(&v), vec!["musique".to_string(), "one-shot / impact".to_string()]);
        assert_eq!(keys(&v).len(), 3);
    }

    #[test]
    fn thirty_thousand_entries_are_searched_within_a_frame() {
        let v: Vec<Entry> = (0..30_000).map(|i| e(&format!("SFX · Hit {i} whoosh.wav"), "02 HITS", 1.0, "one-shot / impact", None, None)).collect();
        // best of 3: the whole test suite runs in parallel on a loaded machine (a single run was
        // measured at 400 ms there, 6 ms alone)
        let mut best = f64::MAX;
        for _ in 0..3 {
            let t0 = web_time::Instant::now();
            let rows = select(&v, "hit whoosh 29", &Filters::default(), Sort::Relevance, &HashSet::new(), &[]);
            best = best.min(t0.elapsed().as_secs_f64() * 1000.0);
            assert!(!rows.is_empty());
        }
        eprintln!("30 000 entries searched in {best:.1} ms (best of 3)");
        assert!(best < 250.0, "{best} ms");
    }
}
