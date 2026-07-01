//! airlock — sandbox Claude Code (and other agents) in fast, disposable microVMs.
//!
//! This crate is a thin, testable orchestration layer over the `smolvm` microVM
//! runtime. See `docs/decisions/0001-architecture-and-trust-model.md` for the
//! design and threat model.

pub mod auth;
pub mod checkpoint;
pub mod cli;
pub mod config;
pub mod error;
pub mod fleet;
pub mod image;
pub mod names;
pub mod paths;
pub mod ports;
pub mod secrets;
pub mod smolvm;
pub mod ssh;
pub mod toolchains;

pub use error::{Error, Result};
pub use fleet::{Fleet, FleetIndex, Member};
pub use names::VmName;
pub use smolvm::Smolvm;
