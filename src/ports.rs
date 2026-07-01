//! Collision-free host port allocation for fleet members.
//!
//! Each running VM forwards a distinct host TCP port to the guest sshd. Ports are
//! allocated by walking upward from a per-profile base, skipping ports that are
//! already bound on the host or already assigned to another fleet member.
//!
//! Note the inherent TOCTOU: a port reported free here can be taken by another
//! process before smolvm binds it. The `exclude` set eliminates collisions
//! *between airlock fleet members*, which is the guarantee we actually promise;
//! cross-process races are a best-effort probe. See `docs/pitfalls.md`.

use std::collections::HashSet;
use std::net::TcpListener;

use crate::error::{Error, Result};

/// Default first host port for SSH forwards.
pub const DEFAULT_SSH_BASE_PORT: u16 = 2200;

/// How many candidate ports to probe before giving up.
const MAX_PROBE: u16 = 1000;

/// Returns `true` if a TCP listener can currently bind `127.0.0.1:port`.
pub fn is_port_free(port: u16) -> bool {
    if port == 0 {
        return false;
    }
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Find the first free host port at or above `preferred`, skipping `exclude`.
pub fn find_free_port(preferred: u16, exclude: &HashSet<u16>) -> Result<u16> {
    let start = preferred.max(1);
    let mut port = start;
    for _ in 0..MAX_PROBE {
        if !exclude.contains(&port) && is_port_free(port) {
            return Ok(port);
        }
        match port.checked_add(1) {
            Some(next) => port = next,
            None => break,
        }
    }
    Err(Error::NoFreePort {
        start,
        end: start.saturating_add(MAX_PROBE),
    })
}

/// Allocate `count` distinct free host ports starting near `base`, never reusing
/// a port in `already_used` (typically the ports of existing fleet members).
pub fn allocate_ports(base: u16, count: usize, already_used: &HashSet<u16>) -> Result<Vec<u16>> {
    let mut exclude = already_used.clone();
    let mut out = Vec::with_capacity(count);
    let mut next = base.max(1);
    for _ in 0..count {
        let port = find_free_port(next, &exclude)?;
        exclude.insert(port);
        out.push(port);
        next = port.saturating_add(1);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_is_never_free() {
        assert!(!is_port_free(0));
    }

    #[test]
    fn find_skips_a_bound_port() -> anyhow::Result<()> {
        // Bind an ephemeral port and confirm the allocator walks past it.
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let taken = listener.local_addr()?.port();
        let empty = HashSet::new();
        let found = find_free_port(taken, &empty)?;
        assert_ne!(found, taken, "allocator returned a bound port");
        assert!(found > taken);
        Ok(())
    }

    #[test]
    fn find_respects_exclude_set() -> anyhow::Result<()> {
        // Use a free port, then exclude it, and confirm we get a different one.
        let free = {
            let l = TcpListener::bind(("127.0.0.1", 0))?;
            l.local_addr()?.port()
            // listener dropped here → port is free again
        };
        let mut exclude = HashSet::new();
        exclude.insert(free);
        let found = find_free_port(free, &exclude)?;
        assert_ne!(found, free);
        Ok(())
    }

    #[test]
    fn allocate_returns_distinct_ports() -> anyhow::Result<()> {
        let used = HashSet::new();
        let ports = allocate_ports(20_000, 5, &used)?;
        assert_eq!(ports.len(), 5);
        let unique: HashSet<u16> = ports.iter().copied().collect();
        assert_eq!(unique.len(), 5, "ports must be distinct: {ports:?}");
        Ok(())
    }

    #[test]
    fn allocate_avoids_already_used() -> anyhow::Result<()> {
        let mut used = HashSet::new();
        used.insert(20_000u16);
        used.insert(20_001u16);
        let ports = allocate_ports(20_000, 3, &used)?;
        assert!(ports.iter().all(|p| !used.contains(p)));
        Ok(())
    }
}
