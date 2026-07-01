//! The crate-wide error type.
//!
//! Library code returns [`Error`]; the `main` binary wraps these in `anyhow` at
//! the boundary. Large upstream error sources are boxed so `Error` stays small
//! enough to pass by value in a `Result` without tripping `clippy::result_large_err`.

use std::path::PathBuf;
use thiserror::Error;

/// A `Result` alias using the crate [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong inside airlock's library surface.
#[derive(Debug, Error)]
pub enum Error {
    /// The platform did not expose a home directory, so no config/state paths exist.
    #[error("could not determine a home directory for airlock state")]
    NoHomeDir,

    /// No `airlock.toml` was found searching upward from the start directory.
    #[error("no airlock.toml found searching upward from {searched_from}; run `airlock init`")]
    ConfigNotFound {
        /// The directory the upward search began from.
        searched_from: PathBuf,
    },

    /// The config file existed but could not be read.
    #[error("failed to read config at {path}")]
    ConfigRead {
        /// Path that failed to read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The config file was not valid TOML / did not match the schema.
    #[error("invalid config at {path}: {source}")]
    ConfigParse {
        /// Path that failed to parse.
        path: PathBuf,
        /// Underlying TOML error (boxed: it is comparatively large).
        #[source]
        source: Box<toml::de::Error>,
    },

    /// A semantic problem with an otherwise well-formed config.
    #[error("invalid config: {reason}")]
    ConfigValidate {
        /// Human-readable reason.
        reason: String,
    },

    /// A VM name did not satisfy smolvm machine-name rules.
    #[error("invalid VM name {name:?}: {reason}")]
    InvalidVmName {
        /// The offending name.
        name: String,
        /// Why it was rejected.
        reason: String,
    },

    /// No free host TCP port could be found in the probed range.
    #[error("no free host port found starting at {start} (searched through {end})")]
    NoFreePort {
        /// First port tried.
        start: u16,
        /// Last port tried.
        end: u16,
    },

    /// A required host tool was not found on `PATH`.
    #[error("required host tool {tool:?} not found on PATH")]
    ToolNotFound {
        /// The tool that is missing (e.g. `smolvm`, `docker`, `ssh`).
        tool: String,
    },

    /// A `smolvm` invocation exited non-zero.
    #[error("smolvm {args} failed (exit {code}): {stderr}")]
    Smolvm {
        /// The smolvm subcommand/args that failed (no secret values).
        args: String,
        /// Exit code.
        code: i32,
        /// Captured stderr (trimmed).
        stderr: String,
    },

    /// A `docker`/`podman` invocation exited non-zero.
    #[error("{engine} {args} failed (exit {code}): {stderr}")]
    Docker {
        /// Container engine used.
        engine: String,
        /// The subcommand/args that failed.
        args: String,
        /// Exit code.
        code: i32,
        /// Captured stderr (trimmed).
        stderr: String,
    },

    /// A generic external command exited non-zero.
    #[error("command `{cmd}` failed (exit {code})")]
    CommandFailed {
        /// The command (program + notable args, no secrets).
        cmd: String,
        /// Exit code.
        code: i32,
    },

    /// A command could not be spawned at all (e.g. binary missing, permission).
    #[error("failed to run `{cmd}`")]
    CommandSpawn {
        /// The command that could not be spawned.
        cmd: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A `.env` file could not be parsed.
    #[error("failed to parse env file {path}: {source}")]
    EnvFile {
        /// Path to the offending file.
        path: PathBuf,
        /// Underlying dotenvy error (boxed).
        #[source]
        source: Box<dotenvy::Error>,
    },

    /// A referenced VM is not part of the fleet.
    #[error("VM {name:?} is not part of fleet {profile:?}")]
    VmNotFound {
        /// Requested VM name.
        name: String,
        /// Profile searched.
        profile: String,
    },

    /// Transparent passthrough for I/O errors with no better context.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Transparent passthrough for JSON (de)serialization errors.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
