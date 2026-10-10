//! AI restoration in pure Rust on candle: blind denoising with **SCUNet** and 2x
//! super-resolution with **SPAN**, run over photos of any size in overlapping tiles.
//!
//! On macOS the networks run on the GPU through Metal; elsewhere on the CPU. The weights are not
//! part of LightCraft and nothing depends on them: see [`files`] and `docs/ai-restore.md`.
//!
//! Images here are interleaved RGB `f32`, 0..1, **gamma-encoded** (what both networks were
//! trained on). Converting from and to the working space, and blending the result with the
//! original by a strength, is the caller's business ([`blend`] does the latter).

#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod files;
#[allow(dead_code)]
pub mod pt;
pub mod scunet;
pub mod span;
pub mod tiles;

use std::path::Path;

use candle_core::{Device, Tensor};

pub use tiles::Tiling;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("model: {0}")]
    Model(String),
    #[error("image: {0}")]
    Image(String),
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Candle(#[from] candle_core::Error),
    #[error("{0}: {1}")]
    Io(String, std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Interleaved RGB, 0..1.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
}

/// The device the networks run on: Metal on macOS, else the CPU. `FILMCRAFT_ML_DEVICE=cpu`
/// forces the CPU.
pub fn pick_device() -> Device {
    let force_cpu = std::env::var("FILMCRAFT_ML_DEVICE").is_ok_and(|v| v.eq_ignore_ascii_case("cpu"));
    if force_cpu {
        return Device::Cpu;
    }
    #[cfg(target_os = "macos")]
    if let Ok(d) = Device::new_metal(0) {
        return d;
    }
    Device::Cpu
}

fn env_usize(name: &str) -> Option<usize> {
    std::env::var(name).ok()?.trim().parse().ok()
}

fn read(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| Error::Io(path.display().to_string(), e))
}

/// The blind denoiser.
pub struct Denoiser {
    net: scunet::ScUnet,
    pub tiling: Tiling,
}

impl Denoiser {
    /// Load `scunet_color_real_psnr.pth`.
    pub fn load(weights: &Path) -> Result<Self> {
        let dev = pick_device();
        let w = pt::read(&read(weights)?).map_err(|e| Error::Model(format!("{}: {e}", weights.display())))?;
        let net = scunet::ScUnet::load(&w, &dev)?;
        let tiling = Tiling { tile: env_usize("FILMCRAFT_ML_TILE").unwrap_or(512), overlap: env_usize("FILMCRAFT_ML_OVERLAP").unwrap_or(48), multiple: 64 };
        Ok(Self { net, tiling })
    }

    /// Denoise `img`; `progress(done, tiles)` returns `false` to cancel.
    pub fn denoise(&self, img: &Image, progress: &mut dyn FnMut(usize, usize) -> bool) -> Result<Image> {
        tiles::run(img, 1, self.tiling, self.net.device(), &|x: &Tensor| self.net.forward(x), progress)
    }

    pub fn device(&self) -> &Device {
        self.net.device()
    }
}

/// The 2x upscaler.
pub struct Upscaler {
    net: span::Span,
    pub tiling: Tiling,
}

impl Upscaler {
    /// Load `2xNomosUni_span_multijpg.safetensors`.
    pub fn load(weights: &Path) -> Result<Self> {
        Self::load_with(weights, span::Norm::REFERENCE)
    }

    pub fn load_with(weights: &Path, norm: span::Norm) -> Result<Self> {
        let dev = pick_device();
        let w = span::read_safetensors(&read(weights)?, &dev)?;
        let net = span::Span::load(&w, norm, &dev)?;
        let tiling = Tiling { tile: env_usize("FILMCRAFT_ML_TILE").unwrap_or(512), overlap: env_usize("FILMCRAFT_ML_OVERLAP").unwrap_or(24), multiple: 1 };
        Ok(Self { net, tiling })
    }

    pub fn scale(&self) -> usize {
        self.net.scale()
    }

    /// Upscale `img` by [`Upscaler::scale`]; the result is clamped to 0..1.
    pub fn upscale(&self, img: &Image, progress: &mut dyn FnMut(usize, usize) -> bool) -> Result<Image> {
        let mut out = tiles::run(img, self.net.scale(), self.tiling, self.net.device(), &|x: &Tensor| self.net.forward(x), progress)?;
        out.data.iter_mut().for_each(|v| *v = v.clamp(0.0, 1.0));
        Ok(out)
    }

    pub fn device(&self) -> &Device {
        self.net.device()
    }
}

/// `orig + strength * (restored - orig)` per sample, `strength` in 0..1, into `restored`.
/// Both images must have the same size.
pub fn blend(orig: &Image, restored: &mut Image, strength: f32) -> Result<()> {
    if orig.width != restored.width || orig.height != restored.height {
        return Err(Error::Image("blend of images of different sizes".into()));
    }
    let s = strength.clamp(0.0, 1.0);
    restored.data.iter_mut().zip(&orig.data).for_each(|(r, o)| *r = o + s * (*r - o));
    Ok(())
}
