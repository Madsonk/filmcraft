//! The tokenizers of SigLIP (Google, Apache-2.0), read from the model's own `tokenizer.json`
//! (a data file; nothing is embedded):
//!
//! - **SigLIP 1** (`google/siglip-base-patch16-224`): SentencePiece *Unigram*, 32 000 pieces.
//!   Text is lower-cased, ASCII punctuation is dropped, runs of blanks collapse, every word is
//!   prefixed with `▁`, and the best segmentation (Viterbi on the pieces' log-probabilities) is
//!   taken. Characters outside the vocabulary fall back to byte pieces (`<0xE9>`).
//! - **SigLIP 2** (`google/siglip2-base-patch16-224`): the Gemma *BPE* (256 000 pieces): blanks
//!   become `▁`, characters are merged by rank, unknown characters fall back to byte pieces.
//!   The model expects lower-cased text.
//!
//! Both end with the end-of-sequence piece and pad to [`CONTEXT`] tokens: the text tower reads
//! its last position, so the padding is part of the input.
//!
//! No Unicode normalization table is applied (the Unigram file carries SentencePiece's NFKC map;
//! text typed on a keyboard is already composed, so it makes no difference in practice).

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::{Error, Result};

/// Tokens the text tower takes.
pub const CONTEXT: usize = 64;
/// `tokenizer.json` is capped (the Gemma one is 34 MB).
const MAX_FILE: u64 = 64 << 20;
const SPACE: char = '\u{2581}';

#[derive(Deserialize)]
struct File {
    model: Model,
}

#[derive(Deserialize)]
struct Model {
    #[serde(rename = "type")]
    kind: String,
    vocab: Vocab,
    #[serde(default)]
    merges: Vec<Merge>,
    #[serde(default)]
    unk_id: Option<u32>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Vocab {
    /// Unigram: `[piece, log-probability]`, the index is the id.
    List(Vec<(String, f64)>),
    /// BPE: `piece → id`.
    Map(HashMap<String, u32>),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Merge {
    Joined(String),
    Pair(String, String),
}

enum Kind {
    Unigram { pieces: HashMap<String, (u32, f32)>, longest: usize },
    Bpe { vocab: HashMap<String, u32>, ranks: HashMap<String, u32> },
}

pub struct Tokenizer {
    kind: Kind,
    eos: u32,
    pad: u32,
    unk: u32,
    bytes: [Option<u32>; 256],
}

impl Tokenizer {
    /// From `tokenizer.json` in `dir`.
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join("tokenizer.json");
        let meta = std::fs::metadata(&path).map_err(|e| Error::Model(format!("tokenizer.json: {e}")))?;
        if meta.len() > MAX_FILE {
            return Err(Error::Model("tokenizer.json is too large".into()));
        }
        let text = std::fs::read_to_string(&path).map_err(|e| Error::Model(format!("tokenizer.json: {e}")))?;
        Self::from_json(&text)
    }

    pub fn from_json(text: &str) -> Result<Self> {
        let f: File = serde_json::from_str(text).map_err(|e| Error::Model(format!("tokenizer.json: {e}")))?;
        let m = f.model;
        let mut bytes = [None; 256];
        let (kind, eos, pad, unk) = match (m.kind.as_str(), m.vocab) {
            ("Unigram", Vocab::List(list)) => {
                let mut pieces = HashMap::with_capacity(list.len());
                let mut longest = 1;
                for (id, (p, s)) in list.into_iter().enumerate() {
                    let id = id as u32;
                    if let Some(b) = byte_piece(&p) {
                        bytes[b as usize] = Some(id);
                    }
                    longest = longest.max(p.chars().count());
                    pieces.insert(p, (id, s as f32));
                }
                // SentencePiece (SigLIP 1): `</s>` = 1 ends the text and pads; `<unk>` = 2
                (Kind::Unigram { pieces, longest }, 1, 1, m.unk_id.unwrap_or(2))
            }
            ("BPE", Vocab::Map(vocab)) => {
                for b in 0..=255u8 {
                    bytes[b as usize] = vocab.get(&format!("<0x{b:02X}>")).copied();
                }
                let ranks = m
                    .merges
                    .into_iter()
                    .enumerate()
                    .map(|(i, mg)| {
                        let k = match mg {
                            Merge::Joined(s) => s,
                            Merge::Pair(a, b) => format!("{a} {b}"),
                        };
                        (k, i as u32)
                    })
                    .collect();
                // Gemma: `<pad>` = 0, `<eos>` = 1, `<unk>` = 3
                (Kind::Bpe { vocab, ranks }, 1, 0, 3)
            }
            (other, _) => return Err(Error::Model(format!("unsupported tokenizer `{other}`"))),
        };
        Ok(Self { kind, eos, pad, unk, bytes })
    }

    /// The `CONTEXT` ids of `text`: tokens, end of text, padding.
    pub fn encode(&self, text: &str) -> Vec<u32> {
        let lower = text.to_lowercase();
        let mut ids = match &self.kind {
            Kind::Unigram { .. } => {
                let clean: String =
                    lower.chars().filter(|c| !"!\"#$%&'()*+,-.:;=?@[\\]^_`{|}~".contains(*c)).map(|c| if c.is_whitespace() { ' ' } else { c }).collect();
                let mut ids = Vec::new();
                for word in clean.split_whitespace() {
                    let w: Vec<char> = std::iter::once(SPACE).chain(word.chars()).collect();
                    ids.extend(self.unigram(&w));
                }
                ids
            }
            Kind::Bpe { .. } => {
                let w: Vec<char> = lower.chars().map(|c| if c == ' ' { SPACE } else { c }).collect();
                self.bpe(&w)
            }
        };
        ids.truncate(CONTEXT - 1);
        ids.push(self.eos);
        ids.resize(CONTEXT, self.pad);
        ids
    }

    /// Byte-fallback pieces for `c` (or `<unk>`).
    fn fallback(&self, c: char, out: &mut Vec<u32>) {
        let mut buf = [0u8; 4];
        let enc = c.encode_utf8(&mut buf).as_bytes();
        if enc.iter().all(|b| self.bytes[*b as usize].is_some()) {
            out.extend(enc.iter().filter_map(|b| self.bytes[*b as usize]));
        } else {
            out.push(self.unk);
        }
    }

    fn unigram(&self, w: &[char]) -> Vec<u32> {
        let Kind::Unigram { pieces, longest } = &self.kind else { return Vec::new() };
        let n = w.len();
        // best[i] = (score of the best segmentation of w[..i], start of its last piece)
        let mut best: Vec<(f32, usize)> = vec![(f32::NEG_INFINITY, 0); n + 1];
        best[0].0 = 0.0;
        for i in 0..n {
            if best[i].0 == f32::NEG_INFINITY {
                continue;
            }
            let mut any = false;
            let mut s = String::new();
            for j in i + 1..=n.min(i + longest) {
                s.push(w[j - 1]);
                if let Some((_, score)) = pieces.get(&s) {
                    any = true;
                    let v = best[i].0 + score;
                    if v > best[j].0 {
                        best[j] = (v, i);
                    }
                }
            }
            if !any {
                // an unknown character: bytes (or `<unk>`) at a fixed, heavy cost
                let v = best[i].0 - 20.0;
                if v > best[i + 1].0 {
                    best[i + 1] = (v, i);
                }
            }
        }
        let mut cuts = Vec::new();
        let mut j = n;
        while j > 0 {
            let i = best[j].1;
            cuts.push((i, j));
            j = i;
        }
        let mut out = Vec::new();
        for (i, j) in cuts.into_iter().rev() {
            let s: String = w[i..j].iter().collect();
            match pieces.get(&s) {
                Some((id, _)) => out.push(*id),
                None => w[i..j].iter().for_each(|c| self.fallback(*c, &mut out)),
            }
        }
        out
    }

    fn bpe(&self, w: &[char]) -> Vec<u32> {
        let Kind::Bpe { vocab, ranks } = &self.kind else { return Vec::new() };
        // symbols: one per character, or per byte when the character has no piece
        let mut sym: Vec<String> = Vec::new();
        for c in w {
            let s = c.to_string();
            if vocab.contains_key(&s) {
                sym.push(s);
            } else {
                let mut buf = [0u8; 4];
                for b in c.encode_utf8(&mut buf).as_bytes() {
                    sym.push(format!("<0x{b:02X}>"));
                }
            }
        }
        loop {
            let mut pick: Option<(u32, usize)> = None;
            for i in 0..sym.len().saturating_sub(1) {
                if let Some(r) = ranks.get(&format!("{} {}", sym[i], sym[i + 1]))
                    && pick.is_none_or(|(br, _)| *r < br)
                {
                    pick = Some((*r, i));
                }
            }
            let Some((_, i)) = pick else { break };
            let merged = format!("{}{}", sym[i], sym[i + 1]);
            sym[i] = merged;
            sym.remove(i + 1);
        }
        sym.iter().map(|s| vocab.get(s).copied().unwrap_or(self.unk)).collect()
    }
}

/// `<0xE9>` → 0xE9.
fn byte_piece(p: &str) -> Option<u8> {
    let h = p.strip_prefix("<0x")?.strip_suffix('>')?;
    (h.len() == 2).then(|| u8::from_str_radix(h, 16).ok()).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNIGRAM: &str = r#"{"model":{"type":"Unigram","unk_id":2,"vocab":[["<pad>",0],["</s>",0],["<unk>",0],["▁a",-1.0],["▁photo",-2.0],["▁pho",-3.0],["to",-3.0],["▁of",-2.0],["▁",-5.0],["<0xC3>",-7.0],["<0xA9>",-7.0],["a",-4.0],["b",-4.0]]}}"#;
    const BPE: &str = r#"{"model":{"type":"BPE","vocab":{"<pad>":0,"<eos>":1,"<bos>":2,"<unk>":3,"h":4,"i":5,"▁":6,"hi":7,"▁hi":8,"t":9,"<0xC3>":10,"<0xA9>":11},"merges":["h i",["▁","hi"]]}}"#;

    #[test]
    fn unigram_takes_the_best_segmentation_drops_punctuation_and_pads() {
        let t = Tokenizer::from_json(UNIGRAM).unwrap();
        let ids = t.encode("A Photo, of a");
        assert_eq!(&ids[..5], &[3, 4, 7, 3, 1]);
        assert_eq!(ids.len(), CONTEXT);
        assert!(ids[5..].iter().all(|i| *i == 1));
        // a character outside the vocabulary falls back to its UTF-8 bytes
        assert_eq!(&t.encode("é")[..4], &[8, 9, 10, 1]);
    }

    #[test]
    fn bpe_merges_by_rank_and_pads_with_the_pad_piece() {
        let t = Tokenizer::from_json(BPE).unwrap();
        let ids = t.encode(" Hi");
        assert_eq!(&ids[..2], &[8, 1]);
        assert!(ids[2..].iter().all(|i| *i == 0));
        assert_eq!(&t.encode("é")[..3], &[10, 11, 1]);
    }

    #[test]
    fn long_text_is_cut_to_the_context_and_still_ends_with_eos() {
        let t = Tokenizer::from_json(UNIGRAM).unwrap();
        let ids = t.encode(&"a ".repeat(200));
        assert_eq!(ids.len(), CONTEXT);
        assert_eq!(ids[CONTEXT - 1], 1);
        assert!(ids[..CONTEXT - 1].iter().all(|i| *i == 3));
    }
}
