# Roadmap

## Shipped (v0.1)

- Orchestration over `smolvm`: build, up, ls, start/stop/restart/rm, monitor.
- Base image builder: Ubuntu + Claude Code + Go/Node/Rust/kubectl/gh + sshd,
  with pinned toolchain versions and baked non-secret Claude settings.
- Fleet from one profile with non-colliding names/ports/overlays.
- Launch-time secret injection: `.env`, GitHub token, kubeconfig, Anthropic creds.
- Connect: `ssh`, `shell`, `claude`, `exec`, `cp`.
- Restart with state via `stop`/`start` (smolvm overlay persistence).
- Restore any `.smolmachine` (`machine create --from`).
- Registry-backed profiles (`[image] registry`) that push on build and boot from
  the ref — the prerequisite for portable `pack --from-vm` checkpoints.

## Deferred (not in v0.1)

- **HTTP control plane.** `smolvm serve` exists; a long-running `airlockd` that
  supervises fleets across reboots is future work. Today airlock is one-shot CLI.
- **GPU passthrough profiles.** `smolvm --gpu` works; a curated CC-mode GPU profile
  for confidential inference is out of scope for the sandbox tool.
- **Remote hosts.** Everything is local `127.0.0.1` today. Driving `smolvm` on a
  remote builder over SSH is deferred.
- **Non-GitHub logins.** `login` currently covers GitHub via `gh`. GitLab / other
  forges are stubbed out of scope, not partially implemented.

## Verification status

E2E-verified on the dev host: build, up (+ fleet scaling with distinct ports), all
toolchains + Claude Code inside the VM, secret injection (`ANTHROPIC_API_KEY`,
`GH_TOKEN`), `exec`/`shell`/`ssh`/`cp`, stop/start state persistence, `restore` from
a real pack, teardown. Verified against smolvm's own `pack`/`create --from` that a
registry image can be packed and restored.

**Not** E2E-verified here: the full registry-backed `checkpoint` loop (build→push→
up-from-ref→`pack --from-vm`). The local registry container would not start in this
sandbox, and smolvm's puller runs inside the guest so it needs a registry reachable
from there. The code path is wired and unit-tested; validate it against a real
registry (e.g. ghcr.io) before relying on it.

## Off the table

- Reimplementing the microVM engine. We orchestrate `smolvm`; we do not fork it.
- Baking any secret material into images or checkpoints. Ever.
