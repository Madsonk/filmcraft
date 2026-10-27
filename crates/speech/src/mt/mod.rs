//! Offline machine translation of subtitles (feature `translate`): the Opus-MT Marian models of
//! [`crate::models::mt_catalogue`], run on the CPU with candle.
//!
//! * [`spm`]: SentencePiece unigram tokenisation (read from the model's `.spm` files).
//! * [`marian`]: the network and beam search.
//! * [`Translator`]: text in, text out. Models are downloaded on request (see `models`), never bundled.

#[cfg(feature = "translate")]
pub mod marian;
pub mod spm;

#[cfg(feature = "translate")]
mod translator {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Mutex;

    use super::marian::{Config, Marian};
    use super::spm::Spm;
    use crate::SpeechError;

    fn merr(e: candle_core::Error) -> SpeechError {
        SpeechError::Model(e.to_string())
    }

    pub struct Translator {
        model: Mutex<Marian>,
        src: Spm,
        ids: HashMap<String, u32>,
        pieces: Vec<String>,
        unk: u32,
        eos: u32,
    }

    impl Translator {
        /// Load a model directory (`config.json`, `vocab.json`, `source.spm`, `target.spm`, `model.safetensors`).
        pub fn load(dir: &Path) -> Result<Self, SpeechError> {
            let read = |n: &str| std::fs::read(dir.join(n)).map_err(|e| SpeechError::Model(format!("{}: {e}", dir.join(n).display())));
            let cfg: Config = serde_json::from_slice(&read("config.json")?).map_err(|e| SpeechError::Model(format!("config.json: {e}")))?;
            let vocab: HashMap<String, u32> = serde_json::from_slice(&read("vocab.json")?).map_err(|e| SpeechError::Model(format!("vocab.json: {e}")))?;
            let mut pieces = vec![String::new(); cfg.vocab_size];
            for (p, i) in &vocab {
                if let Some(slot) = pieces.get_mut(*i as usize) {
                    *slot = p.clone();
                }
            }
            let unk = vocab.get("<unk>").copied().unwrap_or(0);
            let eos = cfg.eos_token_id;
            let src = Spm::from_bytes(&read("source.spm")?)?;
            let model = Marian::load(cfg, read("model.safetensors")?).map_err(merr)?;
            Ok(Self { model: Mutex::new(model), src, ids: vocab, pieces, unk, eos })
        }

        /// Translate one sentence (or a short passage).
        pub fn translate(&self, text: &str, beams: usize) -> Result<String, SpeechError> {
            let text = text.trim();
            if text.is_empty() {
                return Ok(String::new());
            }
            let mut ids: Vec<u32> = self.src.encode(text).iter().map(|p| self.ids.get(p).copied().unwrap_or(self.unk)).collect();
            ids.truncate(400);
            ids.push(self.eos);
            let max_len = (ids.len() * 2 + 10).min(500);
            let m = self.model.lock().unwrap_or_else(|e| e.into_inner());
            let out = m.translate(&ids, beams, max_len).map_err(merr)?;
            let words: Vec<&str> = out.iter().filter_map(|&i| self.pieces.get(i as usize)).map(String::as_str).collect();
            Ok(Spm::decode(&words))
        }
    }
}

#[cfg(feature = "translate")]
pub use translator::Translator;
