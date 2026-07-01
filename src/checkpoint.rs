//! Checkpoint and restore fleet members via smolvm `pack`.
//!
//! A checkpoint is a `.smolmachine` artifact produced from a *stopped* VM
//! snapshot; restoring creates a new fleet member from it (~250ms boot, no image
//! pull). Secret refs are stripped from packs by smolvm, so a checkpoint never
//! carries live credentials — they are re-injected at connect time.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::fleet::{now_unix, Fleet, Member};
use crate::names::VmName;
use crate::ports;

/// Checkpoint a member to a `.smolmachine` file, stopping it first (pack requires
/// a stopped snapshot). Returns the payload path. The VM is left stopped.
///
/// smolvm can only pack VMs booted from a **registry** image, so this requires a
/// registry-backed profile (`[image] registry`). Local-archive profiles get a
/// clear error — their state already persists across `airlock stop`/`start`.
pub fn checkpoint(fleet: &Fleet, selector: &str, output: Option<PathBuf>) -> Result<PathBuf> {
    if fleet.cfg().image.registry.is_none() {
        return Err(Error::ConfigValidate {
            reason: "portable checkpoints need a registry-backed profile — set `[image] registry` \
                     in airlock.toml and `airlock build` again. (Local VM state already survives \
                     `airlock stop` then `airlock start`.)"
                .to_owned(),
        });
    }

    let member = fleet.resolve_member(selector)?;
    let payload = output.unwrap_or_else(|| {
        fleet
            .layout()
            .profile_data(fleet.profile())
            .join(format!("{}.smolmachine", member.name))
    });

    // `pack create -o BASE` writes BASE (stub) + BASE.smolmachine (payload). Strip
    // the extension so the payload lands exactly at the requested path.
    let stub_base = strip_smolmachine(&payload);

    tracing::info!(vm = %member.name, "stopping VM for checkpoint");
    let _ = fleet.smolvm().stop(&member.name);

    fleet.smolvm().pack_from_vm(
        &member.name,
        &stub_base,
        Some(fleet.cfg().resources.cpus),
        Some(fleet.cfg().resources.memory),
    )?;

    Ok(add_smolmachine(&stub_base))
}

/// Remove a trailing `.smolmachine` extension (the pack stub base name).
fn strip_smolmachine(path: &Path) -> PathBuf {
    if path.extension().is_some_and(|e| e == "smolmachine") {
        path.with_extension("")
    } else {
        path.to_path_buf()
    }
}

/// Append `.smolmachine` to a stub base to name the payload smolvm produces.
fn add_smolmachine(base: &Path) -> PathBuf {
    PathBuf::from(format!("{}.smolmachine", base.to_string_lossy()))
}

/// Restore a `.smolmachine` file as a new fleet member. Best-effort adds an SSH
/// port forward; if that is unsupported the member is still usable via the
/// exec-based connect commands.
pub fn restore(fleet: &Fleet, pack: &Path) -> Result<Member> {
    let mut index = fleet.load_index()?;
    let idx = index.next_index();
    let name = VmName::member(fleet.profile(), idx)?;

    let base = fleet.cfg().ssh.base_port;
    let used = index.used_ports();
    let preferred = base.saturating_add(u16::try_from(idx).unwrap_or(u16::MAX));
    let port = ports::find_free_port(preferred, &used)?;

    tracing::info!(vm = %name, pack = %pack.display(), "restoring from checkpoint");
    fleet.smolvm().create_from_pack(&name, pack)?;

    let forwarded = match fleet.smolvm().update(&name, &[format!("{port}:22")], &[]) {
        Ok(()) => port,
        Err(e) => {
            tracing::warn!(error = %e, "could not add SSH port to restored VM; use exec-based connect");
            0
        }
    };
    fleet.smolvm().start(&name)?;

    let member = Member {
        name,
        index: idx,
        ssh_port: forwarded,
        image_tag: format!(
            "restored:{}",
            pack.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        ),
        created_at: now_unix(),
    };
    index.members.push(member.clone());
    fleet.save_index(&index)?;
    Ok(member)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_and_add_smolmachine_are_inverse() {
        assert_eq!(
            strip_smolmachine(Path::new("/data/snap.smolmachine")),
            Path::new("/data/snap")
        );
        // No extension → unchanged.
        assert_eq!(
            strip_smolmachine(Path::new("/data/snap")),
            Path::new("/data/snap")
        );
        assert_eq!(
            add_smolmachine(Path::new("/data/snap")),
            Path::new("/data/snap.smolmachine")
        );
        // A requested `foo.smolmachine` round-trips back to itself as the payload.
        let requested = Path::new("/data/foo.smolmachine");
        assert_eq!(add_smolmachine(&strip_smolmachine(requested)), requested);
    }
}
