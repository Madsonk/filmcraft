//! Optional neural tools for FilmCraft, in pure Rust on candle (Metal on macOS, else the CPU).
//!
//! | module | tool | weights (never bundled, downloaded on request with a licence dialog) |
//! |---|---|---|
//! | [`rife`] | RIFE v4 frame interpolation (midpoint frame) | `TensorForger/RIFE-safetensors` (MIT) |
//!
//! Images are interleaved RGB `f32`, 0..1, gamma-encoded ([`Image`], shared with `filmcraft-mlrestore`).
#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod beat_this;
pub mod catalogue;
pub mod htdemucs;
pub mod rife;
pub mod rvm;

pub use filmcraft_mlrestore::{Image, pick_device};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("model: {0}")]
    Model(String),
    #[error("input: {0}")]
    Input(String),
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Candle(#[from] candle_core::Error),
    #[error("{0}: {1}")]
    Io(String, std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Read a whole file, naming it in the error.
pub fn read_file(path: &std::path::Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| Error::Io(path.display().to_string(), e))
}
