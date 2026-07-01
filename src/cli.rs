//! Command-line interface definition (clap derive).
//!
//! Kept in the library so the parser can be unit-tested; `main` parses a [`Cli`]
//! and dispatches. Interactive passthrough commands capture trailing args after
//! `--`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// Sandbox Claude Code (and other agents) in fast, disposable microVMs.
#[derive(Debug, Parser)]
#[command(name = "airlock", version, about, long_about = None)]
pub struct Cli {
    /// Increase log verbosity (also honours `RUST_LOG`).
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Scaffold an `airlock.toml` in the current directory.
    Init(InitArgs),
    /// Check host prerequisites (KVM, smolvm, docker, ssh, gh).
    Status,
    /// Log in to a service so its credentials can be injected at launch.
    Login(LoginArgs),
    /// Build (or rebuild) the base image for this profile.
    Build,
    /// Create and start VMs from the profile.
    Up(UpArgs),
    /// List the fleet.
    Ls(LsArgs),
    /// Start stopped VMs.
    Start(SelectorArgs),
    /// Stop running VMs.
    Stop(SelectorArgs),
    /// Restart VMs.
    Restart(SelectorArgs),
    /// Delete VMs and remove them from the fleet.
    Rm(SelectorArgs),
    /// Monitor a VM (health checks + restart) in the foreground.
    Monitor(TargetArgs),
    /// Launch Claude Code inside a VM.
    Claude(PassthroughArgs),
    /// Open an interactive shell inside a VM (via smolvm exec).
    Shell(TargetArgs),
    /// Run a command inside a VM (via smolvm exec).
    Exec(PassthroughArgs),
    /// SSH into a VM (real sshd endpoint).
    Ssh(PassthroughArgs),
    /// Copy files between host and a VM (`SELECTOR:PATH` form).
    Cp(CpArgs),
    /// Checkpoint a VM to a portable `.smolmachine` file.
    Checkpoint(CheckpointArgs),
    /// Restore a VM from a `.smolmachine` file.
    Restore(RestoreArgs),
}

/// `airlock init`
#[derive(Debug, Args)]
pub struct InitArgs {
    /// Profile name (defaults to the directory name).
    #[arg(long)]
    pub name: Option<String>,
    /// Overwrite an existing `airlock.toml`.
    #[arg(long)]
    pub force: bool,
}

/// Services `airlock login` understands.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum LoginService {
    /// GitHub, via `gh auth login`.
    Github,
}

/// `airlock login`
#[derive(Debug, Args)]
pub struct LoginArgs {
    /// Service to log into.
    #[arg(value_enum, default_value_t = LoginService::Github)]
    pub service: LoginService,
}

/// `airlock up`
#[derive(Debug, Args)]
pub struct UpArgs {
    /// How many VMs to create.
    #[arg(short = 'n', long, default_value_t = 1)]
    pub count: usize,
    /// Rebuild the base image before creating VMs.
    #[arg(long)]
    pub rebuild: bool,
}

/// `airlock ls`
#[derive(Debug, Args)]
pub struct LsArgs {
    /// Emit JSON instead of a table.
    #[arg(long)]
    pub json: bool,
}

/// Shared target-selection args for lifecycle commands.
#[derive(Debug, Args)]
pub struct SelectorArgs {
    /// VM names or ordinals.
    pub targets: Vec<String>,
    /// Apply to every fleet member.
    #[arg(long)]
    pub all: bool,
}

/// A single required target.
#[derive(Debug, Args)]
pub struct TargetArgs {
    /// VM name or ordinal.
    pub target: String,
}

/// A target plus trailing command/args (after `--`).
#[derive(Debug, Args)]
pub struct PassthroughArgs {
    /// VM name or ordinal.
    pub target: String,
    /// Command and arguments to pass through.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub rest: Vec<String>,
}

/// `airlock cp`
#[derive(Debug, Args)]
pub struct CpArgs {
    /// Source (`SELECTOR:PATH` or a local path).
    pub src: String,
    /// Destination (`SELECTOR:PATH` or a local path).
    pub dst: String,
}

/// `airlock checkpoint`
#[derive(Debug, Args)]
pub struct CheckpointArgs {
    /// VM name or ordinal.
    pub target: String,
    /// Output `.smolmachine` path (defaults under the profile data dir).
    #[arg(short, long)]
    pub output: Option<PathBuf>,
}

/// `airlock restore`
#[derive(Debug, Args)]
pub struct RestoreArgs {
    /// Path to a `.smolmachine` file.
    pub pack: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_up_with_count() {
        let cli = Cli::try_parse_from(["airlock", "up", "-n", "3"]).expect("parses");
        match cli.command {
            Command::Up(a) => assert_eq!(a.count, 3),
            other => panic!("wrong command: {other:?}"),
        }
    }

    #[test]
    fn exec_captures_trailing_command() {
        let cli = Cli::try_parse_from(["airlock", "exec", "web-01", "ls", "-la"]).expect("parses");
        match cli.command {
            Command::Exec(a) => {
                assert_eq!(a.target, "web-01");
                assert_eq!(a.rest, vec!["ls", "-la"]);
            }
            other => panic!("wrong command: {other:?}"),
        }
    }

    #[test]
    fn claude_allows_no_extra_args() {
        let cli = Cli::try_parse_from(["airlock", "claude", "1"]).expect("parses");
        match cli.command {
            Command::Claude(a) => {
                assert_eq!(a.target, "1");
                assert!(a.rest.is_empty());
            }
            other => panic!("wrong command: {other:?}"),
        }
    }

    #[test]
    fn stop_all_flag() {
        let cli = Cli::try_parse_from(["airlock", "stop", "--all"]).expect("parses");
        match cli.command {
            Command::Stop(a) => assert!(a.all),
            other => panic!("wrong command: {other:?}"),
        }
    }
}
