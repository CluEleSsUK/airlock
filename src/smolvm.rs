//! Typed wrapper around the `smolvm` binary.
//!
//! The argv builders ([`create_args`], [`exec_args`], …) are pure functions over
//! typed specs, so they are unit-tested without touching KVM. The [`Smolvm`]
//! executor turns a spec into a `std::process::Command`, injecting secret values
//! through the child's *environment block* (never argv — argv carries only the
//! `--secret-env NAME=NAME` reference, which is safe to log).

use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::names::VmName;
use crate::secrets::SecretEnv;

/// The smolvm executable name expected on `PATH`.
pub const BIN: &str = "smolvm";

/// Where the root filesystem for a machine comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageSource {
    /// An OCI registry reference, e.g. `ubuntu:24.04` or `airlock/web:latest`.
    Registry(String),
    /// A local `docker save` archive fed via `--image ./path.tar`.
    LocalArchive(PathBuf),
    /// A packed `.smolmachine` artifact fed via `--from ./path.smolmachine`.
    Packed(PathBuf),
    /// No image (bare Alpine agent VM).
    None,
}

impl ImageSource {
    fn args(&self) -> Vec<String> {
        match self {
            Self::Registry(r) => vec!["--image".into(), r.clone()],
            Self::LocalArchive(p) => vec!["--image".into(), p.display().to_string()],
            Self::Packed(p) => vec!["--from".into(), p.display().to_string()],
            Self::None => Vec::new(),
        }
    }
}

/// Guest egress policy, mapped onto smolvm's `--net` / `--allow-*` flags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetSpec {
    /// No outbound networking (smolvm default).
    Off,
    /// Unrestricted outbound (`--net`).
    All,
    /// Egress only to the listed hosts/CIDRs (each implies `--net`).
    Allow {
        /// Allowed hostnames.
        hosts: Vec<String>,
        /// Allowed CIDR ranges.
        cidrs: Vec<String>,
    },
}

impl NetSpec {
    fn args(&self) -> Vec<String> {
        match self {
            Self::Off => Vec::new(),
            Self::All => vec!["--net".into()],
            Self::Allow { hosts, cidrs } => {
                let mut a = Vec::new();
                for h in hosts {
                    a.push("--allow-host".into());
                    a.push(h.clone());
                }
                for c in cidrs {
                    a.push("--allow-cidr".into());
                    a.push(c.clone());
                }
                a
            }
        }
    }
}

/// Parameters for `smolvm machine create`.
#[derive(Clone, Debug)]
pub struct CreateSpec {
    /// Machine name.
    pub name: VmName,
    /// Root filesystem source.
    pub image: ImageSource,
    /// vCPUs (None → smolvm default).
    pub cpus: Option<u32>,
    /// Memory MiB (None → smolvm default).
    pub mem: Option<u32>,
    /// Egress policy.
    pub net: NetSpec,
    /// `HOST:GUEST[:ro]` volume mounts.
    pub volumes: Vec<String>,
    /// `HOST:GUEST` port forwards.
    pub ports: Vec<String>,
    /// Forward the host SSH agent into the VM.
    pub ssh_agent: bool,
    /// Optional persistent workload command (runs as a detached container each start).
    pub workload: Option<Vec<String>>,
}

/// Parameters for `smolvm machine exec`.
#[derive(Clone, Debug)]
pub struct ExecSpec {
    /// Target machine.
    pub name: VmName,
    /// Command + args to run in the guest.
    pub command: Vec<String>,
    /// Keep stdin open (`-i`).
    pub interactive: bool,
    /// Allocate a PTY (`-t`).
    pub tty: bool,
    /// Working directory in the guest.
    pub workdir: Option<String>,
    /// Non-secret environment variables (`-e`).
    pub env: Vec<(String, String)>,
    /// Secret env vars: argv gets `--secret-env NAME=NAME`, value goes to child env.
    pub secret_env: Vec<SecretEnv>,
    /// Spawn detached in the background (`-d`); incompatible with interactive/tty.
    pub detach: bool,
}

impl ExecSpec {
    /// A minimal exec spec running `command` in `name` with no extras.
    pub fn new(name: VmName, command: Vec<String>) -> Self {
        Self {
            name,
            command,
            interactive: false,
            tty: false,
            workdir: None,
            env: Vec::new(),
            secret_env: Vec::new(),
            detach: false,
        }
    }
}

/// One machine as reported by `smolvm machine ls --json`.
///
/// Only the fields airlock uses are modelled; unknown fields are ignored so
/// upstream additions do not break parsing.
#[derive(Clone, Debug, Deserialize)]
pub struct MachineInfo {
    /// Machine name.
    pub name: String,
    /// Lifecycle state (`created`, `running`, `stopped`, …).
    #[serde(default)]
    pub state: String,
    /// Host PID when running.
    #[serde(default)]
    pub pid: Option<u32>,
    /// vCPUs.
    #[serde(default)]
    pub cpus: Option<u32>,
    /// Memory MiB.
    #[serde(default)]
    pub memory_mib: Option<u32>,
    /// Whether outbound networking is enabled.
    #[serde(default)]
    pub network: bool,
    /// Image reference or `local:<digest>`.
    #[serde(default)]
    pub image: Option<String>,
    /// Unix creation timestamp (seconds).
    #[serde(default)]
    pub created_at: Option<i64>,
}

impl MachineInfo {
    /// Whether the machine is currently running.
    pub fn is_running(&self) -> bool {
        self.state == "running"
    }
}

/// Build argv (after the `smolvm` program name) for `machine create`.
pub fn create_args(spec: &CreateSpec) -> Vec<String> {
    let mut a = vec![
        "machine".into(),
        "create".into(),
        "--name".into(),
        spec.name.as_str().to_owned(),
    ];
    a.extend(spec.image.args());
    if let Some(cpus) = spec.cpus {
        a.push("--cpus".into());
        a.push(cpus.to_string());
    }
    if let Some(mem) = spec.mem {
        a.push("--mem".into());
        a.push(mem.to_string());
    }
    a.extend(spec.net.args());
    for v in &spec.volumes {
        a.push("--volume".into());
        a.push(v.clone());
    }
    for p in &spec.ports {
        a.push("--port".into());
        a.push(p.clone());
    }
    if spec.ssh_agent {
        a.push("--ssh-agent".into());
    }
    if let Some(workload) = &spec.workload {
        a.push("--".into());
        a.extend(workload.iter().cloned());
    }
    a
}

/// Build argv (after the `smolvm` program name) for `machine exec`. Secret values
/// are **not** included; each secret contributes only `--secret-env NAME=NAME`.
pub fn exec_args(spec: &ExecSpec) -> Vec<String> {
    let mut a = vec![
        "machine".into(),
        "exec".into(),
        "--name".into(),
        spec.name.as_str().to_owned(),
    ];
    if spec.detach {
        a.push("-d".into());
    }
    if spec.interactive {
        a.push("-i".into());
    }
    if spec.tty {
        a.push("-t".into());
    }
    if let Some(dir) = &spec.workdir {
        a.push("-w".into());
        a.push(dir.clone());
    }
    for (k, v) in &spec.env {
        a.push("-e".into());
        a.push(format!("{k}={v}"));
    }
    for s in &spec.secret_env {
        a.push("--secret-env".into());
        a.push(format!("{name}={name}", name = s.guest_name));
    }
    a.push("--".into());
    a.extend(spec.command.iter().cloned());
    a
}

/// A handle to the `smolvm` binary.
#[derive(Clone, Debug)]
pub struct Smolvm {
    bin: PathBuf,
}

impl Smolvm {
    /// Locate `smolvm` on `PATH`.
    pub fn discover() -> Result<Self> {
        let bin = which::which(BIN).map_err(|_| Error::ToolNotFound { tool: BIN.into() })?;
        Ok(Self { bin })
    }

    /// Use an explicit binary path (tests / non-standard installs).
    pub fn with_binary(bin: PathBuf) -> Self {
        Self { bin }
    }

    fn command(&self, args: &[String]) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args);
        cmd
    }

    /// Apply secret + plain env to a child command. Secrets ride the environment
    /// block, never argv.
    fn apply_env(cmd: &mut Command, secret_env: &[SecretEnv], env: &[(String, String)]) {
        for s in secret_env {
            cmd.env(&s.guest_name, s.value.expose());
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
    }

    /// Run a command to completion, capturing output; error on non-zero exit.
    fn run_checked(&self, args: &[String], secret_env: &[SecretEnv]) -> Result<String> {
        let mut cmd = self.command(args);
        Self::apply_env(&mut cmd, secret_env, &[]);
        let out = cmd.output().map_err(|source| Error::CommandSpawn {
            cmd: format!("{BIN} {}", redact_args(args)),
            source,
        })?;
        if !out.status.success() {
            return Err(Error::Smolvm {
                args: redact_args(args),
                code: out.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Run a command inheriting stdio (interactive), returning its exit status.
    fn run_inherit(
        &self,
        args: &[String],
        secret_env: &[SecretEnv],
        env: &[(String, String)],
    ) -> Result<ExitStatus> {
        let mut cmd = self.command(args);
        Self::apply_env(&mut cmd, secret_env, env);
        cmd.stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        cmd.status().map_err(|source| Error::CommandSpawn {
            cmd: format!("{BIN} {}", redact_args(args)),
            source,
        })
    }

    /// `smolvm machine create …`
    pub fn create(&self, spec: &CreateSpec) -> Result<()> {
        self.run_checked(&create_args(spec), &[])?;
        Ok(())
    }

    /// `smolvm machine create --from PACK --name NAME`
    pub fn create_from_pack(&self, name: &VmName, pack: &Path) -> Result<()> {
        let args = vec![
            "machine".into(),
            "create".into(),
            "--name".into(),
            name.as_str().to_owned(),
            "--from".into(),
            pack.display().to_string(),
        ];
        self.run_checked(&args, &[])?;
        Ok(())
    }

    /// `smolvm machine update --name NAME [--port …] [--volume …]` (stopped VM).
    pub fn update(&self, name: &VmName, ports: &[String], volumes: &[String]) -> Result<()> {
        let mut args = vec![
            "machine".into(),
            "update".into(),
            "--name".into(),
            name.as_str().to_owned(),
        ];
        for p in ports {
            args.push("--port".into());
            args.push(p.clone());
        }
        for v in volumes {
            args.push("--volume".into());
            args.push(v.clone());
        }
        self.run_checked(&args, &[])?;
        Ok(())
    }

    /// `smolvm machine start --name NAME`
    pub fn start(&self, name: &VmName) -> Result<()> {
        self.run_checked(&simple("start", name), &[])?;
        Ok(())
    }

    /// `smolvm machine stop --name NAME`
    pub fn stop(&self, name: &VmName) -> Result<()> {
        self.run_checked(&simple("stop", name), &[])?;
        Ok(())
    }

    /// `smolvm machine delete --name NAME [-f]`
    pub fn delete(&self, name: &VmName, force: bool) -> Result<()> {
        let mut args = simple("delete", name);
        if force {
            args.push("--force".into());
        }
        self.run_checked(&args, &[])?;
        Ok(())
    }

    /// `smolvm machine ls --json`, parsed.
    pub fn ls(&self) -> Result<Vec<MachineInfo>> {
        let json = self.run_checked(&["machine".into(), "ls".into(), "--json".into()], &[])?;
        let machines: Vec<MachineInfo> = serde_json::from_str(&json)?;
        Ok(machines)
    }

    /// Fetch a single machine's info, if it exists.
    pub fn get(&self, name: &VmName) -> Result<Option<MachineInfo>> {
        Ok(self.ls()?.into_iter().find(|m| m.name == name.as_str()))
    }

    /// `smolvm machine exec …` inheriting stdio (interactive shells / claude).
    pub fn exec_interactive(&self, spec: &ExecSpec) -> Result<ExitStatus> {
        self.run_inherit(&exec_args(spec), &spec.secret_env, &[])
    }

    /// `smolvm machine exec …` capturing stdout (provisioning / checks).
    pub fn exec_capture(&self, spec: &ExecSpec) -> Result<String> {
        self.run_checked(&exec_args(spec), &spec.secret_env)
    }

    /// `smolvm machine cp SRC DST` (either side may be `machine:path`).
    pub fn cp(&self, src: &str, dst: &str) -> Result<()> {
        self.run_checked(
            &[
                "machine".into(),
                "cp".into(),
                src.to_owned(),
                dst.to_owned(),
            ],
            &[],
        )?;
        Ok(())
    }

    /// Stream a host directory's *contents* into `guest_dest` inside the VM — an
    /// isolated VM-local copy (the host is never mounted). Implemented as
    /// `tar -C host -cf - . | smolvm exec -i -- sh -c 'tar -x …'`.
    pub fn copy_tree_in(
        &self,
        name: &VmName,
        host_dir: &Path,
        guest_dest: &str,
        excludes: &[String],
        chown_dev: bool,
    ) -> Result<()> {
        let mut tar = Command::new("tar");
        tar.arg("-C").arg(host_dir);
        for e in excludes {
            tar.arg(format!("--exclude=./{e}"));
            tar.arg(format!("--exclude=*/{e}"));
        }
        tar.arg("-cf").arg("-").arg(".");
        tar.stdout(Stdio::piped()).stderr(Stdio::inherit());
        let mut tar_child = tar.spawn().map_err(|source| Error::CommandSpawn {
            cmd: "tar".to_owned(),
            source,
        })?;
        let tar_out = tar_child.stdout.take().ok_or(Error::CommandFailed {
            cmd: "tar".to_owned(),
            code: -1,
        })?;

        let chown = if chown_dev {
            format!(" && chown -R dev:dev '{guest_dest}'")
        } else {
            String::new()
        };
        let guest_cmd = format!("mkdir -p '{guest_dest}' && tar -C '{guest_dest}' -xf -{chown}");
        let args = vec![
            "machine".to_owned(),
            "exec".to_owned(),
            "-i".to_owned(),
            "--name".to_owned(),
            name.as_str().to_owned(),
            "--".to_owned(),
            "sh".to_owned(),
            "-c".to_owned(),
            guest_cmd,
        ];
        let mut exec = self.command(&args);
        exec.stdin(Stdio::from(tar_out));
        let status = exec.status().map_err(|source| Error::CommandSpawn {
            cmd: format!("{BIN} machine exec (copy-in)"),
            source,
        })?;
        let _ = tar_child.wait();
        if !status.success() {
            return Err(Error::Smolvm {
                args: format!("machine exec -i --name {} -- tar -x", name.as_str()),
                code: status.code().unwrap_or(-1),
                stderr: format!("copy into {guest_dest} failed"),
            });
        }
        Ok(())
    }

    /// `smolvm machine monitor --name NAME …` (foreground, long-running).
    pub fn monitor(&self, name: &VmName, restart_policy: &str) -> Result<ExitStatus> {
        let args = vec![
            "machine".into(),
            "monitor".into(),
            "--name".into(),
            name.as_str().to_owned(),
            "--restart".into(),
            restart_policy.to_owned(),
        ];
        self.run_inherit(&args, &[], &[])
    }

    /// `smolvm pack create --from-vm NAME -o OUT` (VM must be stopped).
    pub fn pack_from_vm(
        &self,
        name: &VmName,
        out: &Path,
        cpus: Option<u32>,
        mem: Option<u32>,
    ) -> Result<()> {
        let mut args = vec![
            "pack".into(),
            "create".into(),
            "--from-vm".into(),
            name.as_str().to_owned(),
            "--output".into(),
            out.display().to_string(),
        ];
        if let Some(c) = cpus {
            args.push("--cpus".into());
            args.push(c.to_string());
        }
        if let Some(m) = mem {
            args.push("--mem".into());
            args.push(m.to_string());
        }
        self.run_checked(&args, &[])?;
        Ok(())
    }
}

fn simple(sub: &str, name: &VmName) -> Vec<String> {
    vec![
        "machine".into(),
        sub.to_owned(),
        "--name".into(),
        name.as_str().to_owned(),
    ]
}

/// Render argv for error/log messages. argv never contains secret *values*
/// (only `--secret-env NAME=NAME` references), so this is safe to surface.
fn redact_args(args: &[String]) -> String {
    args.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(s: &str) -> VmName {
        VmName::new(s).expect("valid name")
    }

    #[test]
    fn create_args_minimal() {
        let spec = CreateSpec {
            name: name("web-01"),
            image: ImageSource::Registry("ubuntu:24.04".into()),
            cpus: None,
            mem: None,
            net: NetSpec::Off,
            volumes: vec![],
            ports: vec![],
            ssh_agent: false,
            workload: None,
        };
        assert_eq!(
            create_args(&spec),
            vec![
                "machine",
                "create",
                "--name",
                "web-01",
                "--image",
                "ubuntu:24.04"
            ]
        );
    }

    #[test]
    fn create_args_full() {
        let spec = CreateSpec {
            name: name("web-01"),
            image: ImageSource::LocalArchive(PathBuf::from("/tmp/img.tar")),
            cpus: Some(2),
            mem: Some(4096),
            net: NetSpec::All,
            volumes: vec!["/host/src:/work:ro".into()],
            ports: vec!["2201:22".into()],
            ssh_agent: true,
            workload: Some(vec!["/usr/sbin/sshd".into(), "-D".into()]),
        };
        let args = create_args(&spec);
        assert!(args.contains(&"--cpus".to_string()) && args.contains(&"2".to_string()));
        assert!(args.contains(&"--mem".to_string()) && args.contains(&"4096".to_string()));
        assert!(args.contains(&"--net".to_string()));
        assert!(args.contains(&"--ssh-agent".to_string()));
        assert!(args.contains(&"--volume".to_string()));
        assert!(args.contains(&"/host/src:/work:ro".to_string()));
        assert!(args.contains(&"--port".to_string()));
        assert!(args.contains(&"2201:22".to_string()));
        assert!(args.contains(&"--image".to_string()));
        assert!(args.contains(&"/tmp/img.tar".to_string()));
        // workload after `--`
        let sep = args
            .iter()
            .position(|s| s == "--")
            .expect("workload separator");
        assert_eq!(&args[sep + 1..], &["/usr/sbin/sshd", "-D"]);
    }

    #[test]
    fn net_allow_expands_hosts_and_cidrs() {
        let net = NetSpec::Allow {
            hosts: vec!["api.anthropic.com".into(), "github.com".into()],
            cidrs: vec!["10.0.0.0/8".into()],
        };
        let args = net.args();
        assert_eq!(
            args,
            vec![
                "--allow-host",
                "api.anthropic.com",
                "--allow-host",
                "github.com",
                "--allow-cidr",
                "10.0.0.0/8",
            ]
        );
    }

    #[test]
    fn net_off_and_all() {
        assert!(NetSpec::Off.args().is_empty());
        assert_eq!(NetSpec::All.args(), vec!["--net"]);
    }

    #[test]
    fn exec_args_interactive_with_secret_refs_carry_no_values() {
        let mut spec = ExecSpec::new(name("web-01"), vec!["bash".into(), "-l".into()]);
        spec.interactive = true;
        spec.tty = true;
        spec.secret_env = vec![SecretEnv::new("GH_TOKEN", "super-secret")];
        spec.env = vec![("KUBECONFIG".into(), "/home/dev/.kube/config".into())];
        let args = exec_args(&spec);

        // The secret VALUE must never appear in argv.
        assert!(
            !args.iter().any(|a| a.contains("super-secret")),
            "secret leaked into argv: {args:?}"
        );
        // Only the reference NAME=NAME appears.
        assert!(args.contains(&"--secret-env".to_string()));
        assert!(args.contains(&"GH_TOKEN=GH_TOKEN".to_string()));
        assert!(args.contains(&"-i".to_string()) && args.contains(&"-t".to_string()));
        // Command comes after the `--` separator.
        let sep = args.iter().position(|s| s == "--").expect("separator");
        assert_eq!(&args[sep + 1..], &["bash", "-l"]);
    }

    #[test]
    fn image_source_packed_uses_from() {
        let src = ImageSource::Packed(PathBuf::from("/tmp/x.smolmachine"));
        assert_eq!(src.args(), vec!["--from", "/tmp/x.smolmachine"]);
    }

    #[test]
    fn machine_info_parses_real_schema() -> anyhow::Result<()> {
        // Trimmed from an actual `smolvm machine ls --json`.
        let json = r#"[
          {"name":"web-01","state":"running","pid":149773,"cpus":4,
           "memory_mib":8192,"network":true,"image":"local:abc",
           "created_at":1782894957,"ephemeral":false,"restart_policy":"never"}
        ]"#;
        let machines: Vec<MachineInfo> = serde_json::from_str(json)?;
        assert_eq!(machines.len(), 1);
        assert!(machines[0].is_running());
        assert_eq!(machines[0].pid, Some(149773));
        assert_eq!(machines[0].cpus, Some(4));
        Ok(())
    }
}
