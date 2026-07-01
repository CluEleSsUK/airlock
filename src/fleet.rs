//! Fleet orchestration: many non-colliding VMs from one profile.
//!
//! A [`Fleet`] ties together the profile config, the host [`Layout`], the built
//! image, and the [`Smolvm`] executor. Each member is a distinct smolvm machine
//! named `<profile>-NN` with its own overlay, a unique forwarded SSH port, and an
//! entry in a persisted [`FleetIndex`]. Because every member is an independent
//! machine, members never share mutable guest state.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::auth;
use crate::config::{Config, NetworkPolicy};
use crate::error::{Error, Result};
use crate::image;
use crate::names::VmName;
use crate::paths::Layout;
use crate::ports;
use crate::secrets::SecretEnv;
use crate::smolvm::{CreateSpec, ExecSpec, ImageSource, MachineInfo, NetSpec, Smolvm};

/// One VM in a fleet, as recorded in the persisted index.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Member {
    /// Machine name (`<profile>-NN`).
    pub name: VmName,
    /// Ordinal within the fleet.
    pub index: u32,
    /// Host port forwarded to the guest sshd (`0` = not forwarded).
    pub ssh_port: u16,
    /// Image tag the member was created from.
    pub image_tag: String,
    /// Unix creation time (seconds).
    pub created_at: i64,
}

/// The persisted per-profile fleet index.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FleetIndex {
    /// Fleet members, in creation order.
    pub members: Vec<Member>,
}

impl FleetIndex {
    /// Load the index, returning an empty one if the file does not exist.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    /// Persist the index atomically (write-temp-then-rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// The next unused ordinal (fleet ordinals start at 1).
    pub fn next_index(&self) -> u32 {
        self.members
            .iter()
            .map(|m| m.index)
            .max()
            .map_or(1, |m| m + 1)
    }

    /// The set of host SSH ports already assigned to members.
    pub fn used_ports(&self) -> HashSet<u16> {
        self.members
            .iter()
            .map(|m| m.ssh_port)
            .filter(|p| *p != 0)
            .collect()
    }
}

/// A fleet member joined with its live smolvm state (if any).
pub type MemberStatus = (Member, Option<MachineInfo>);

/// Secret and non-secret environment injected into an interactive session.
type SessionEnv = (Vec<SecretEnv>, Vec<(String, String)>);

/// An orchestrator bound to one profile.
pub struct Fleet {
    cfg: Config,
    config_path: PathBuf,
    layout: Layout,
    profile: String,
    smolvm: Smolvm,
}

impl Fleet {
    /// Open the fleet for the profile discovered upward from the working dir.
    pub fn open(smolvm: Smolvm) -> Result<Self> {
        let cwd = std::env::current_dir()?;
        Self::open_from(&cwd, smolvm)
    }

    /// Open the fleet for the profile discovered upward from `start_dir`.
    pub fn open_from(start_dir: &Path, smolvm: Smolvm) -> Result<Self> {
        let (cfg, config_path) = Config::find_and_load(start_dir)?;
        let layout = Layout::discover()?;
        let profile = cfg.profile_name(&config_path);
        layout.ensure_profile_dirs(&profile)?;
        Ok(Self {
            cfg,
            config_path,
            layout,
            profile,
            smolvm,
        })
    }

    /// Profile name.
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// The loaded config.
    pub fn cfg(&self) -> &Config {
        &self.cfg
    }

    /// The host layout.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The smolvm executor.
    pub fn smolvm(&self) -> &Smolvm {
        &self.smolvm
    }

    /// The directory containing this profile's `airlock.toml`.
    pub fn config_dir(&self) -> &Path {
        self.config_path.parent().unwrap_or(Path::new("."))
    }

    /// Load the fleet index.
    pub fn load_index(&self) -> Result<FleetIndex> {
        FleetIndex::load(&self.layout.fleet_index(&self.profile))
    }

    /// Save the fleet index.
    pub fn save_index(&self, index: &FleetIndex) -> Result<()> {
        index.save(&self.layout.fleet_index(&self.profile))
    }

    /// Build the base image if the archive is missing (or `rebuild`), returning
    /// the archive path.
    pub fn ensure_image(&self, rebuild: bool) -> Result<PathBuf> {
        let archive = self.layout.image_archive(&self.profile);
        if rebuild || !archive.exists() {
            image::build(&self.cfg, &self.layout, &self.profile)?;
        }
        Ok(archive)
    }

    /// Bring up `count` new members, building the image first if needed. Each new
    /// member is created, started, and persisted before moving to the next, so a
    /// failure part-way still records what succeeded.
    pub fn up(&self, count: usize, rebuild: bool) -> Result<Vec<Member>> {
        let archive = self.ensure_image(rebuild)?;
        let tag = self.cfg.image_tag(&self.profile);
        let index_path = self.layout.fleet_index(&self.profile);
        let mut index = FleetIndex::load(&index_path)?;
        let mut used = index.used_ports();

        let base = self.cfg.ssh.base_port;
        let start = index.next_index();
        let ssh_agent = auth::ssh_agent_available();
        let image_src = self.launch_image(&archive);
        let net = self.launch_net();
        let volumes = self.volumes();

        let mut created = Vec::with_capacity(count);
        for i in 0..count {
            let idx = start + i as u32;
            let name = VmName::member(&self.profile, idx)?;
            let preferred = base.saturating_add(u16::try_from(idx).unwrap_or(u16::MAX));
            let port = ports::find_free_port(preferred, &used)?;
            used.insert(port);

            let spec = CreateSpec {
                name: name.clone(),
                image: image_src.clone(),
                cpus: Some(self.cfg.resources.cpus),
                mem: Some(self.cfg.resources.memory),
                net: net.clone(),
                volumes: volumes.clone(),
                ports: vec![format!("{port}:22")],
                ssh_agent,
                workload: None,
            };
            tracing::info!(vm = %name, port, "creating VM");
            self.smolvm.create(&spec)?;
            self.smolvm.start(&name)?;
            self.ensure_sshd(&name);

            let member = Member {
                name,
                index: idx,
                ssh_port: port,
                image_tag: tag.clone(),
                created_at: now_unix(),
            };
            index.members.push(member.clone());
            index.save(&index_path)?;
            created.push(member);
        }
        Ok(created)
    }

    /// The members joined with their live smolvm state (if any).
    pub fn list(&self) -> Result<Vec<MemberStatus>> {
        let index = self.load_index()?;
        let machines = self.smolvm.ls().unwrap_or_default();
        Ok(index
            .members
            .into_iter()
            .map(|m| {
                let info = machines
                    .iter()
                    .find(|mi| mi.name == m.name.as_str())
                    .cloned();
                (m, info)
            })
            .collect())
    }

    /// Resolve a selector (full name or ordinal) to a member.
    pub fn resolve_member(&self, selector: &str) -> Result<Member> {
        let index = self.load_index()?;
        if let Some(m) = index.members.iter().find(|m| m.name.as_str() == selector) {
            return Ok(m.clone());
        }
        if let Ok(idx) = selector.parse::<u32>() {
            if let Some(m) = index.members.iter().find(|m| m.index == idx) {
                return Ok(m.clone());
            }
        }
        Err(Error::VmNotFound {
            name: selector.to_owned(),
            profile: self.profile.clone(),
        })
    }

    /// Resolve a set of selectors, or all members when `all` is set.
    pub fn resolve_targets(&self, selectors: &[String], all: bool) -> Result<Vec<Member>> {
        if all {
            return Ok(self.load_index()?.members);
        }
        selectors.iter().map(|s| self.resolve_member(s)).collect()
    }

    /// Start members and (re)start their guest sshd.
    pub fn start_members(&self, members: &[Member]) -> Result<()> {
        for m in members {
            self.smolvm.start(&m.name)?;
            self.ensure_sshd(&m.name);
        }
        Ok(())
    }

    /// Stop members.
    pub fn stop_members(&self, members: &[Member]) -> Result<()> {
        for m in members {
            self.smolvm.stop(&m.name)?;
        }
        Ok(())
    }

    /// Restart members (stop then start).
    pub fn restart_members(&self, members: &[Member]) -> Result<()> {
        for m in members {
            // Ignore stop failure (may already be stopped), then start.
            let _ = self.smolvm.stop(&m.name);
            self.smolvm.start(&m.name)?;
            self.ensure_sshd(&m.name);
        }
        Ok(())
    }

    /// Delete members and drop them from the index.
    pub fn remove_members(&self, members: &[Member], force: bool) -> Result<()> {
        let mut index = self.load_index()?;
        for m in members {
            if let Err(e) = self.smolvm.delete(&m.name, force) {
                tracing::warn!(vm = %m.name, error = %e, "delete failed; removing from index anyway");
            }
            index.members.retain(|x| x.name != m.name);
        }
        self.save_index(&index)?;
        Ok(())
    }

    /// Run an interactive `exec` session (shell / claude / arbitrary command).
    /// This is the guaranteed connect path: secrets are injected fresh and the
    /// command runs in the image filesystem.
    pub fn exec_session(
        &self,
        selector: &str,
        command: Vec<String>,
        interactive: bool,
    ) -> Result<std::process::ExitStatus> {
        let member = self.resolve_member(selector)?;
        self.ensure_running(&member)?;
        let (secret_env, plain_env) = self.session_env()?;
        let spec = ExecSpec {
            name: member.name,
            command,
            interactive,
            tty: interactive,
            workdir: None,
            env: plain_env,
            secret_env,
            detach: false,
        };
        self.smolvm.exec_interactive(&spec)
    }

    /// SSH into a member (optional real-SSH path). Requires the guest sshd and a
    /// forwarded port.
    pub fn ssh_session(
        &self,
        selector: &str,
        command: &[String],
    ) -> Result<std::process::ExitStatus> {
        let member = self.resolve_member(selector)?;
        if member.ssh_port == 0 {
            return Err(Error::ConfigValidate {
                reason: format!(
                    "{} has no forwarded SSH port; use `airlock shell`/`claude` instead",
                    member.name
                ),
            });
        }
        self.ensure_running(&member)?;
        self.ensure_sshd(&member.name);
        let (secret_env, plain_env) = self.session_env()?;
        let target = crate::ssh::SshTarget::loopback(
            &self.layout,
            &self.profile,
            &self.cfg.ssh.user,
            member.ssh_port,
        );
        crate::ssh::connect(&target, &secret_env, &plain_env, command)
    }

    /// Run `smolvm machine monitor` for a member in the foreground.
    pub fn monitor(&self, selector: &str) -> Result<std::process::ExitStatus> {
        let member = self.resolve_member(selector)?;
        self.smolvm.monitor(&member.name, "on-failure")
    }

    /// Copy a file between host and a member, translating a bare member selector
    /// in `machine:path` form to its real name.
    pub fn cp(&self, src: &str, dst: &str) -> Result<()> {
        let src = self.translate_cp_arg(src)?;
        let dst = self.translate_cp_arg(dst)?;
        self.smolvm.cp(&src, &dst)
    }

    // --- internals ---

    fn net_spec(&self) -> NetSpec {
        match self.cfg.network.policy {
            NetworkPolicy::Off => NetSpec::Off,
            NetworkPolicy::All => NetSpec::All,
            NetworkPolicy::Allow => NetSpec::Allow {
                hosts: self.cfg.network.allow_hosts.clone(),
                cidrs: self.cfg.network.allow_cidrs.clone(),
            },
        }
    }

    /// The rootfs source: a registry ref (checkpointable) when configured, else
    /// the fast local archive.
    fn launch_image(&self, archive: &Path) -> ImageSource {
        match self.cfg.registry_ref(&self.profile) {
            Some(reference) => ImageSource::Registry(reference),
            None => ImageSource::LocalArchive(archive.to_path_buf()),
        }
    }

    /// Egress policy for launch. Registry-backed images pull from inside the
    /// guest, so networking must be enabled even if the profile says `off`.
    fn launch_net(&self) -> NetSpec {
        let net = self.net_spec();
        if self.cfg.image.registry.is_some() && net == NetSpec::Off {
            NetSpec::All
        } else {
            net
        }
    }

    fn volumes(&self) -> Vec<String> {
        let mut v = self.cfg.mounts.volumes.clone();
        if let Some(km) = auth::kubeconfig_mount(&self.cfg) {
            v.push(km.volume);
        }
        v
    }

    /// The secret + plain env injected into every interactive session.
    fn session_env(&self) -> Result<SessionEnv> {
        let secret_env = auth::resolve_secret_env(&self.cfg, self.config_dir())?;
        let mut plain_env = Vec::new();
        if let Some(km) = auth::kubeconfig_mount(&self.cfg) {
            plain_env.push(("KUBECONFIG".to_owned(), km.kubeconfig_path));
        }
        Ok((secret_env, plain_env))
    }

    fn ensure_running(&self, member: &Member) -> Result<()> {
        if let Some(info) = self.smolvm.get(&member.name)? {
            if !info.is_running() {
                self.smolvm.start(&member.name)?;
                self.ensure_sshd(&member.name);
            }
        }
        Ok(())
    }

    /// Best-effort start of the guest sshd (only present in airlock-built images).
    fn ensure_sshd(&self, name: &VmName) {
        let mut spec = ExecSpec::new(name.clone(), vec!["/usr/local/bin/airlock-sshd".to_owned()]);
        spec.detach = true;
        if let Err(e) = self.smolvm.exec_capture(&spec) {
            tracing::debug!(vm = %name, error = %e, "guest sshd not started (ssh may be unavailable)");
        }
    }

    fn translate_cp_arg(&self, arg: &str) -> Result<String> {
        // `SELECTOR:PATH` where SELECTOR resolves to a member → `NAME:PATH`.
        if let Some((sel, path)) = arg.split_once(':') {
            if let Ok(member) = self.resolve_member(sel) {
                return Ok(format!("{}:{}", member.name, path));
            }
        }
        Ok(arg.to_owned())
    }
}

pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(name: &str, index: u32, port: u16) -> Member {
        Member {
            name: VmName::new(name).unwrap(),
            index,
            ssh_port: port,
            image_tag: "airlock/x:latest".to_owned(),
            created_at: 0,
        }
    }

    #[test]
    fn index_next_index_and_used_ports() {
        let mut index = FleetIndex::default();
        assert_eq!(index.next_index(), 1);
        index.members.push(member("web-01", 1, 2201));
        index.members.push(member("web-02", 2, 2202));
        assert_eq!(index.next_index(), 3);
        let used = index.used_ports();
        assert!(used.contains(&2201) && used.contains(&2202));
    }

    #[test]
    fn index_round_trips_through_disk() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("fleet.json");
        let mut index = FleetIndex::default();
        index.members.push(member("web-01", 1, 2201));
        index.save(&path)?;

        let loaded = FleetIndex::load(&path)?;
        assert_eq!(loaded.members.len(), 1);
        assert_eq!(loaded.members[0].name.as_str(), "web-01");
        assert_eq!(loaded.members[0].ssh_port, 2201);
        Ok(())
    }

    #[test]
    fn load_missing_index_is_empty() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let index = FleetIndex::load(&tmp.path().join("nope.json"))?;
        assert!(index.members.is_empty());
        Ok(())
    }

    #[test]
    fn zero_port_excluded_from_used() {
        let mut index = FleetIndex::default();
        index.members.push(member("web-01", 1, 0));
        assert!(index.used_ports().is_empty());
    }
}
