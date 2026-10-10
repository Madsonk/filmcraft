//! A SentencePiece unigram model, read from a `.spm` file (a protobuf), for the Opus-MT tokenisers.
//!
//! Only what translation needs: the pieces and their scores, the dummy `▁` prefix, whitespace
//! collapsing, and the Viterbi segmentation that SentencePiece uses for unigram models. The
//! `nmt_nfkc` normaliser is approximated by the substitutions that matter in subtitles (no-break and
//! thin spaces, ellipsis, ligatures, full-width forms, control characters); rare compatibility
//! characters pass through unchanged and may come out as `<unk>`.

use std::collections::HashMap;

use crate::SpeechError;

/// The word-boundary mark SentencePiece puts in front of words.
pub const SPACE: char = '\u{2581}';

pub struct Spm {
    /// piece → score (normal and user-defined pieces).
    pieces: HashMap<String, f32>,
    max_chars: usize,
    unk_score: f32,
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    fn varint(&mut self) -> Option<u64> {
        let (mut r, mut s) = (0u64, 0u32);
        loop {
            let c = *self.b.get(self.i)?;
            self.i += 1;
            r |= u64::from(c & 0x7f) << s;
            s += 7;
            if c & 0x80 == 0 {
                return Some(r);
            }
            if s > 63 {
                return None;
            }
        }
    }
    /// Next field: (number, wire type, bytes of a length-delimited field / raw fixed bytes / varint as 8 LE bytes).
    fn field(&mut self) -> Option<(u64, u8, &'a [u8])> {
        if self.i >= self.b.len() {
            return None;
        }
        let k = self.varint()?;
        let (f, t) = (k >> 3, (k & 7) as u8);
        match t {
            0 => {
                let v = self.varint()?;
                // store in a leaked-free way: callers of varint fields use `field0`
                let _ = v;
                Some((f, t, &[]))
            }
            1 => {
                let s = self.b.get(self.i..self.i + 8)?;
                self.i += 8;
                Some((f, t, s))
            }
            2 => {
                let n = self.varint()? as usize;
                let s = self.b.get(self.i..self.i.checked_add(n)?)?;
                self.i += n;
                Some((f, t, s))
            }
            5 => {
                let s = self.b.get(self.i..self.i + 4)?;
                self.i += 4;
                Some((f, t, s))
            }
            _ => None,
        }
    }
}

impl Spm {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SpeechError> {
        let bad = || SpeechError::Model("sentencepiece model: unreadable".into());
        let mut r = Reader { b: bytes, i: 0 };
        let mut pieces = HashMap::new();
        let mut max_chars = 1;
        let mut min_score = 0f32;
        let mut model_type = 1u64;
        while r.i < bytes.len() {
            let start = r.i;
            let k = r.varint().ok_or_else(bad)?;
            let (f, t) = (k >> 3, (k & 7) as u8);
            r.i = start;
            let (_, _, data) = r.field().ok_or_else(bad)?;
            if f == 1 && t == 2 {
                let mut p = Reader { b: data, i: 0 };
                let (mut text, mut score, mut ty) = (String::new(), 0f32, 1u64);
                while p.i < data.len() {
                    let st = p.i;
                    let kk = p.varint().ok_or_else(bad)?;
                    let (ff, tt) = (kk >> 3, (kk & 7) as u8);
                    if tt == 0 {
                        let v = p.varint().ok_or_else(bad)?;
                        if ff == 3 {
                            ty = v;
                        }
                        continue;
                    }
                    p.i = st;
                    let (_, _, d) = p.field().ok_or_else(bad)?;
                    match ff {
                        1 => text = String::from_utf8_lossy(d).into_owned(),
                        2 if d.len() == 4 => score = f32::from_le_bytes([d[0], d[1], d[2], d[3]]),
                        _ => {}
                    }
                }
                // 1 normal, 4 user defined; control (3), unknown (2), unused (5) and bytes (6) are not segmentation pieces
                if ty == 1 || ty == 4 {
                    max_chars = max_chars.max(text.chars().count());
                    min_score = min_score.min(score);
                    pieces.insert(text, score);
                }
            } else if f == 2 && t == 2 {
                // trainer_spec: model_type is field 3
                let mut p = Reader { b: data, i: 0 };
                while p.i < data.len() {
                    let st = p.i;
                    let kk = p.varint().ok_or_else(bad)?;
                    let (ff, tt) = (kk >> 3, (kk & 7) as u8);
                    if tt == 0 {
                        let v = p.varint().ok_or_else(bad)?;
                        if ff == 3 {
                            model_type = v;
                        }
                        continue;
                    }
                    p.i = st;
                    p.field().ok_or_else(bad)?;
                }
            }
        }
        if pieces.is_empty() {
            return Err(bad());
        }
        if model_type != 1 {
            return Err(SpeechError::Model(format!("sentencepiece model type {model_type} is not supported (unigram only)")));
        }
        Ok(Spm { pieces, max_chars, unk_score: min_score - 10.0 })
    }

    /// Normalise and add the dummy prefix: `"  Bonjour  le monde "` → `"▁Bonjour▁le▁monde"`.
    pub fn normalize(text: &str) -> String {
        let mut s = String::with_capacity(text.len() + 4);
        let mut last_space = true;
        for c in text.chars() {
            let rep: &str = match c {
                ' ' | '\u{a0}' | '\u{2002}'..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\t' | '\n' | '\r' => " ",
                '\u{2026}' => "...",
                '\u{fb01}' => "fi",
                '\u{fb02}' => "fl",
                '\u{200b}' | '\u{feff}' | '\u{ad}' => "",
                c if c.is_control() => "",
                _ => {
                    s.push(c);
                    last_space = false;
                    continue;
                }
            };
            for r in rep.chars() {
                if r == ' ' {
                    if !last_space {
                        s.push(' ');
                    }
                    last_space = true;
                } else {
                    s.push(r);
                    last_space = false;
                }
            }
        }
        let s = s.trim_end();
        format!("{SPACE}{}", s.replace(' ', &SPACE.to_string()))
    }

    /// Viterbi segmentation; characters no piece covers come out as `<unk>`.
    pub fn encode(&self, text: &str) -> Vec<String> {
        let norm = Self::normalize(text);
        let chars: Vec<char> = norm.chars().collect();
        let n = chars.len();
        let mut best = vec![f32::NEG_INFINITY; n + 1];
        let mut from = vec![(0usize, false); n + 1];
        best[0] = 0.0;
        for i in 0..n {
            if !best[i].is_finite() {
                continue;
            }
            let mut any = false;
            let mut piece = String::new();
            for j in i..n.min(i + self.max_chars) {
                piece.push(chars[j]);
                if let Some(&sc) = self.pieces.get(&piece) {
                    any = true;
                    let v = best[i] + sc;
                    if v > best[j + 1] {
                        best[j + 1] = v;
                        from[j + 1] = (i, true);
                    }
                }
            }
            if !any || !self.pieces.contains_key(&chars[i].to_string()) {
                let v = best[i] + self.unk_score;
                if v > best[i + 1] {
                    best[i + 1] = v;
                    from[i + 1] = (i, false);
                }
            }
        }
        let mut out: Vec<String> = Vec::new();
        let mut e = n;
        while e > 0 {
            let (s, known) = from[e];
            out.push(if known { chars[s..e].iter().collect() } else { "<unk>".into() });
            e = s;
        }
        out.reverse();
        // consecutive unknown characters become one <unk>
        out.dedup_by(|a, b| a == "<unk>" && b == "<unk>");
        out
    }

    /// Join pieces back into text.
    pub fn decode(pieces: &[&str]) -> String {
        let s: String = pieces.concat();
        s.replace(SPACE, " ").trim().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_spaces() {
        assert_eq!(Spm::normalize("  Bonjour \u{a0} le   monde… "), "▁Bonjour▁le▁monde...");
    }

    #[test]
    fn viterbi_prefers_the_best_total_score() {
        let mut pieces = HashMap::new();
        for (p, s) in [("▁a", -1.0), ("▁", -5.0), ("a", -4.0), ("b", -1.0), ("ab", -1.5), ("▁ab", -1.2)] {
            pieces.insert(p.to_string(), s);
        }
        let spm = Spm { pieces, max_chars: 3, unk_score: -20.0 };
        assert_eq!(spm.encode("ab"), vec!["▁ab"]);
        assert_eq!(spm.encode("a b"), vec!["▁a", "▁", "b"]);
        assert_eq!(spm.encode("a z"), vec!["▁a", "▁", "<unk>"]);
        assert_eq!(Spm::decode(&["▁ab", "▁", "b"]), "ab b");
    }
}
