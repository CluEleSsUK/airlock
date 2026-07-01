//! Toolchain installation fragments for the generated Dockerfile.
//!
//! Each `*_install` function returns a self-contained Dockerfile fragment. They
//! are assembled conditionally by [`crate::image::render_dockerfile`] according
//! to the profile's toolchain switches. Versions are pinned by default and
//! overridable per profile.

use crate::config::Versions;

/// Default Go version (matches the host toolchain at time of writing).
pub const GO_DEFAULT: &str = "1.26.0";
/// Default Node.js major line (LTS).
pub const NODE_DEFAULT: &str = "22";
/// Default kubectl version.
pub const KUBECTL_DEFAULT: &str = "1.31.0";
/// Default Rust channel.
pub const RUST_DEFAULT: &str = "stable";

/// Versions resolved from a profile, with defaults filled in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// Go version, e.g. `1.26.0`.
    pub go: String,
    /// Node major line, e.g. `22`.
    pub node: String,
    /// kubectl version, e.g. `1.31.0`.
    pub kubectl: String,
    /// Rust channel, e.g. `stable`.
    pub rust: String,
}

/// Resolve configured versions, substituting built-in defaults for any omission.
pub fn resolve(v: &Versions) -> Resolved {
    Resolved {
        go: v.go.clone().unwrap_or_else(|| GO_DEFAULT.to_owned()),
        node: v.node.clone().unwrap_or_else(|| NODE_DEFAULT.to_owned()),
        kubectl: v
            .kubectl
            .clone()
            .unwrap_or_else(|| KUBECTL_DEFAULT.to_owned()),
        rust: v.rust.clone().unwrap_or_else(|| RUST_DEFAULT.to_owned()),
    }
}

/// Go, from the official tarball; symlinked onto the default `PATH`.
pub fn go_install(version: &str) -> String {
    format!(
        "# Go {version}\n\
         RUN curl -fsSL https://go.dev/dl/go{version}.linux-amd64.tar.gz \\\n\
         \x20   | tar -C /usr/local -xz \\\n\
         \x20&& ln -sf /usr/local/go/bin/go /usr/local/bin/go \\\n\
         \x20&& ln -sf /usr/local/go/bin/gofmt /usr/local/bin/gofmt\n\
         ENV GOPATH=/home/dev/go\n"
    )
}

/// Node.js runtime via NodeSource (installs the latest patch of the major line).
pub fn node_runtime(major: &str) -> String {
    format!(
        "# Node.js {major}.x runtime\n\
         RUN curl -fsSL https://deb.nodesource.com/setup_{major}.x | bash - \\\n\
         \x20&& apt-get install -y --no-install-recommends nodejs \\\n\
         \x20&& rm -rf /var/lib/apt/lists/*\n"
    )
}

/// TypeScript developer tooling (installed on top of the Node runtime).
pub fn ts_tooling() -> String {
    "# TypeScript tooling\n\
     RUN npm install -g pnpm typescript\n"
        .to_owned()
}

/// kubectl, pinned binary from the Kubernetes release bucket.
pub fn kubectl_install(version: &str) -> String {
    format!(
        "# kubectl {version}\n\
         RUN curl -fsSL -o /usr/local/bin/kubectl \\\n\
         \x20   https://dl.k8s.io/release/v{version}/bin/linux/amd64/kubectl \\\n\
         \x20&& chmod +x /usr/local/bin/kubectl\n"
    )
}

/// GitHub CLI from the official apt repository, plus a git credential helper that
/// uses `gh` (works once `GH_TOKEN` is injected at launch).
pub fn gh_install() -> String {
    "# GitHub CLI (+ git credential helper via gh)\n\
     RUN mkdir -p -m 755 /etc/apt/keyrings \\\n\
     \x20&& curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \\\n\
     \x20      -o /etc/apt/keyrings/githubcli-archive-keyring.gpg \\\n\
     \x20&& chmod go+r /etc/apt/keyrings/githubcli-archive-keyring.gpg \\\n\
     \x20&& echo \"deb [arch=amd64 signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] \
     https://cli.github.com/packages stable main\" > /etc/apt/sources.list.d/github-cli.list \\\n\
     \x20&& apt-get update \\\n\
     \x20&& apt-get install -y --no-install-recommends gh \\\n\
     \x20&& rm -rf /var/lib/apt/lists/* \\\n\
     \x20&& git config --system credential.\"https://github.com\".helper \"!gh auth git-credential\"\n"
        .to_owned()
}

/// Rust via rustup, installed system-wide and symlinked onto the default `PATH`.
pub fn rust_install(channel: &str) -> String {
    format!(
        "# Rust ({channel})\n\
         ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo\n\
         RUN curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \\\n\
         \x20   | sh -s -- -y --no-modify-path --profile minimal --default-toolchain {channel} \\\n\
         \x20&& ln -sf /usr/local/cargo/bin/* /usr/local/bin/ \\\n\
         \x20&& chmod -R a+rX /usr/local/rustup /usr/local/cargo\n"
    )
}

/// Claude Code, installed globally via npm (requires the Node runtime).
pub fn claude_install() -> String {
    "# Claude Code\n\
     RUN npm install -g @anthropic-ai/claude-code\n"
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Versions;

    #[test]
    fn resolve_uses_defaults_when_unset() {
        let r = resolve(&Versions::default());
        assert_eq!(r.go, GO_DEFAULT);
        assert_eq!(r.node, NODE_DEFAULT);
        assert_eq!(r.kubectl, KUBECTL_DEFAULT);
        assert_eq!(r.rust, RUST_DEFAULT);
    }

    #[test]
    fn resolve_honours_overrides() {
        let v = Versions {
            go: Some("1.27.1".into()),
            node: Some("20".into()),
            kubectl: None,
            rust: Some("1.95.0".into()),
        };
        let r = resolve(&v);
        assert_eq!(r.go, "1.27.1");
        assert_eq!(r.node, "20");
        assert_eq!(r.kubectl, KUBECTL_DEFAULT);
        assert_eq!(r.rust, "1.95.0");
    }

    #[test]
    fn install_fragments_embed_versions() {
        assert!(go_install("1.26.0").contains("go1.26.0.linux-amd64.tar.gz"));
        assert!(kubectl_install("1.31.0").contains("release/v1.31.0/bin/linux/amd64/kubectl"));
        assert!(node_runtime("22").contains("setup_22.x"));
        assert!(rust_install("stable").contains("--default-toolchain stable"));
        assert!(claude_install().contains("@anthropic-ai/claude-code"));
    }
}
