//! Login flows and launch-time secret resolution.
//!
//! Everything here runs on the trusted host and produces values that are injected
//! into a VM only at launch — via smolvm `--secret-env` (env-style) or a read-only
//! host mount (kubeconfig). Nothing is written into an image or a checkpoint.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::paths::expand_tilde;
use crate::secrets::{parse_env_file, Secret, SecretEnv};

/// A read-only kubeconfig mount plus the guest `KUBECONFIG` path it exposes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KubeMount {
    /// `HOST_DIR:GUEST_DIR:ro` volume argument.
    pub volume: String,
    /// Value to set `KUBECONFIG` to inside the guest.
    pub kubeconfig_path: String,
}

/// Run `gh auth login` interactively (device flow). Inherits the terminal.
pub fn login_github() -> Result<()> {
    which::which("gh").map_err(|_| Error::ToolNotFound {
        tool: "gh".to_owned(),
    })?;
    let status = Command::new("gh")
        .args(["auth", "login"])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|source| Error::CommandSpawn {
            cmd: "gh auth login".to_owned(),
            source,
        })?;
    if !status.success() {
        return Err(Error::CommandFailed {
            cmd: "gh auth login".to_owned(),
            code: status.code().unwrap_or(-1),
        });
    }
    Ok(())
}

/// Best-effort GitHub token from `gh auth token`. Returns `None` when gh is
/// missing or the user is not logged in — secrets are never fatal.
pub fn github_token() -> Option<Secret> {
    let out = Command::new("gh").args(["auth", "token"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let token = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if token.is_empty() {
        None
    } else {
        Some(Secret::new(token))
    }
}

/// Resolve the env-style secrets a profile wants injected, in precedence order:
/// `.env` file, then GitHub token, then the Anthropic API key. All are optional.
pub fn resolve_secret_env(cfg: &Config, config_dir: &Path) -> Result<Vec<SecretEnv>> {
    let mut out: Vec<SecretEnv> = Vec::new();

    if let Some(env_file) = &cfg.secrets.env_file {
        let path = resolve_relative(config_dir, env_file);
        if path.is_file() {
            out.extend(parse_env_file(&path)?);
        } else {
            tracing::debug!(path = %path.display(), "env_file not found; skipping");
        }
    }

    if cfg.secrets.github {
        if let Some(token) = github_token() {
            out.push(SecretEnv {
                guest_name: "GH_TOKEN".to_owned(),
                value: token.clone(),
            });
            out.push(SecretEnv {
                guest_name: "GITHUB_TOKEN".to_owned(),
                value: token,
            });
        } else {
            tracing::debug!("github secret requested but no gh token available");
        }
    }

    if let Some(var) = &cfg.secrets.anthropic_api_key_env {
        match std::env::var(var) {
            Ok(val) if !val.is_empty() => {
                out.push(SecretEnv {
                    guest_name: "ANTHROPIC_API_KEY".to_owned(),
                    value: Secret::new(val),
                });
            }
            _ => tracing::debug!(var = %var, "anthropic api key env var not set; skipping"),
        }
    }

    Ok(out)
}

/// Compute the kubeconfig mount for a profile, if configured and present.
pub fn kubeconfig_mount(cfg: &Config) -> Option<KubeMount> {
    let raw = cfg.secrets.kubeconfig.as_ref()?;
    let host_path = expand_tilde(raw);
    if !host_path.is_file() {
        tracing::warn!(path = %host_path.display(), "kubeconfig not found; skipping mount");
        return None;
    }
    let parent = host_path.parent()?;
    let filename = host_path.file_name()?.to_string_lossy().into_owned();
    let guest_dir = "/home/dev/.kube";
    Some(KubeMount {
        volume: format!("{}:{}:ro", parent.display(), guest_dir),
        kubeconfig_path: format!("{guest_dir}/{filename}"),
    })
}

/// Whether a host SSH agent is available to forward.
pub fn ssh_agent_available() -> bool {
    match std::env::var_os("SSH_AUTH_SOCK") {
        Some(sock) if !sock.is_empty() => Path::new(&sock).exists(),
        _ => false,
    }
}

/// Resolve `raw` against `config_dir` unless it is absolute or tilde-prefixed.
fn resolve_relative(config_dir: &Path, raw: &str) -> std::path::PathBuf {
    if raw.starts_with('~') || Path::new(raw).is_absolute() {
        expand_tilde(raw)
    } else {
        config_dir.join(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kubeconfig_mount_none_when_unset() {
        let cfg = Config::default();
        assert!(kubeconfig_mount(&cfg).is_none());
    }

    #[test]
    fn kubeconfig_mount_builds_readonly_dir_mount() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let kube = tmp.path().join("kube");
        std::fs::create_dir_all(&kube)?;
        std::fs::write(kube.join("config"), "apiVersion: v1")?;

        let mut cfg = Config::default();
        cfg.secrets.kubeconfig = Some(kube.join("config").display().to_string());
        let mount = kubeconfig_mount(&cfg).expect("mount present");
        assert!(mount.volume.ends_with(":/home/dev/.kube:ro"));
        assert_eq!(mount.kubeconfig_path, "/home/dev/.kube/config");
        Ok(())
    }

    #[test]
    fn resolve_secret_env_reads_env_file_relative_to_config() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        std::fs::write(tmp.path().join(".env"), "FOO=bar\nBAZ=qux\n")?;

        let mut cfg = Config::default();
        // Disable network-dependent sources so this test is hermetic.
        cfg.secrets.github = false;
        cfg.secrets.anthropic_api_key_env = None;
        cfg.secrets.env_file = Some(".env".to_owned());

        let resolved = resolve_secret_env(&cfg, tmp.path())?;
        let names: Vec<&str> = resolved.iter().map(|s| s.guest_name.as_str()).collect();
        assert!(names.contains(&"FOO") && names.contains(&"BAZ"));
        Ok(())
    }

    #[test]
    fn resolve_relative_handles_absolute_and_relative() {
        let base = Path::new("/home/me/proj");
        assert_eq!(
            resolve_relative(base, "sub/.env"),
            Path::new("/home/me/proj/sub/.env")
        );
        assert_eq!(
            resolve_relative(base, "/etc/app/.env"),
            Path::new("/etc/app/.env")
        );
    }
}
