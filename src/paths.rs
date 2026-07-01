//! Host-side filesystem layout for airlock state.
//!
//! Per-profile artifacts (SSH keypair, image build context) live under the XDG
//! data dir; mutable fleet state (the `fleet.json` index) lives under the XDG
//! state dir. [`Layout`] can be constructed from explicit roots so tests run
//! fully sandboxed in a temp dir.

use std::path::{Path, PathBuf};

use directories::{BaseDirs, ProjectDirs};

use crate::error::{Error, Result};

const QUALIFIER: &str = "ai";
const ORG: &str = "confidential";
const APP: &str = "airlock";

/// Resolved host directories airlock reads and writes.
#[derive(Clone, Debug)]
pub struct Layout {
    data_root: PathBuf,
    state_root: PathBuf,
}

impl Layout {
    /// Discover the platform's standard data/state directories for airlock.
    pub fn discover() -> Result<Self> {
        let pd = ProjectDirs::from(QUALIFIER, ORG, APP).ok_or(Error::NoHomeDir)?;
        let data_root = pd.data_dir().to_path_buf();
        // `state_dir` is `Some` on Linux, `None` on macOS/Windows — fall back to data.
        let state_root = pd
            .state_dir()
            .map_or_else(|| data_root.clone(), Path::to_path_buf);
        Ok(Self {
            data_root,
            state_root,
        })
    }

    /// Construct a layout rooted at explicit directories (used by tests).
    pub fn with_roots(data_root: PathBuf, state_root: PathBuf) -> Self {
        Self {
            data_root,
            state_root,
        }
    }

    /// Per-profile data directory (keys, build context, cached image ref).
    pub fn profile_data(&self, profile: &str) -> PathBuf {
        self.data_root.join(profile)
    }

    /// Per-profile state directory (fleet index).
    pub fn profile_state(&self, profile: &str) -> PathBuf {
        self.state_root.join(profile)
    }

    /// Directory holding the generated Dockerfile and staged build inputs.
    pub fn build_context(&self, profile: &str) -> PathBuf {
        self.profile_data(profile).join("build")
    }

    /// Private SSH key used to reach this profile's VMs.
    pub fn ssh_key(&self, profile: &str) -> PathBuf {
        self.profile_data(profile).join("id_ed25519")
    }

    /// Public SSH key baked into the profile's image as `authorized_keys`.
    pub fn ssh_pubkey(&self, profile: &str) -> PathBuf {
        self.profile_data(profile).join("id_ed25519.pub")
    }

    /// The saved OCI image archive (`docker save` output) for this profile.
    pub fn image_archive(&self, profile: &str) -> PathBuf {
        self.profile_data(profile).join("image.tar")
    }

    /// The JSON fleet index recording member name → host port, image, order.
    pub fn fleet_index(&self, profile: &str) -> PathBuf {
        self.profile_state(profile).join("fleet.json")
    }

    /// Ensure the per-profile data and state directories exist.
    pub fn ensure_profile_dirs(&self, profile: &str) -> Result<()> {
        std::fs::create_dir_all(self.profile_data(profile))?;
        std::fs::create_dir_all(self.profile_state(profile))?;
        Ok(())
    }
}

/// Expand a leading `~` or `~/` to the user's home directory. Paths without a
/// leading tilde are returned unchanged.
pub fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        if let Some(base) = BaseDirs::new() {
            return base.home_dir().to_path_buf();
        }
    } else if let Some(rest) = path.strip_prefix("~/") {
        if let Some(base) = BaseDirs::new() {
            return base.home_dir().join(rest);
        }
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_paths_are_namespaced() {
        let layout = Layout::with_roots(PathBuf::from("/data"), PathBuf::from("/state"));
        assert_eq!(layout.profile_data("web"), PathBuf::from("/data/web"));
        assert_eq!(layout.profile_state("web"), PathBuf::from("/state/web"));
        assert_eq!(
            layout.fleet_index("web"),
            PathBuf::from("/state/web/fleet.json")
        );
        assert_eq!(layout.ssh_key("web"), PathBuf::from("/data/web/id_ed25519"));
    }

    #[test]
    fn distinct_profiles_do_not_share_dirs() {
        let layout = Layout::with_roots(PathBuf::from("/data"), PathBuf::from("/state"));
        assert_ne!(layout.profile_data("a"), layout.profile_data("b"));
        assert_ne!(layout.fleet_index("a"), layout.fleet_index("b"));
    }

    #[test]
    fn ensure_profile_dirs_creates_them() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let layout = Layout::with_roots(tmp.path().join("data"), tmp.path().join("state"));
        layout.ensure_profile_dirs("proj")?;
        assert!(layout.profile_data("proj").is_dir());
        assert!(layout.profile_state("proj").is_dir());
        Ok(())
    }

    #[test]
    fn expand_tilde_passes_through_absolute() {
        assert_eq!(expand_tilde("/etc/hosts"), PathBuf::from("/etc/hosts"));
        assert_eq!(expand_tilde("./rel"), PathBuf::from("./rel"));
    }
}
