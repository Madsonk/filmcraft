//! Semantic photo search in pure Rust on candle: SigLIP embeds photos and sentences into the same
//! 768-dimensional space, so "a child playing in a pool" finds photos by what they show.
//!
//! - [`Embedder`]: the SigLIP image and text towers (Metal on macOS, else the CPU), with their
//!   pure-Rust [`tokenizer`] and Pillow-compatible [`preprocess`]ing.
//! - [`VectorIndex`]: the vectors (16-bit floats, an append-only file), brute-force cosine search.
//! - [`hash`]: perceptual hashes for near-duplicates.
//!
//! The weights are not part of LightCraft: they are Google's Apache-2.0 SigLIP checkpoints, read
//! from a folder the user downloads them to ([`fetch`]), only when they ask for it.
//!
//! Ported from the Hugging Face `transformers` implementation (Apache-2.0); see `NOTICE`. This
//! crate is therefore licensed under the Apache License 2.0 only (not the MIT option of the rest
//! of LightCraft). No UI dependencies (L3).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(not(target_arch = "wasm32"))]
pub mod fetch;
#[cfg(not(target_arch = "wasm32"))]
pub mod hash;
#[cfg(not(target_arch = "wasm32"))]
pub mod index;
#[cfg(not(target_arch = "wasm32"))]
mod model;
#[cfg(not(target_arch = "wasm32"))]
pub mod preprocess;
#[cfg(not(target_arch = "wasm32"))]
mod records;
#[cfg(not(target_arch = "wasm32"))]
pub mod tokenizer;

#[cfg(not(target_arch = "wasm32"))]
pub use index::{HashStore, VectorIndex, key_of};
#[cfg(not(target_arch = "wasm32"))]
pub use model::{Embedder, IMAGE, Resample, best_device, is_model_dir, model_id};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("search model: {0}")]
    Model(String),
    #[error("search model files not found in {0} (expected model.safetensors, tokenizer.json, preprocessor_config.json)")]
    Missing(std::path::PathBuf),
    #[cfg(not(target_arch = "wasm32"))]
    #[error(transparent)]
    Candle(#[from] candle_core::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
