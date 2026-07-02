//! airlock CLI entry point.
//!
//! Thin dispatch over the `airlock` library. Library errors (`thiserror`) are
//! wrapped with `anyhow` context here at the boundary.

use std::path::Path;

use anyhow::{bail, Context, Result};
use clap::Parser;

use airlock::checkpoint;
use airlock::cli::{
    CheckpointArgs, Cli, Command, CpArgs, InitArgs, LoginArgs, LoginService, PassthroughArgs,
    RestoreArgs, SelectorArgs, TargetArgs, UpArgs,
};
use airlock::config;
use airlock::fleet::{Fleet, UpOptions};
use airlock::image;
use airlock::smolvm::Smolvm;

fn main() {
    let cli = Cli::parse();
    init_tracing(cli.verbose);
    match run(cli) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
    }
}

fn init_tracing(verbose: bool) {
    use tracing_subscriber::{fmt, EnvFilter};
    let fallback = if verbose {
        "airlock=debug"
    } else {
        "airlock=info"
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(fallback));
    fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .with_writer(std::io::stderr)
        .init();
}

fn run(cli: Cli) -> Result<i32> {
    match cli.command {
        Command::Init(args) => cmd_init(args),
        Command::Status => cmd_status(),
        Command::Login(args) => cmd_login(args),
        Command::Build => cmd_build(),
        Command::Up(args) => cmd_up(args),
        Command::Ls(args) => cmd_ls(args.json),
        Command::Start(args) => cmd_lifecycle(args, Lifecycle::Start),
        Command::Stop(args) => cmd_lifecycle(args, Lifecycle::Stop),
        Command::Restart(args) => cmd_lifecycle(args, Lifecycle::Restart),
        Command::Rm(args) => cmd_rm(args),
        Command::Monitor(args) => cmd_monitor(args),
        Command::Claude(args) => cmd_claude(args),
        Command::Shell(args) => cmd_shell(args),
        Command::Exec(args) => cmd_exec(args),
        Command::Ssh(args) => cmd_ssh(args),
        Command::Cp(args) => cmd_cp(args),
        Command::Checkpoint(args) => cmd_checkpoint(args),
        Command::Restore(args) => cmd_restore(args),
    }
}

fn open_fleet() -> Result<Fleet> {
    let smolvm = Smolvm::discover()
        .context("smolvm not found on PATH — install it from github.com/smol-machines/smolvm")?;
    Fleet::open(smolvm).context("no airlock.toml found — run `airlock init` in your project")
}

fn cmd_init(args: InitArgs) -> Result<i32> {
    let cwd = std::env::current_dir()?;
    let path = cwd.join(config::CONFIG_FILENAME);
    if path.exists() && !args.force {
        bail!(
            "{} already exists (use --force to overwrite)",
            path.display()
        );
    }
    let name = args.name.unwrap_or_else(|| {
        config::sanitize_profile(
            &cwd.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "airlock".to_owned()),
        )
    });
    std::fs::write(&path, config::scaffold_toml(&name))
        .with_context(|| format!("writing {}", path.display()))?;
    println!("Wrote {}", path.display());
    println!("Next: `airlock build` then `airlock up`.");
    Ok(0)
}

fn cmd_status() -> Result<i32> {
    println!("airlock host check:");
    let kvm = Path::new("/dev/kvm").exists();
    report(
        "/dev/kvm present",
        kvm,
        "confidential/virtualised host without nested KVM?",
    );
    if kvm {
        let writable = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/kvm")
            .is_ok();
        report(
            "/dev/kvm accessible",
            writable,
            "add your user to the `kvm` group: sudo usermod -aG kvm $USER",
        );
    }
    report(
        "smolvm on PATH",
        which_ok("smolvm"),
        "install from github.com/smol-machines/smolvm",
    );
    let engine = which_ok("docker") || which_ok("podman");
    report("docker or podman", engine, "needed to build the base image");
    report("ssh client", which_ok("ssh"), "needed for `airlock ssh`");
    report(
        "ssh-keygen",
        which_ok("ssh-keygen"),
        "needed to generate the per-profile key",
    );
    report(
        "gh (GitHub CLI)",
        which_ok("gh"),
        "optional: `airlock login github` and GH_TOKEN injection",
    );

    let ok = kvm && which_ok("smolvm") && engine;
    if ok {
        println!("\nReady. Try: airlock init && airlock up");
    } else {
        println!("\nSome prerequisites are missing (see hints above).");
    }
    Ok(0)
}

fn cmd_login(args: LoginArgs) -> Result<i32> {
    match args.service {
        LoginService::Github => {
            airlock::auth::login_github().context("gh auth login failed")?;
            println!("GitHub login complete; GH_TOKEN will be injected into VMs at launch.");
        }
    }
    Ok(0)
}

fn cmd_build() -> Result<i32> {
    let engine = image::detect_engine().context("no container engine (docker/podman) found")?;
    tracing::info!(%engine, "building base image (this can take several minutes the first time)");
    let fleet = open_fleet()?;
    let archive = fleet.ensure_image(true)?;
    println!("Built base image → {}", archive.display());
    Ok(0)
}

fn cmd_up(args: UpArgs) -> Result<i32> {
    if args.count == 0 {
        bail!("--count must be at least 1");
    }
    let fleet = open_fleet()?;
    let opts = UpOptions {
        count: args.count,
        rebuild: args.rebuild,
        binds: args.bind,
        copies: args.copy,
        project: if args.no_project { Some(false) } else { None },
    };
    let members = fleet.up(&opts)?;
    println!(
        "Brought up {} VM(s) in profile '{}':",
        members.len(),
        fleet.profile()
    );
    for m in &members {
        println!("  {}  (ssh port {})", m.name, m.ssh_port);
    }
    if let Some(first) = members.first() {
        println!("\nConnect with:  airlock claude {}", first.index);
        println!("Or a shell:    airlock shell {}", first.index);
    }
    Ok(0)
}

fn cmd_ls(json: bool) -> Result<i32> {
    let fleet = open_fleet()?;
    let rows = fleet.list()?;

    if json {
        let items: Vec<_> = rows
            .iter()
            .map(|(m, info)| {
                serde_json::json!({
                    "name": m.name.as_str(),
                    "index": m.index,
                    "state": info.as_ref().map_or("absent", |i| i.state.as_str()),
                    "ssh_port": m.ssh_port,
                    "pid": info.as_ref().and_then(|i| i.pid),
                    "image_tag": m.image_tag,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(0);
    }

    if rows.is_empty() {
        println!("No VMs in profile '{}'. Run `airlock up`.", fleet.profile());
        return Ok(0);
    }
    println!("{:<16} {:<10} {:>8}  IMAGE", "NAME", "STATE", "SSH");
    for (m, info) in &rows {
        let state = info.as_ref().map_or("absent", |i| i.state.as_str());
        let ssh = if m.ssh_port == 0 {
            "-".to_owned()
        } else {
            m.ssh_port.to_string()
        };
        println!(
            "{:<16} {:<10} {:>8}  {}",
            m.name.as_str(),
            state,
            ssh,
            m.image_tag
        );
    }
    Ok(0)
}

enum Lifecycle {
    Start,
    Stop,
    Restart,
}

fn cmd_lifecycle(args: SelectorArgs, action: Lifecycle) -> Result<i32> {
    let fleet = open_fleet()?;
    let members = fleet.resolve_targets(&args.targets, args.all)?;
    if members.is_empty() {
        bail!("specify one or more VM names/ordinals, or --all");
    }
    match action {
        Lifecycle::Start => fleet.start_members(&members)?,
        Lifecycle::Stop => fleet.stop_members(&members)?,
        Lifecycle::Restart => fleet.restart_members(&members)?,
    }
    let verb = match action {
        Lifecycle::Start => "started",
        Lifecycle::Stop => "stopped",
        Lifecycle::Restart => "restarted",
    };
    println!("{verb} {} VM(s)", members.len());
    Ok(0)
}

fn cmd_rm(args: SelectorArgs) -> Result<i32> {
    let fleet = open_fleet()?;
    let members = fleet.resolve_targets(&args.targets, args.all)?;
    if members.is_empty() {
        bail!("specify one or more VM names/ordinals, or --all");
    }
    fleet.remove_members(&members, true)?;
    println!("removed {} VM(s)", members.len());
    Ok(0)
}

fn cmd_monitor(args: TargetArgs) -> Result<i32> {
    let fleet = open_fleet()?;
    let status = fleet.monitor(&args.target)?;
    Ok(status.code().unwrap_or(0))
}

fn cmd_claude(args: PassthroughArgs) -> Result<i32> {
    let fleet = open_fleet()?;
    let mut command = vec!["claude".to_owned()];
    command.extend(args.rest);
    let status = fleet.exec_session(&args.target, command, true)?;
    Ok(status.code().unwrap_or(0))
}

fn cmd_shell(args: TargetArgs) -> Result<i32> {
    let fleet = open_fleet()?;
    // Empty command → `airlock-login` opens the dev user's login shell (fish).
    let status = fleet.exec_session(&args.target, Vec::new(), true)?;
    Ok(status.code().unwrap_or(0))
}

fn cmd_exec(args: PassthroughArgs) -> Result<i32> {
    if args.rest.is_empty() {
        bail!(
            "provide a command, e.g. `airlock exec {} -- ls -la`",
            args.target
        );
    }
    let fleet = open_fleet()?;
    // Non-interactive by default so output can be piped/captured; use `shell` for a PTY.
    let status = fleet.exec_session(&args.target, args.rest, false)?;
    Ok(status.code().unwrap_or(0))
}

fn cmd_ssh(args: PassthroughArgs) -> Result<i32> {
    let fleet = open_fleet()?;
    let status = fleet.ssh_session(&args.target, &args.rest)?;
    Ok(status.code().unwrap_or(0))
}

fn cmd_cp(args: CpArgs) -> Result<i32> {
    let fleet = open_fleet()?;
    fleet.cp(&args.src, &args.dst)?;
    println!("copied {} → {}", args.src, args.dst);
    Ok(0)
}

fn cmd_checkpoint(args: CheckpointArgs) -> Result<i32> {
    let fleet = open_fleet()?;
    let out = checkpoint::checkpoint(&fleet, &args.target, args.output)?;
    println!("checkpoint → {}", out.display());
    println!("(the VM was stopped to snapshot it; `airlock start` to resume)");
    Ok(0)
}

fn cmd_restore(args: RestoreArgs) -> Result<i32> {
    let fleet = open_fleet()?;
    let member = checkpoint::restore(&fleet, &args.pack)?;
    println!("restored as {} (ordinal {})", member.name, member.index);
    Ok(0)
}

fn which_ok(tool: &str) -> bool {
    which::which(tool).is_ok()
}

fn report(label: &str, ok: bool, hint: &str) {
    if ok {
        println!("  [ok]   {label}");
    } else {
        println!("  [MISS] {label}  — {hint}");
    }
}
