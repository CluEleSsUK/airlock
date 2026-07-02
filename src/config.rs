//! The `airlock.toml` profile schema and loading.
//!
//! A profile is project-local, discovered by walking up from the working
//! directory (like `.git`). Every field has a default so a minimal file — even an
//! empty one — yields a working configuration. Unknown keys are rejected to catch
//! typos early.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::ports::DEFAULT_SSH_BASE_PORT;

/// The config file name airlock looks for.
pub const CONFIG_FILENAME: &str = "airlock.toml";

/// A fully-parsed, validated profile.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Explicit profile name. Defaults to the config's parent directory name.
    pub name: Option<String>,
    /// Base image + toolchains to bake.
    pub image: ImageConfig,
    /// Per-VM CPU/memory.
    pub resources: Resources,
    /// Guest egress policy.
    pub network: NetworkConfig,
    /// Which host secrets to inject at launch.
    pub secrets: SecretsConfig,
    /// SSH access configuration.
    pub ssh: SshConfig,
    /// How host directories (project + repos) are shared into VMs.
    pub workspace: WorkspaceConfig,
    /// How the guest home is provisioned (shell, dotfiles, Claude config).
    pub home: HomeConfig,
    /// Extra raw host directory mounts (always bind).
    pub mounts: MountsConfig,
}

/// Base image and the toolchains baked on top of it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ImageConfig {
    /// Base OCI image (Debian/Ubuntu family expected).
    pub base: String,
    /// Explicit built-image tag. Defaults to `airlock/<profile>:latest`.
    pub tag: Option<String>,
    /// Optional registry prefix (e.g. `ghcr.io/you`). When set, `build` pushes
    /// there and `up` boots VMs from the registry ref — which is what enables
    /// portable `.smolmachine` checkpoints (`pack --from-vm` needs a registry image).
    pub registry: Option<String>,
    /// Toggle individual toolchains on/off.
    pub toolchains: Toolchains,
    /// Pinned toolchain versions (omit to use built-in defaults).
    pub versions: Versions,
    /// Which non-secret Claude settings to bake in.
    pub claude_settings: ClaudeSettings,
}

impl Default for ImageConfig {
    fn default() -> Self {
        Self {
            base: "ubuntu:24.04".to_owned(),
            tag: None,
            registry: None,
            toolchains: Toolchains::default(),
            versions: Versions::default(),
            claude_settings: ClaudeSettings::default(),
        }
    }
}

/// Toolchain feature switches. All default on — a batteries-included agent box.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Toolchains {
    /// Claude Code CLI.
    pub claude: bool,
    /// Rust (rustup + stable toolchain).
    pub rust: bool,
    /// Go toolchain.
    pub go: bool,
    /// Node.js (includes TypeScript/npm/pnpm).
    pub node: bool,
    /// `kubectl`.
    pub kubectl: bool,
    /// GitHub CLI (`gh`).
    pub gh: bool,
}

impl Default for Toolchains {
    fn default() -> Self {
        Self {
            claude: true,
            rust: true,
            go: true,
            node: true,
            kubectl: true,
            gh: true,
        }
    }
}

/// Pinned toolchain versions. `None` means "use the built-in pinned default".
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Versions {
    /// Go version, e.g. `1.26.0`.
    pub go: Option<String>,
    /// Node major/LTS line, e.g. `22`.
    pub node: Option<String>,
    /// kubectl version, e.g. `1.31.0`.
    pub kubectl: Option<String>,
    /// Rust toolchain channel, e.g. `stable` or `1.95.0`.
    pub rust: Option<String>,
}

/// Which of the user's `~/.claude` files/dirs to bake into the image. Secret
/// files (credentials, keys) are always excluded regardless of this list.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClaudeSettings {
    /// Host directory to copy from (defaults to `~/.claude`).
    pub source: Option<String>,
    /// Entries (files or dirs) under `source` to include.
    pub include: Vec<String>,
}

impl Default for ClaudeSettings {
    fn default() -> Self {
        Self {
            source: None,
            include: vec![
                "settings.json".to_owned(),
                "CLAUDE.md".to_owned(),
                "agents".to_owned(),
                "skills".to_owned(),
            ],
        }
    }
}

/// Per-VM resource allocation (mirrors smolvm defaults).
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Resources {
    /// vCPUs per VM.
    pub cpus: u32,
    /// Memory per VM in MiB (elastic via virtio balloon in smolvm).
    pub memory: u32,
}

impl Default for Resources {
    fn default() -> Self {
        Self {
            cpus: 4,
            memory: 8192,
        }
    }
}

/// Guest egress policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkPolicy {
    /// No outbound networking at all.
    Off,
    /// Unrestricted outbound.
    All,
    /// Outbound only to the configured hosts/CIDRs.
    Allow,
}

/// Network configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkConfig {
    /// Egress policy.
    pub policy: NetworkPolicy,
    /// Allowed hostnames (used when `policy = "allow"`).
    pub allow_hosts: Vec<String>,
    /// Allowed CIDR ranges (used when `policy = "allow"`).
    pub allow_cidrs: Vec<String>,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            policy: NetworkPolicy::All,
            allow_hosts: Vec::new(),
            allow_cidrs: Vec::new(),
        }
    }
}

/// Which host secrets to inject into each VM at launch.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecretsConfig {
    /// Path (relative to the config) to a `.env` file to inject as env vars.
    pub env_file: Option<String>,
    /// Inject `gh auth token` as `GH_TOKEN`/`GITHUB_TOKEN`.
    pub github: bool,
    /// Host env var whose value is injected as `ANTHROPIC_API_KEY` (if set).
    pub anthropic_api_key_env: Option<String>,
    /// Host kubeconfig to copy into the guest `~/.kube/config` (supports `~`).
    pub kubeconfig: Option<String>,
}

impl Default for SecretsConfig {
    fn default() -> Self {
        Self {
            env_file: Some(".env".to_owned()),
            github: true,
            anthropic_api_key_env: Some("ANTHROPIC_API_KEY".to_owned()),
            kubeconfig: None,
        }
    }
}

/// SSH access configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SshConfig {
    /// First host port used for SSH forwards; members increment from here.
    pub base_port: u16,
    /// Guest username to log in as.
    pub user: String,
}

impl Default for SshConfig {
    fn default() -> Self {
        Self {
            base_port: DEFAULT_SSH_BASE_PORT,
            user: "dev".to_owned(),
        }
    }
}

/// Extra host directory mounts shared into every VM.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MountsConfig {
    /// `HOST:GUEST[:ro]` bind mounts.
    pub volumes: Vec<String>,
}

/// How a host directory is made available inside a VM.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShareMode {
    /// Snapshot-copy into the VM at creation (isolated; host untouched).
    Copy,
    /// Live read-write bind mount (edits sync both ways).
    Bind,
    /// Live read-only bind mount.
    BindRo,
    /// Do not share.
    Off,
}

impl ShareMode {
    /// Whether this mode is a bind mount (vs a copy / off).
    pub fn is_bind(self) -> bool {
        matches!(self, Self::Bind | Self::BindRo)
    }
}

/// How host home content (dotfiles, Claude config) is provisioned into the guest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provision {
    /// Bake a secret-filtered subset into the image (no credentials).
    Bake,
    /// Copy the real thing into the VM at creation (isolated VM-local copy).
    Copy,
    /// Do not provision.
    Off,
}

/// How host directories (the project you run in, plus `repos`) reach the VM.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceConfig {
    /// Sharing mode for the project dir and `repos`.
    pub mode: ShareMode,
    /// Share the directory you run `airlock up` in as `/home/dev/project`.
    pub project: bool,
    /// Extra host dirs to share, each at `/home/dev/repos/<name>`.
    pub repos: Vec<String>,
    /// Path fragments to skip when copying (e.g. `node_modules`, `target`).
    pub exclude: Vec<String>,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            mode: ShareMode::Copy,
            project: true,
            repos: Vec::new(),
            // Heavy, always-regenerable dirs skipped when copying so `up` stays fast.
            exclude: ["node_modules", "target", ".venv", "__pycache__", "dist"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
        }
    }
}

/// How the guest `dev` home is set up.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HomeConfig {
    /// Login shell for `dev` (must be installable via apt, e.g. `fish`, `bash`, `zsh`).
    pub shell: String,
    /// Host dotfiles directory. Auto-detected from common locations if omitted.
    pub dotfiles: Option<String>,
    /// How to provision dotfiles.
    pub dotfiles_provision: Provision,
    /// How to provision `~/.claude` (bake subset / copy real / off).
    pub claude: Provision,
}

impl Default for HomeConfig {
    fn default() -> Self {
        Self {
            shell: "fish".to_owned(),
            dotfiles: None,
            dotfiles_provision: Provision::Bake,
            claude: Provision::Bake,
        }
    }
}

impl Config {
    /// Parse and validate a config from disk.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path).map_err(|source| Error::ConfigRead {
            path: path.to_path_buf(),
            source,
        })?;
        let cfg: Self = toml::from_str(&raw).map_err(|source| Error::ConfigParse {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Search upward from `start` for `airlock.toml`, then load it. Returns the
    /// config alongside the path it was found at.
    pub fn find_and_load(start: &Path) -> Result<(Self, PathBuf)> {
        let path = find_config(start).ok_or_else(|| Error::ConfigNotFound {
            searched_from: start.to_path_buf(),
        })?;
        let cfg = Self::load(&path)?;
        Ok((cfg, path))
    }

    /// Semantic validation beyond what the type system enforces.
    pub fn validate(&self) -> Result<()> {
        if self.resources.cpus == 0 {
            return Err(Error::ConfigValidate {
                reason: "resources.cpus must be at least 1".to_owned(),
            });
        }
        if self.resources.memory < 256 {
            return Err(Error::ConfigValidate {
                reason: "resources.memory must be at least 256 MiB".to_owned(),
            });
        }
        if self.ssh.base_port == 0 {
            return Err(Error::ConfigValidate {
                reason: "ssh.base_port must not be 0".to_owned(),
            });
        }
        if self.network.policy == NetworkPolicy::Allow
            && self.network.allow_hosts.is_empty()
            && self.network.allow_cidrs.is_empty()
        {
            return Err(Error::ConfigValidate {
                reason: "network.policy = \"allow\" requires at least one allow_hosts or \
                         allow_cidrs entry"
                    .to_owned(),
            });
        }
        Ok(())
    }

    /// The effective profile name: explicit `name`, else the config's parent
    /// directory name, sanitised into a valid VM-name prefix.
    pub fn profile_name(&self, config_path: &Path) -> String {
        let raw = self.name.clone().unwrap_or_else(|| {
            config_path
                .parent()
                .and_then(Path::file_name)
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "airlock".to_owned())
        });
        sanitize_profile(&raw)
    }

    /// The built-image tag for this profile.
    pub fn image_tag(&self, profile: &str) -> String {
        self.image
            .tag
            .clone()
            .unwrap_or_else(|| format!("airlock/{profile}:latest"))
    }

    /// The registry reference VMs boot from when a registry is configured.
    pub fn registry_ref(&self, profile: &str) -> Option<String> {
        self.image
            .registry
            .as_ref()
            .map(|r| format!("{}/airlock-{profile}:latest", r.trim_end_matches('/')))
    }
}

/// Walk up from `start` looking for an `airlock.toml`.
pub fn find_config(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let candidate = d.join(CONFIG_FILENAME);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = d.parent();
    }
    None
}

/// Turn an arbitrary directory/profile string into a valid VM-name prefix:
/// non-`[A-Za-z0-9_-]` become `-`, leading non-alphanumerics are trimmed.
pub fn sanitize_profile(raw: &str) -> String {
    let mapped: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = mapped.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
    if trimmed.is_empty() {
        "airlock".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Render a commented starter `airlock.toml` for `airlock init`.
pub fn scaffold_toml(name: &str) -> String {
    format!(
        r#"# airlock.toml — profile for microVM-sandboxed Claude Code
# Every field has a sensible default; delete what you don't need.

name = "{name}"

[image]
base = "ubuntu:24.04"
# tag = "airlock/{name}:latest"   # defaults to this
# registry = "ghcr.io/you"        # push here + boot from it; enables `airlock checkpoint`

[image.toolchains]
claude = true
rust = true
go = true
node = true      # includes TypeScript / npm / pnpm
kubectl = true
gh = true

# [image.versions]                # pin toolchains; omit for built-in defaults
# go = "1.26.0"
# node = "22"
# kubectl = "1.31.0"
# rust = "stable"

[image.claude_settings]
# Non-secret Claude config baked into the image (credentials are NEVER baked).
# source = "~/.claude"
include = ["settings.json", "CLAUDE.md", "agents", "skills"]

[resources]
cpus = 4
memory = 8192     # MiB

[network]
policy = "all"    # "off" | "all" | "allow"
# When policy = "allow", restrict egress to these:
# allow_hosts = ["api.anthropic.com", "github.com", "registry.npmjs.org"]
# allow_cidrs = []

[secrets]
# Secrets are injected at launch — never baked into the image or a checkpoint.
env_file = ".env"                          # inject KEY=VALUE pairs (if the file exists)
github = true                              # inject `gh auth token` as GH_TOKEN
anthropic_api_key_env = "ANTHROPIC_API_KEY"  # inject this host env var if present
# kubeconfig = "~/.kube/config"            # copy into the guest ~/.kube/config

[workspace]
# How your host dirs reach the VM: "copy" (isolated snapshot) | "bind" (live rw)
# | "bind-ro" (live read-only) | "off".
mode = "copy"
project = true            # share the dir you run `airlock up` in as /home/dev/project
repos = []                # extra host dirs → /home/dev/repos/<name>, e.g. ["~/code/foo"]
exclude = ["node_modules", "target", ".venv", "dist"]  # skipped when copying

[home]
shell = "fish"            # login shell for the `dev` user
# dotfiles = "~/repos/dotfiles"   # auto-detected from ~/repos/dotfiles, ~/.dotfiles, ~/dotfiles
dotfiles_provision = "bake"       # "bake" (secret-filtered) | "copy" | "off"
claude = "bake"                   # "bake" (settings only, no creds) | "copy" (real ~/.claude) | "off"

[ssh]
base_port = 2200   # member NN forwards host port base_port+NN to guest :22
user = "dev"

[mounts]
# Extra raw bind mounts, full control (HOST:GUEST[:ro]):
volumes = []
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_uses_defaults() -> anyhow::Result<()> {
        let cfg: Config = toml::from_str("")?;
        assert_eq!(cfg.image.base, "ubuntu:24.04");
        assert!(cfg.image.toolchains.claude);
        assert!(cfg.image.toolchains.rust);
        assert_eq!(cfg.resources.cpus, 4);
        assert_eq!(cfg.resources.memory, 8192);
        assert_eq!(cfg.network.policy, NetworkPolicy::All);
        assert_eq!(cfg.ssh.base_port, 2200);
        assert_eq!(cfg.ssh.user, "dev");
        cfg.validate()?;
        Ok(())
    }

    #[test]
    fn scaffold_is_valid_and_parses() -> anyhow::Result<()> {
        let toml_src = scaffold_toml("demo");
        let cfg: Config = toml::from_str(&toml_src)?;
        cfg.validate()?;
        assert_eq!(cfg.name.as_deref(), Some("demo"));
        assert_eq!(cfg.image_tag("demo"), "airlock/demo:latest");
        Ok(())
    }

    #[test]
    fn workspace_and_home_defaults() -> anyhow::Result<()> {
        let cfg: Config = toml::from_str("")?;
        assert_eq!(cfg.workspace.mode, ShareMode::Copy);
        assert!(cfg.workspace.project);
        assert_eq!(cfg.home.shell, "fish");
        assert_eq!(cfg.home.claude, Provision::Bake);
        assert_eq!(cfg.home.dotfiles_provision, Provision::Bake);
        assert!(!cfg.workspace.mode.is_bind());
        Ok(())
    }

    #[test]
    fn share_mode_parses_kebab_case() {
        let cfg: Config = toml::from_str("[workspace]\nmode = \"bind-ro\"\n").expect("parses");
        assert_eq!(cfg.workspace.mode, ShareMode::BindRo);
        assert!(cfg.workspace.mode.is_bind());
    }

    #[test]
    fn unknown_field_is_rejected() {
        let err = toml::from_str::<Config>("definitely_not_a_field = 1");
        assert!(err.is_err(), "unknown fields should be rejected");
    }

    #[test]
    fn allow_policy_requires_entries() {
        let cfg: Config = toml::from_str("[network]\npolicy = \"allow\"\n").expect("parses");
        assert!(cfg.validate().is_err(), "empty allow-list must be invalid");

        let ok: Config = toml::from_str(
            "[network]\npolicy = \"allow\"\nallow_hosts = [\"api.anthropic.com\"]\n",
        )
        .expect("parses");
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn zero_cpus_is_invalid() {
        let cfg: Config = toml::from_str("[resources]\ncpus = 0\n").expect("parses");
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn profile_name_falls_back_to_dir() {
        let cfg = Config::default();
        let name = cfg.profile_name(Path::new("/home/me/my project/airlock.toml"));
        // "my project" → sanitised
        assert_eq!(name, "my-project");
    }

    #[test]
    fn sanitize_profile_handles_edge_cases() {
        assert_eq!(sanitize_profile("web"), "web");
        assert_eq!(sanitize_profile("my.proj"), "my-proj");
        assert_eq!(sanitize_profile("--weird--"), "weird--");
        assert_eq!(sanitize_profile("***"), "airlock");
        assert_eq!(sanitize_profile("123app"), "123app");
    }

    #[test]
    fn registry_ref_is_none_by_default_and_formats_when_set() {
        let cfg = Config::default();
        assert!(cfg.registry_ref("web").is_none());

        let with_reg: Config =
            toml::from_str("[image]\nregistry = \"ghcr.io/me/\"\n").expect("parses");
        assert_eq!(
            with_reg.registry_ref("web").as_deref(),
            Some("ghcr.io/me/airlock-web:latest")
        );
    }

    #[test]
    fn find_config_walks_upward() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let nested = tmp.path().join("a/b/c");
        std::fs::create_dir_all(&nested)?;
        std::fs::write(tmp.path().join(CONFIG_FILENAME), "name = \"root\"\n")?;
        let found = find_config(&nested).expect("should find upward");
        assert_eq!(found, tmp.path().join(CONFIG_FILENAME));
        Ok(())
    }
}
