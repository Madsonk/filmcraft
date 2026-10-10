//! Decoding Whisper token ids to text from the model's `tokenizer.json` (byte-level BPE: each
//! vocabulary entry is a string of "printable" stand-in characters, one per byte, using the
//! GPT-2 byte↔unicode table; special tokens are listed separately as added tokens).
//!
//! Decoding is what transcription needs. Encoding ([`Tokenizer::encode`]) exists for one purpose:
//! the *initial prompt* (a short text such as "Euh, alors, heu, hum…" that Whisper reads as the text
//! that came just before the audio, which makes it write hesitations and a speaker's vocabulary the
//! way the prompt does). It is GPT-2 byte-level BPE: the text is split with the GPT-2 pattern, each
//! piece is mapped to the byte↔unicode alphabet and merged by rank, the pieces are looked up in the
//! vocabulary.

use std::collections::HashMap;

use crate::SpeechError;

pub struct Tokenizer {
    /// Bytes of each regular token, by id.
    bytes: Vec<Vec<u8>>,
    /// Special (added) tokens by content and by id.
    special: HashMap<String, u32>,
    special_by_id: HashMap<u32, String>,
    pub eot: u32,
    pub sot: u32,
    pub transcribe: u32,
    pub translate: Option<u32>,
    pub no_timestamps: u32,
    pub no_speech: Option<u32>,
    /// `<|0.00|>`; every later id is a timestamp in 0.02 s steps.
    pub timestamp_begin: u32,
    /// Language tokens (`"en"` → id), multilingual models only.
    pub languages: Vec<(String, u32)>,
    /// `<|startofprev|>`: introduces the prompt text.
    pub start_of_prev: Option<u32>,
    /// Vocabulary (byte-alphabet string → id) and merge ranks, for [`Tokenizer::encode`].
    vocab: HashMap<String, u32>,
    merges: HashMap<(String, String), usize>,
    byte_to_char: Vec<char>,
}

/// GPT-2's byte ↔ printable character table: `(byte, char)` pairs.
fn byte_char_pairs() -> Vec<(u8, char)> {
    let mut bs: Vec<u32> = (b'!' as u32..=b'~' as u32).chain(0xA1..=0xAC).chain(0xAE..=0xFF).collect();
    let mut cs = bs.clone();
    let mut n = 0;
    for b in 0..256u32 {
        if !bs.contains(&b) {
            bs.push(b);
            cs.push(256 + n);
            n += 1;
        }
    }
    bs.iter().zip(&cs).filter_map(|(&b, &c)| Some((b as u8, char::from_u32(c)?))).collect()
}

/// GPT-2's byte → printable character table, inverted.
fn unicode_to_byte() -> HashMap<char, u8> {
    byte_char_pairs().into_iter().map(|(b, c)| (c, b)).collect()
}

/// The GPT-2 pre-tokenisation pattern, by hand:
/// `'s|'t|'re|'ve|'m|'ll|'d| ?\p{L}+| ?\p{N}+| ?[^\s\p{L}\p{N}]+|\s+(?!\S)|\s+`.
fn pretokenize(text: &str) -> Vec<String> {
    let c: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let class = |ch: char| -> u8 {
        if ch.is_alphabetic() {
            1
        } else if ch.is_numeric() {
            2
        } else if ch.is_whitespace() {
            0
        } else {
            3
        }
    };
    while i < c.len() {
        // contractions
        if c[i] == '\'' {
            let rest: String = c[i + 1..].iter().take(2).collect();
            let hit = ["re", "ve", "ll"]
                .iter()
                .find(|k| rest.starts_with(**k))
                .map(|k| k.len())
                .or_else(|| ["s", "t", "m", "d"].iter().find(|k| rest.starts_with(**k)).map(|k| k.len()));
            if let Some(n) = hit {
                out.push(c[i..i + 1 + n].iter().collect());
                i += 1 + n;
                continue;
            }
        }
        let start = i;
        let lead_space = c[i] == ' ' && i + 1 < c.len() && class(c[i + 1]) != 0;
        let body = if lead_space { i + 1 } else { i };
        let k = class(c[body]);
        if k != 0 {
            let mut j = body;
            while j < c.len() && class(c[j]) == k {
                j += 1;
            }
            out.push(c[start..j].iter().collect());
            i = j;
        } else {
            // whitespace run: keep the last space for the next word
            let mut j = i;
            while j < c.len() && class(c[j]) == 0 {
                j += 1;
            }
            if j < c.len() && j - i > 1 {
                j -= 1;
            }
            out.push(c[i..j].iter().collect());
            i = j;
        }
    }
    out
}

impl Tokenizer {
    pub fn from_json(json: &str) -> Result<Self, SpeechError> {
        let bad = |m: &str| SpeechError::Model(format!("tokenizer.json: {m}"));
        let v: serde_json::Value = serde_json::from_str(json).map_err(|e| bad(&e.to_string()))?;
        let vocab = v["model"]["vocab"].as_object().ok_or_else(|| bad("no model.vocab"))?;
        let table = unicode_to_byte();
        let byte_to_char = {
            let mut v = vec!['\0'; 256];
            for (b, ch) in byte_char_pairs() {
                v[b as usize] = ch;
            }
            v
        };
        let mut vocab_map: HashMap<String, u32> = HashMap::with_capacity(vocab.len());
        for (tok, id) in vocab {
            if let Some(id) = id.as_u64() {
                vocab_map.insert(tok.clone(), id as u32);
            }
        }
        // merges: ["a b", …] (older files) or [["a","b"], …] (newer ones)
        let mut merges: HashMap<(String, String), usize> = HashMap::new();
        if let Some(list) = v["model"]["merges"].as_array() {
            for (rank, m) in list.iter().enumerate() {
                let pair = match m {
                    serde_json::Value::String(s) => s.split_once(' ').map(|(a, b)| (a.to_string(), b.to_string())),
                    serde_json::Value::Array(a) if a.len() == 2 => {
                        Some((a[0].as_str().unwrap_or_default().to_string(), a[1].as_str().unwrap_or_default().to_string()))
                    }
                    _ => None,
                };
                if let Some(p) = pair {
                    merges.entry(p).or_insert(rank);
                }
            }
        }
        let max = vocab.values().filter_map(|x| x.as_u64()).max().unwrap_or(0) as usize;
        let mut bytes = vec![Vec::new(); max + 1];
        for (tok, id) in vocab {
            let id = id.as_u64().ok_or_else(|| bad("vocab id"))? as usize;
            bytes[id] = tok.chars().filter_map(|c| table.get(&c).copied()).collect();
        }
        let mut special = HashMap::new();
        let mut special_by_id = HashMap::new();
        for a in v["added_tokens"].as_array().ok_or_else(|| bad("no added_tokens"))? {
            let (Some(id), Some(c)) = (a["id"].as_u64(), a["content"].as_str()) else { continue };
            special.insert(c.to_string(), id as u32);
            special_by_id.insert(id as u32, c.to_string());
        }
        let get = |k: &str| special.get(k).copied();
        let need = |k: &str| get(k).ok_or_else(|| bad(&format!("no {k}")));
        let sot = need("<|startoftranscript|>")?;
        let translate = get("<|translate|>");
        let mut languages: Vec<(String, u32)> = special
            .iter()
            .filter(|(k, id)| **id > sot && translate.is_none_or(|t| **id < t) && k.starts_with("<|") && k.ends_with("|>"))
            .map(|(k, id)| (k[2..k.len() - 2].to_string(), *id))
            .collect();
        languages.sort_by_key(|x| x.1);
        Ok(Self {
            eot: need("<|endoftext|>")?,
            sot,
            transcribe: need("<|transcribe|>")?,
            translate,
            no_timestamps: need("<|notimestamps|>")?,
            no_speech: get("<|nospeech|>").or_else(|| get("<|nocaptions|>")),
            timestamp_begin: need("<|0.00|>")?,
            languages,
            start_of_prev: get("<|startofprev|>"),
            vocab: vocab_map,
            merges,
            byte_to_char,
            bytes,
            special,
            special_by_id,
        })
    }

    /// Token ids of `text` (byte-level BPE; no special tokens are produced). Unknown pieces are skipped.
    pub fn encode(&self, text: &str) -> Vec<u32> {
        let mut ids = Vec::new();
        for piece in pretokenize(text) {
            let mut parts: Vec<String> = piece.bytes().map(|b| self.byte_to_char[b as usize].to_string()).collect();
            loop {
                let best = (0..parts.len().saturating_sub(1)).filter_map(|i| self.merges.get(&(parts[i].clone(), parts[i + 1].clone())).map(|r| (*r, i))).min();
                let Some((_, i)) = best else { break };
                let merged = format!("{}{}", parts[i], parts[i + 1]);
                parts.splice(i..i + 2, [merged]);
            }
            ids.extend(parts.iter().filter_map(|p| self.vocab.get(p).copied()));
        }
        ids
    }

    pub fn multilingual(&self) -> bool {
        !self.languages.is_empty()
    }

    pub fn language_token(&self, code: &str) -> Option<u32> {
        self.languages.iter().find(|(c, _)| c == code).map(|x| x.1)
    }

    pub fn is_timestamp(&self, id: u32) -> bool {
        id >= self.timestamp_begin
    }

    /// Bytes of a regular token (empty for special tokens).
    pub fn token_bytes(&self, id: u32) -> &[u8] {
        if self.special_by_id.contains_key(&id) {
            return &[];
        }
        self.bytes.get(id as usize).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn decode(&self, ids: &[u32]) -> String {
        let b: Vec<u8> = ids.iter().flat_map(|&i| self.token_bytes(i).iter().copied()).collect();
        String::from_utf8_lossy(&b).into_owned()
    }

    /// Ids of all special tokens that must never be sampled as text (everything special except
    /// end-of-text and the timestamps).
    pub fn non_text_specials(&self) -> Vec<u32> {
        self.special.values().copied().filter(|&id| id != self.eot && id < self.timestamp_begin).collect()
    }
}

/// Group text tokens into words: a token whose text starts with a space starts a new word;
/// punctuation-only tokens join the word before them (opening quotes/brackets the word after).
/// Returns (word text, token index range).
pub fn group_words(tok: &Tokenizer, ids: &[u32]) -> Vec<(String, std::ops::Range<usize>)> {
    let mut groups: Vec<(Vec<u8>, std::ops::Range<usize>)> = Vec::new();
    let mut pending_open: Option<(Vec<u8>, usize)> = None;
    for (i, &id) in ids.iter().enumerate() {
        let b = tok.token_bytes(id);
        if b.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(b);
        let trimmed = text.trim();
        let starts_space = b[0] == b' ';
        let punct = !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_punctuation() || "“”‘’«»¿¡…。，！？：、".contains(c));
        let opening = punct && trimmed.chars().all(|c| "\"'“‘¿¡([{«-".contains(c)) && starts_space;
        if opening {
            let start = pending_open.as_ref().map(|p| p.1).unwrap_or(i);
            let mut acc = pending_open.take().map(|p| p.0).unwrap_or_default();
            acc.extend_from_slice(b);
            pending_open = Some((acc, start));
            continue;
        }
        if let Some((mut pre, start)) = pending_open.take() {
            pre.extend_from_slice(b);
            groups.push((pre, start..i + 1));
            continue;
        }
        match groups.last_mut() {
            Some(g) if !starts_space => {
                g.0.extend_from_slice(b);
                g.1.end = i + 1;
            }
            Some(g) if punct => {
                // " ," style punctuation with a space still belongs to the previous word
                g.0.extend_from_slice(b);
                g.1.end = i + 1;
            }
            _ => groups.push((b.to_vec(), i..i + 1)),
        }
    }
    if let Some((pre, start)) = pending_open {
        groups.push((pre, start..ids.len()));
    }
    groups.into_iter().map(|(b, r)| (String::from_utf8_lossy(&b).trim().to_string(), r)).filter(|(t, _)| !t.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretokenizer_follows_the_gpt2_pattern() {
        assert_eq!(pretokenize("Euh, alors"), ["Euh", ",", " alors"]);
        assert_eq!(pretokenize("l'heure"), ["l", "'", "heure"]);
        assert_eq!(pretokenize("it's 42 !"), ["it", "'s", " 42", " !"]);
        assert_eq!(pretokenize("a  b"), ["a", " ", " b"]);
        assert_eq!(pretokenize(" hum..."), [" hum", "..."]);
    }

    /// A tiny byte-level vocabulary: single bytes plus the merges `e u`, `eu h`.
    fn tiny() -> Tokenizer {
        let json = r#"{"model":{"vocab":{"Ġ":0,"e":1,"u":2,"h":3,"eu":4,"euh":5,"Ġe":6},"merges":["e u","eu h","Ġ e"]},
            "added_tokens":[{"id":7,"content":"<|endoftext|>"},{"id":8,"content":"<|startoftranscript|>"},{"id":9,"content":"<|transcribe|>"},
            {"id":10,"content":"<|notimestamps|>"},{"id":11,"content":"<|0.00|>"},{"id":12,"content":"<|startofprev|>"}]}"#;
        Tokenizer::from_json(json).unwrap()
    }

    #[test]
    fn bpe_merges_by_rank() {
        let t = tiny();
        assert_eq!(t.encode("euh"), vec![5]);
        assert_eq!(t.encode("eu"), vec![4]);
        assert_eq!(t.encode(" e"), vec![6]);
        assert_eq!(t.start_of_prev, Some(12));
        // encode → decode round trip on what the vocabulary covers
        assert_eq!(t.decode(&t.encode("euh")), "euh");
    }
}
