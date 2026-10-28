//! # filmcraft-extensions
//!
//! The extension host of FilmCraft: small packages that add entries to the **Extensions** menu.
//! An extension is a folder with an `extension.json` manifest ([`manifest`]); each entry runs either
//!
//! * a list of FilmCraft commands, the same `{"id", "params"}` objects the headless CLI (`filmcraft-cli run`)
//!   and the MCP server take, with `${project}`, `${selection}` … variables ([`run`]), or
//! * an external program started with a JSON description of the editing context on its standard input,
//!   whose standard output may hand FilmCraft commands back, one JSON object per line.
//!
//! Extensions live in `<data dir>/extensions/<id>/` ([`store`]); they are installed from a folder or a
//! `.zip` / `.fcext` archive ([`pack`]). No part of this crate loads native code: there is no FFI and
//! no `unsafe`, an extension can only drive FilmCraft through its documented commands (or run a program
//! the user has put in the extension, exactly as if they had run it themselves).
//!
//! The crate knows nothing about the engine: the host (desktop app, CLI) passes a closure that executes
//! one command, so the same extension runs in the window and headless.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod manifest;
pub mod pack;
pub mod run;
pub mod store;

pub use manifest::{Action, ActionKind, Entry, ExecSpec, Manifest, Step};
pub use run::{Host, RunContext, RunReport, StepReport, run_entry};
pub use store::{Extension, ExtensionStore, InstallReport, Listed, Offer};

/// What went wrong, in words a user can act on.
#[derive(Debug, thiserror::Error)]
pub enum ExtError {
    /// `extension.json` is missing, unreadable or invalid.
    #[error("{0}")]
    Manifest(String),
    /// A package (folder or archive) can't be installed.
    #[error("{0}")]
    Package(String),
    #[error("extension `{0}` is not installed")]
    NotInstalled(String),
    /// A file-system error, with what was being done.
    #[error("{0}")]
    Io(String),
    /// An entry can't be run (bad variable, program missing, command failed…).
    #[error("{0}")]
    Run(String),
}

impl ExtError {
    pub(crate) fn io(what: impl std::fmt::Display, e: std::io::Error) -> Self {
        ExtError::Io(format!("{what}: {e}"))
    }
}

pub type Result<T> = std::result::Result<T, ExtError>;
