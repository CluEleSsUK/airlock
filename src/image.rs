//! Base image construction.
//!
//! [`render_dockerfile`] is a pure function of the profile (unit-tested). [`build`]
//! stages a context (Dockerfile + SSH public key + sanitised Claude settings),
//! runs `docker`/`podman build`, and `save`s the result to a tar archive that
//! smolvm boots via `--image ./image.tar`.
//!
//! Invariant: **no secret ever enters the build context.** Claude credentials,
//! `.env`, keys and PEMs are filtered out during staging; secrets are injected at
//! launch instead (see [`crate::secrets`]).

use std::path::Path;
use std::process::{Command, Stdio};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::paths::{expand_tilde, Layout};
use crate::toolchains;

/// Filenames that must never be copied into an image layer.
const SECRET_DENYLIST_EXACT: &[&str] = &[".credentials.json", "id_rsa", "id_ed25519"];
const SECRET_DENYLIST_SUFFIX: &[&str] = &[".key", ".pem"];

/// Render the Dockerfile for `cfg`. Pure — depends only on the config.
pub fn render_dockerfile(cfg: &Config) -> String {
    let v = toolchains::resolve(&cfg.image.versions);
    let tc = &cfg.image.toolchains;
    let need_node = tc.node || tc.claude;

    let mut s = String::new();
    s.push_str(&format!("FROM {}\n", cfg.image.base));
    s.push_str("LABEL org.airlock.managed=\"true\"\n");
    s.push_str("ENV DEBIAN_FRONTEND=noninteractive\n\n");

    // Base packages.
    s.push_str(
        "RUN apt-get update && apt-get install -y --no-install-recommends \\\n\
         \x20     ca-certificates curl wget git jq unzip xz-utils gnupg \\\n\
         \x20     openssh-server sudo tini build-essential pkg-config locales \\\n\
         \x20&& rm -rf /var/lib/apt/lists/*\n\n",
    );

    // Locale.
    s.push_str(
        "RUN sed -i '/en_US.UTF-8/s/^# //' /etc/locale.gen && locale-gen\n\
         ENV LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8\n\n",
    );

    // Non-root dev user (Ubuntu 24.04 ships a uid-1000 `ubuntu` user we reclaim).
    s.push_str(
        "RUN userdel -r ubuntu 2>/dev/null || true \\\n\
         \x20&& useradd --create-home --shell /bin/bash --uid 1000 dev \\\n\
         \x20&& echo 'dev ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/90-dev \\\n\
         \x20&& chmod 0440 /etc/sudoers.d/90-dev\n\n",
    );

    // sshd: key-only login for `dev`, and accept the env vars airlock forwards.
    s.push_str(
        "RUN mkdir -p /run/sshd /home/dev/.ssh && chmod 700 /home/dev/.ssh\n\
         COPY authorized_keys /home/dev/.ssh/authorized_keys\n\
         RUN chown -R dev:dev /home/dev/.ssh \\\n\
         \x20&& chmod 600 /home/dev/.ssh/authorized_keys \\\n\
         \x20&& ssh-keygen -A \\\n\
         \x20&& printf '%s\\n' \\\n\
         \x20     'PermitRootLogin no' \\\n\
         \x20     'PasswordAuthentication no' \\\n\
         \x20     'PubkeyAuthentication yes' \\\n\
         \x20     'AcceptEnv GH_TOKEN GITHUB_TOKEN ANTHROPIC_API_KEY ANTHROPIC_AUTH_TOKEN KUBECONFIG LANG LC_*' \\\n\
         \x20     'X11Forwarding no' \\\n\
         \x20     > /etc/ssh/sshd_config.d/airlock.conf\n\
         RUN printf '#!/bin/sh\\nmkdir -p /run/sshd\\nexec /usr/sbin/sshd -D -e\\n' \\\n\
         \x20      > /usr/local/bin/airlock-sshd && chmod +x /usr/local/bin/airlock-sshd\n\n",
    );

    // Toolchains, in dependency order.
    if tc.go {
        s.push_str(&toolchains::go_install(&v.go));
        s.push('\n');
    }
    if need_node {
        s.push_str(&toolchains::node_runtime(&v.node));
        s.push('\n');
    }
    if tc.node {
        s.push_str(&toolchains::ts_tooling());
        s.push('\n');
    }
    if tc.kubectl {
        s.push_str(&toolchains::kubectl_install(&v.kubectl));
        s.push('\n');
    }
    if tc.gh {
        s.push_str(&toolchains::gh_install());
        s.push('\n');
    }
    if tc.rust {
        s.push_str(&toolchains::rust_install(&v.rust));
        s.push('\n');
    }
    if tc.claude {
        s.push_str(&toolchains::claude_install());
        s.push('\n');
    }

    // Baked non-secret Claude settings, for both the root (exec) and dev (ssh) homes.
    s.push_str(
        "COPY claude-settings/ /home/dev/.claude/\n\
         COPY claude-settings/ /root/.claude/\n\
         RUN chown -R dev:dev /home/dev/.claude\n\n",
    );

    // A friendly default working directory.
    s.push_str(
        "RUN mkdir -p /home/dev/project && chown dev:dev /home/dev/project\n\
         WORKDIR /home/dev/project\n\
         EXPOSE 22\n",
    );

    s
}

/// Build the base image for `profile` and return the path to the saved archive.
pub fn build(cfg: &Config, layout: &Layout, profile: &str) -> Result<std::path::PathBuf> {
    layout.ensure_profile_dirs(profile)?;
    crate::ssh::ensure_keypair(layout, profile)?;

    let ctx = layout.build_context(profile);
    stage_context(cfg, layout, profile, &ctx)?;

    let engine = detect_engine()?;
    let tag = cfg.image_tag(profile);

    tracing::info!(engine = %engine, tag = %tag, "building base image");
    run_build(&engine, &tag, &ctx)?;

    let archive = layout.image_archive(profile);
    tracing::info!(archive = %archive.display(), "saving image archive");
    run_save(&engine, &tag, &archive)?;

    if let Some(reg_ref) = cfg.registry_ref(profile) {
        tracing::info!(reference = %reg_ref, "pushing image to registry (enables checkpoints)");
        run_push(&engine, &tag, &reg_ref)?;
    }

    Ok(archive)
}

/// Detect an available container build engine.
pub fn detect_engine() -> Result<String> {
    for engine in ["docker", "podman"] {
        if which::which(engine).is_ok() {
            return Ok(engine.to_owned());
        }
    }
    Err(Error::ToolNotFound {
        tool: "docker or podman".to_owned(),
    })
}

fn stage_context(cfg: &Config, layout: &Layout, profile: &str, ctx: &Path) -> Result<()> {
    std::fs::create_dir_all(ctx)?;

    // Dockerfile.
    std::fs::write(ctx.join("Dockerfile"), render_dockerfile(cfg))?;

    // authorized_keys ← the profile's SSH public key.
    let pubkey = layout.ssh_pubkey(profile);
    std::fs::copy(&pubkey, ctx.join("authorized_keys")).map_err(|source| Error::ConfigRead {
        path: pubkey,
        source,
    })?;

    // Sanitised Claude settings.
    let settings_dir = ctx.join("claude-settings");
    std::fs::create_dir_all(&settings_dir)?;
    // Marker so `COPY claude-settings/` always has content even when nothing else
    // is staged (an empty COPY source is an error in Docker).
    std::fs::write(settings_dir.join(".airlock-keep"), b"")?;
    stage_claude_settings(cfg, &settings_dir)?;

    Ok(())
}

/// Copy the profile's allowed Claude settings into `dest`, filtering out secrets.
fn stage_claude_settings(cfg: &Config, dest: &Path) -> Result<usize> {
    let source = cfg
        .image
        .claude_settings
        .source
        .clone()
        .map_or_else(|| expand_tilde("~/.claude"), |s| expand_tilde(&s));

    // Resolve a symlinked ~/.claude to its real location.
    let source = std::fs::canonicalize(&source).unwrap_or(source);
    if !source.is_dir() {
        tracing::warn!(source = %source.display(), "claude settings source not found; skipping");
        return Ok(0);
    }

    let mut copied = 0usize;
    for entry in &cfg.image.claude_settings.include {
        if is_secret_filename(entry) {
            continue;
        }
        let src = source.join(entry);
        if !src.exists() {
            continue;
        }
        copied += copy_filtered(&src, &dest.join(entry))?;
    }
    Ok(copied)
}

/// Recursively copy `src` → `dst`, skipping secret-looking filenames. Returns the
/// number of regular files copied.
fn copy_filtered(src: &Path, dst: &Path) -> Result<usize> {
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if is_secret_filename(&name) {
        return Ok(0);
    }

    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        let mut count = 0;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            count += copy_filtered(&entry.path(), &dst.join(entry.file_name()))?;
        }
        Ok(count)
    } else if src.is_file() {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src, dst)?;
        Ok(1)
    } else {
        Ok(0)
    }
}

/// Whether `name` looks like secret material that must not be baked.
fn is_secret_filename(name: &str) -> bool {
    if SECRET_DENYLIST_EXACT.contains(&name) {
        return true;
    }
    if name.starts_with(".env") {
        return true;
    }
    SECRET_DENYLIST_SUFFIX.iter().any(|s| name.ends_with(s))
}

fn run_build(engine: &str, tag: &str, ctx: &Path) -> Result<()> {
    let status = Command::new(engine)
        .args(["build", "-t", tag, &ctx.display().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|source| Error::CommandSpawn {
            cmd: format!("{engine} build"),
            source,
        })?;
    if !status.success() {
        return Err(Error::Docker {
            engine: engine.to_owned(),
            args: format!("build -t {tag}"),
            code: status.code().unwrap_or(-1),
            stderr: "see build output above".to_owned(),
        });
    }
    Ok(())
}

fn run_push(engine: &str, local_tag: &str, reg_ref: &str) -> Result<()> {
    let tagged = Command::new(engine)
        .args(["tag", local_tag, reg_ref])
        .output()
        .map_err(|source| Error::CommandSpawn {
            cmd: format!("{engine} tag"),
            source,
        })?;
    if !tagged.status.success() {
        return Err(Error::Docker {
            engine: engine.to_owned(),
            args: format!("tag {local_tag} {reg_ref}"),
            code: tagged.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&tagged.stderr).trim().to_owned(),
        });
    }
    let status = Command::new(engine)
        .args(["push", reg_ref])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|source| Error::CommandSpawn {
            cmd: format!("{engine} push"),
            source,
        })?;
    if !status.success() {
        return Err(Error::Docker {
            engine: engine.to_owned(),
            args: format!("push {reg_ref}"),
            code: status.code().unwrap_or(-1),
            stderr: "see push output above".to_owned(),
        });
    }
    Ok(())
}

fn run_save(engine: &str, tag: &str, archive: &Path) -> Result<()> {
    let out = Command::new(engine)
        .args(["save", tag, "-o", &archive.display().to_string()])
        .output()
        .map_err(|source| Error::CommandSpawn {
            cmd: format!("{engine} save"),
            source,
        })?;
    if !out.status.success() {
        return Err(Error::Docker {
            engine: engine.to_owned(),
            args: format!("save {tag}"),
            code: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn cfg_with(toml_src: &str) -> Config {
        toml::from_str(toml_src).expect("valid config")
    }

    #[test]
    fn dockerfile_includes_all_toolchains_by_default() {
        let df = render_dockerfile(&Config::default());
        assert!(df.starts_with("FROM ubuntu:24.04"));
        assert!(df.contains("go1.26.0.linux-amd64"));
        assert!(df.contains("setup_22.x"));
        assert!(df.contains("npm install -g pnpm typescript"));
        assert!(df.contains("release/v1.31.0/bin/linux/amd64/kubectl"));
        assert!(df.contains("apt-get install -y --no-install-recommends gh"));
        assert!(df.contains("sh.rustup.rs"));
        assert!(df.contains("@anthropic-ai/claude-code"));
        // sshd + dev user + baked settings.
        assert!(df.contains("useradd --create-home"));
        assert!(df.contains("authorized_keys"));
        assert!(df.contains("COPY claude-settings/ /home/dev/.claude/"));
    }

    #[test]
    fn disabled_toolchains_are_omitted() {
        let df = render_dockerfile(&cfg_with(
            "[image.toolchains]\nrust=false\ngo=false\nkubectl=false\ngh=false\nnode=false\nclaude=false\n",
        ));
        assert!(!df.contains("sh.rustup.rs"));
        assert!(!df.contains("go1.26.0"));
        assert!(!df.contains("kubectl"));
        assert!(!df.contains("setup_22.x"));
        assert!(!df.contains("@anthropic-ai/claude-code"));
    }

    #[test]
    fn claude_pulls_in_node_runtime_even_if_node_off() {
        // Claude needs Node, so the runtime must appear, but TS tooling must not.
        let df = render_dockerfile(&cfg_with(
            "[image.toolchains]\nnode=false\nclaude=true\nrust=false\ngo=false\nkubectl=false\ngh=false\n",
        ));
        assert!(
            df.contains("setup_22.x"),
            "node runtime should be present for claude"
        );
        assert!(
            !df.contains("pnpm typescript"),
            "TS tooling should be absent when node=false"
        );
        assert!(df.contains("@anthropic-ai/claude-code"));
    }

    #[test]
    fn honours_base_image_and_version_overrides() {
        let df = render_dockerfile(&cfg_with(
            "[image]\nbase=\"debian:12\"\n[image.versions]\ngo=\"1.27.0\"\nkubectl=\"1.30.5\"\n",
        ));
        assert!(df.starts_with("FROM debian:12"));
        assert!(df.contains("go1.27.0.linux-amd64"));
        assert!(df.contains("release/v1.30.5/"));
    }

    #[test]
    fn secret_filenames_are_recognised() {
        assert!(is_secret_filename(".credentials.json"));
        assert!(is_secret_filename(".env"));
        assert!(is_secret_filename(".env.local"));
        assert!(is_secret_filename("server.key"));
        assert!(is_secret_filename("cert.pem"));
        assert!(is_secret_filename("id_ed25519"));
        assert!(!is_secret_filename("settings.json"));
        assert!(!is_secret_filename("CLAUDE.md"));
    }

    #[test]
    fn stage_filters_out_secrets() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let source = tmp.path().join("dotclaude");
        std::fs::create_dir_all(&source)?;
        std::fs::write(source.join("settings.json"), "{}")?;
        std::fs::write(source.join(".credentials.json"), "SECRET")?;
        std::fs::create_dir_all(source.join("agents"))?;
        std::fs::write(source.join("agents/reviewer.md"), "agent")?;
        std::fs::write(source.join("agents/.env"), "LEAK=1")?;

        let cfg = cfg_with(&format!(
            "[image.claude_settings]\nsource=\"{}\"\ninclude=[\"settings.json\",\".credentials.json\",\"agents\"]\n",
            source.display()
        ));
        let dest = tmp.path().join("staged");
        std::fs::create_dir_all(&dest)?;
        stage_claude_settings(&cfg, &dest)?;

        assert!(dest.join("settings.json").exists());
        assert!(dest.join("agents/reviewer.md").exists());
        // Secrets must be filtered even if explicitly included / nested.
        assert!(!dest.join(".credentials.json").exists());
        assert!(!dest.join("agents/.env").exists());
        Ok(())
    }
}
