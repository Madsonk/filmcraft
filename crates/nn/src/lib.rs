//! A small ONNX interpreter in pure Rust (layer L1, dependency-free, `wasm32` clean).
//!
//! FilmCraft runs a few neural models (voice clean-up, subject detection for auto-reframe). Their
//! weights are downloaded on demand (crate `filmcraft-modelstore` holds the catalogue), and there is no ONNX runtime in the dependency tree, so this crate reads the `.onnx`
//! file (a protobuf, parsed here) and executes its graph on the CPU.
//!
//! * [`Model::parse`] reads a model; [`Model::run`] feeds named input [`Tensor`]s and returns the
//!   requested outputs. Tensors are dense, row-major, `f32` or `i64` (shapes, indices and pads are
//!   `i64` tensors in an ONNX graph).
//! * Operators: `Add Cast Concat Constant ConstantOfShape Conv ConvTranspose Einsum Gather GRU
//!   MatMul MaxPool Mul Pad Relu Reshape Resize Shape Sigmoid Slice Squeeze Sub Div Tanh
//!   Transpose Unsqueeze Exp Neg Sqrt Identity` (2-D convolutions, forward GRU layers, `nearest`
//!   resize, two-operand `Einsum`). An unsupported operator, attribute or dtype is an error
//!   naming it, never a panic.
//! * Every number read from the file is hostile: shapes are checked with capped products, indices
//!   with `get`, and a model can't ask for more than [`MAX_ELEMENTS`] elements in one tensor.
//!
//! This is not a general runtime: the operator set is the one the models FilmCraft ships need
//! (checked against `onnxruntime` on the real models, see `docs/extensions.md`). Convolutions use
//! `im2col` + a blocked matrix product and split their rows over threads on native targets.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod conv;
mod exec;
mod onnx;
mod ops;
mod par;
mod proto;
mod rnn;
mod tensor;
#[cfg(test)]
mod tests;

pub use exec::Model;
pub use tensor::{Data, Tensor};

/// Largest tensor (in elements) a model may create: 2^29 (2 GiB of `f32`).
pub const MAX_ELEMENTS: usize = 1 << 29;

/// Why a model could not be read or run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NnError(pub String);

impl std::fmt::Display for NnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NnError {}

pub type Result<T> = std::result::Result<T, NnError>;

pub(crate) fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(NnError(msg.into()))
}
