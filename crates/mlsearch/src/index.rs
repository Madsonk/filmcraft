//! The vector index: one unit vector per photo (keyed by a hash of the photo's content key, so a
//! changed file simply isn't found and is embedded again), stored as 16-bit floats in an
//! append-only file next to the catalog, and searched by brute force — 30 000 × 768 floats is a
//! few milliseconds on all cores. Also the perceptual-hash store used to find near-duplicates.

use std::collections::HashMap;
use std::path::Path;

use half::f16;
use rayon::prelude::*;

use crate::records::RecordFile;

/// 64-bit key of a content key (SipHash-1-3 with fixed keys: the same on every run and machine).
pub fn key_of(content_key: &str) -> u64 {
    use std::hash::Hasher;
    let mut h = siphasher::sip::SipHasher13::new_with_keys(0x4c43_5345_4152_4348, 0);
    h.write(content_key.as_bytes());
    h.finish()
}

pub struct VectorIndex {
    dim: usize,
    keys: Vec<u64>,
    data: Vec<f32>,
    pos: HashMap<u64, usize>,
    file: Option<RecordFile>,
}

impl VectorIndex {
    /// An index of `dim`-dimensional vectors made by `model`, kept in `path` (`None`: memory only).
    pub fn open(path: Option<&Path>, model: &str, dim: usize) -> std::io::Result<Self> {
        let mut me = Self { dim, keys: Vec::new(), data: Vec::new(), pos: HashMap::new(), file: None };
        if let Some(p) = path {
            let rec = 8 + dim * 2;
            let (file, bytes) = RecordFile::open(p, b"LCSV", model, rec)?;
            for r in bytes.chunks_exact(rec) {
                let key = u64::from_le_bytes(r[..8].try_into().unwrap_or_default());
                let v = r[8..].as_chunks::<2>().0.iter().map(|c| f16::from_le_bytes(*c).to_f32());
                me.insert(key, v);
            }
            me.file = Some(file);
        }
        Ok(me)
    }

    fn insert(&mut self, key: u64, v: impl Iterator<Item = f32>) {
        let start = self.data.len();
        self.data.extend(v);
        if self.data.len() - start != self.dim {
            self.data.truncate(start);
            return;
        }
        match self.pos.get(&key) {
            // (a later record replaces an earlier one)
            Some(&i) => {
                let (a, b) = (i * self.dim, start);
                let new: Vec<f32> = self.data[b..b + self.dim].to_vec();
                self.data[a..a + self.dim].copy_from_slice(&new);
                self.data.truncate(start);
            }
            None => {
                self.pos.insert(key, self.keys.len());
                self.keys.push(key);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn contains(&self, key: u64) -> bool {
        self.pos.contains_key(&key)
    }

    pub fn get(&self, key: u64) -> Option<&[f32]> {
        let i = *self.pos.get(&key)?;
        self.data.get(i * self.dim..(i + 1) * self.dim)
    }

    /// Bytes the index takes on disk per photo.
    pub fn bytes_per_vector(&self) -> usize {
        8 + self.dim * 2
    }

    /// Add vectors (each rounded to 16-bit floats, as it will be on disk) and write them.
    pub fn add(&mut self, entries: &[(u64, Vec<f32>)]) -> std::io::Result<()> {
        let mut out = Vec::with_capacity(entries.len() * self.bytes_per_vector());
        for (key, v) in entries {
            if v.len() != self.dim {
                continue;
            }
            out.extend_from_slice(&key.to_le_bytes());
            let halves: Vec<f16> = v.iter().map(|x| f16::from_f32(*x)).collect();
            halves.iter().for_each(|h| out.extend_from_slice(&h.to_le_bytes()));
            self.insert(*key, halves.iter().map(|h| h.to_f32()));
        }
        match &mut self.file {
            Some(f) => f.append(&out),
            None => Ok(()),
        }
    }

    /// The `k` best matches for a unit `query` among the keys `keep` accepts, best first:
    /// `(key, cosine)`.
    pub fn top(&self, query: &[f32], k: usize, keep: &(dyn Fn(u64) -> bool + Sync)) -> Vec<(u64, f32)> {
        if query.len() != self.dim || k == 0 {
            return Vec::new();
        }
        let scores: Vec<f32> = self.data.par_chunks_exact(self.dim).map(|v| dot(v, query)).collect();
        let mut ids: Vec<usize> = (0..self.keys.len()).filter(|i| keep(self.keys[*i])).collect();
        let k = k.min(ids.len());
        if k == 0 {
            return Vec::new();
        }
        if k < ids.len() {
            ids.select_nth_unstable_by(k - 1, |a, b| scores[*b].total_cmp(&scores[*a]));
            ids.truncate(k);
        }
        ids.sort_by(|a, b| scores[*b].total_cmp(&scores[*a]));
        ids.into_iter().map(|i| (self.keys[i], scores[i])).collect()
    }
}

/// Dot product (four lanes, so the compiler vectorizes it).
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0f32; 4];
    for (x, y) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
        for l in 0..4 {
            acc[l] += x[l] * y[l];
        }
    }
    acc.iter().sum()
}

/// Perceptual hashes (see [`crate::hash`]) by key, in their own file: they don't depend on the
/// model, so near-duplicates can be found without it.
pub struct HashStore {
    map: HashMap<u64, [u64; 2]>,
    file: Option<RecordFile>,
}

impl HashStore {
    pub fn open(path: Option<&Path>) -> std::io::Result<Self> {
        let mut me = Self { map: HashMap::new(), file: None };
        if let Some(p) = path {
            let (file, bytes) = RecordFile::open(p, b"LCSH", crate::hash::TAG, 24)?;
            for r in bytes.as_chunks::<24>().0 {
                let k = u64::from_le_bytes(r[..8].try_into().unwrap_or_default());
                let a = u64::from_le_bytes(r[8..16].try_into().unwrap_or_default());
                let b = u64::from_le_bytes(r[16..24].try_into().unwrap_or_default());
                me.map.insert(k, [a, b]);
            }
            me.file = Some(file);
        }
        Ok(me)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn get(&self, key: u64) -> Option<[u64; 2]> {
        self.map.get(&key).copied()
    }

    pub fn add(&mut self, entries: &[(u64, [u64; 2])]) -> std::io::Result<()> {
        let mut out = Vec::with_capacity(entries.len() * 24);
        for (k, h) in entries {
            out.extend_from_slice(&k.to_le_bytes());
            out.extend_from_slice(&h[0].to_le_bytes());
            out.extend_from_slice(&h[1].to_le_bytes());
            self.map.insert(*k, *h);
        }
        match &mut self.file {
            Some(f) => f.append(&out),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(seed: u32, dim: usize) -> Vec<f32> {
        let mut v: Vec<f32> = (0..dim).map(|i| ((seed as usize * 31 + i * 17) % 23) as f32 - 11.0).collect();
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter_mut().for_each(|x| *x /= n);
        v
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("lc-search-{name}-{}", std::process::id()))
    }

    #[test]
    fn nearest_first_and_survives_reopening_and_a_torn_tail() {
        let p = tmp("vec");
        let _ = std::fs::remove_file(&p);
        let mut ix = VectorIndex::open(Some(&p), "m", 16).unwrap();
        let entries: Vec<(u64, Vec<f32>)> = (0..50).map(|i| (i as u64, unit(i, 16))).collect();
        ix.add(&entries).unwrap();
        let top = ix.top(&unit(7, 16), 3, &|_| true);
        assert_eq!(top[0].0, 7);
        assert!(top[0].1 > 0.999 && top[0].1 >= top[1].1 && top[1].1 >= top[2].1);
        assert!(ix.top(&unit(7, 16), 3, &|k| k != 7)[0].0 != 7);
        drop(ix);
        // a crash in the middle of a record: cut when reopened, the rest is intact
        let mut b = std::fs::read(&p).unwrap();
        b.extend_from_slice(&[1, 2, 3]);
        std::fs::write(&p, &b).unwrap();
        let ix = VectorIndex::open(Some(&p), "m", 16).unwrap();
        assert_eq!(ix.len(), 50);
        assert_eq!(ix.top(&unit(7, 16), 1, &|_| true)[0].0, 7);
        drop(ix);
        // another model: the old vectors are not used
        assert_eq!(VectorIndex::open(Some(&p), "other", 16).unwrap().len(), 0);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn hashes_persist() {
        let p = tmp("hash");
        let _ = std::fs::remove_file(&p);
        let mut h = HashStore::open(Some(&p)).unwrap();
        h.add(&[(1, [2, 3]), (4, [5, 6])]).unwrap();
        drop(h);
        let h = HashStore::open(Some(&p)).unwrap();
        assert_eq!((h.len(), h.get(4)), (2, Some([5, 6])));
        let _ = std::fs::remove_file(&p);
    }
}
