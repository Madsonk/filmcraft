//! Hesitations ("euh", "um"): the prompt that makes Whisper write them down, and a detector of the
//! filled pauses it still leaves out.

/// The built-in disfluency prompt for a language code (`fr`, `en`, …; unknown → English).
pub fn default_prompt(language: Option<&str>) -> &'static str {
    match language.unwrap_or("en") {
        l if l.starts_with("fr") => "Euh, alors, heu, je pense que, hum, ben, c'est, euh, bah, voilà.",
        _ => "Um, so, uh, I think, you know, hmm, it's, uh, like, well.",
    }
}

/// Hesitation spellings (lower case) Whisper may write; those that are one token in the tokenizer
/// can be favoured while decoding.
pub const SPELLINGS: &[&str] =
    &["euh", "heu", "euhh", "hum", "hmm", "mmh", "mm", "um", "uh", "umm", "uhm", "erm", "er", "hem", "beuh", "bah", "ben", "hmmm", "mhm"];

/// Logit bias on single-token hesitation spellings in hesitation mode (see `Options::hesitations`).
pub const DEFAULT_BIAS: f32 = 2.0;
