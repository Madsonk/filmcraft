//! Append-only files of fixed-size records that survive a crash: a small header (magic, format,
//! record size, a tag naming what the records mean), then records. A torn last record is cut off
//! when the file is opened; a file for another tag or format is replaced (these files are caches
//! of work that can be redone, and are never the only copy of anything).

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

const VERSION: u32 = 1;

pub struct RecordFile {
    file: File,
}

fn header(magic: &[u8; 4], rec_len: usize, tag: &str) -> Vec<u8> {
    let mut h = magic.to_vec();
    h.extend_from_slice(&VERSION.to_le_bytes());
    h.extend_from_slice(&(rec_len as u32).to_le_bytes());
    h.extend_from_slice(&(tag.len() as u32).to_le_bytes());
    h.extend_from_slice(tag.as_bytes());
    h
}

impl RecordFile {
    /// Open (or create) `path`; returns the file and the records it holds, concatenated.
    pub fn open(path: &Path, magic: &[u8; 4], tag: &str, rec_len: usize) -> std::io::Result<(Self, Vec<u8>)> {
        let want = header(magic, rec_len, tag);
        let mut bytes = Vec::new();
        if let Ok(mut f) = File::open(path) {
            f.read_to_end(&mut bytes)?;
        }
        if bytes.len() >= want.len() && bytes[..want.len()] == want[..] {
            let n = (bytes.len() - want.len()) / rec_len;
            let keep = want.len() + n * rec_len;
            let file = OpenOptions::new().append(true).open(path)?;
            if keep < bytes.len() {
                file.set_len(keep as u64)?;
                file.sync_data()?;
            }
            return Ok((Self { file }, bytes[want.len()..keep].to_vec()));
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("part");
        {
            let mut f = File::create(&tmp)?;
            f.write_all(&want)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        let file = OpenOptions::new().append(true).open(path)?;
        Ok((Self { file }, Vec::new()))
    }

    /// Append whole records and make them durable.
    pub fn append(&mut self, records: &[u8]) -> std::io::Result<()> {
        self.file.write_all(records)?;
        self.file.sync_data()
    }
}
