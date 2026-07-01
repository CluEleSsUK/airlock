# 0001 — Architecture and trust model

Date: 2026-07-01
Status: accepted

## Context

We want to run Claude Code (and similar coding agents) in a sandbox that is
isolated from the host at a much stronger boundary than a container or a
`devcontainer`. An agent with shell access on the host can read `~/.ssh`, exfiltrate
`~/.aws`, `git push --force`, or `rm -rf` outside its project. A microVM gives each
agent its own kernel behind a hardware virtualization boundary (KVM), so a full
guest compromise still does not reach host memory or the host filesystem — while
still booting in well under a second.

`smolvm` (github.com/smol-machines/smolvm, v1.3.3) already provides that microVM
engine: libkrun + KVM, OCI images as the rootfs, `<200ms` boot, persistent named
machines, `pack`/`.smolmachine` checkpoints, SSH-agent forwarding, egress
filtering, port forwarding, volume mounts, launch-time secret injection, and a
JSON machine listing. Reimplementing any of that would be a mistake.

## Decision

**airlock is a thin orchestration layer over the `smolvm` binary.** It owns
_workflow and policy_ — building a base image with the agent toolchain baked in,
managing a fleet of non-colliding VMs from one profile, injecting host credentials
safely at launch, and connecting the user in — and delegates all virtualization to
`smolvm`. airlock never talks to libkrun/KVM directly; it constructs `smolvm` argv,
invokes it, and parses `machine ls --json`.

### Trust boundary

```
        HOST (trusted)                      GUEST microVM (untrusted)
  ┌───────────────────────────┐      ┌──────────────────────────────────┐
  │ airlock CLI               │      │ Ubuntu rootfs                     │
  │ ~/.ssh, ~/.aws, host FS   │  ✗   │ claude code + go/node/rust/kubectl│
  │ real GH token, kubeconfig │ ───► │ sshd, a `dev` user                │
  │ Anthropic credentials     │ KVM  │ ONLY the secrets we inject        │
  └───────────────────────────┘      └──────────────────────────────────┘
                    smolvm (libkrun) enforces the boundary
```

The guest is assumed hostile-capable (it runs an autonomous agent). The host must
never be reachable from it except through the narrow, explicit channels airlock
opens: a forwarded SSH port, an egress allowlist, and the specific secrets a
profile injects.

### Bake toolchains, inject secrets

- **Baked into the base OCI image** (non-secret, cacheable): Claude Code, the
  language toolchains (Go, Node/TypeScript, Rust), `kubectl`, `gh`, an OpenSSH
  server, a non-root `dev` user, and the user's **non-secret** Claude settings
  (`settings.json`, `CLAUDE.md`, `agents/`, `skills/`).
- **Injected at launch, never baked** (secret): the GitHub token, Anthropic
  credentials, the `.env` contents, and the kubeconfig. These ride `smolvm`'s
  `--secret-env` / `--secret-file` references, which resolve on the host at launch
  and are explicitly forbidden from `.smolmachine` packs and the HTTP API. A
  checkpoint of a running VM therefore cannot leak them.

This split is a hard rule: a secret in an image layer is a secret in every
container registry, checkpoint, and `docker history` forever.

### Fleet isolation (no stepping on each other)

One profile can launch N VMs. Each member gets:

- a unique machine name `"<profile>-NN"`, so `smolvm` keeps a separate persistent
  overlay/storage disk per VM;
- a unique host SSH port, allocated from a deterministic base and probed for
  freeness before use;
- its own entry in a per-profile fleet index (`fleet.json`) recording name → port,
  image, and creation order.

Because every member is a distinct `smolvm` machine with its own disk, there is no
shared mutable guest state to corrupt. The only host-side shared state is the fleet
index, written atomically.

### Connecting in

Primary path is **real SSH**: the base image runs `sshd`; airlock generates a
per-profile ed25519 keypair (via the host `ssh-keygen`), bakes the public key as
the `dev` user's `authorized_keys`, and forwards `-p <hostport>:22`. `airlock ssh`
/ `airlock claude` then run the host `ssh` client against `127.0.0.1:<hostport>`.
This gives a real terminal, `scp`, and editor-remote support. A fallback,
`airlock shell`, uses `smolvm machine shell` over the vsock channel and needs no
sshd — useful if port forwarding is unavailable.

### Egress policy

Networking is off by default in `smolvm`. A profile chooses one of:
`off` | `all` | `allow { hosts, cidrs }`. The scaffolded default is `all` for
usability, but the profile documents an allowlist (Anthropic + GitHub + package
registries) for high-security use. This keeps the security-first posture available
without forcing every user to enumerate hosts on day one.

## Alternatives rejected

- **Reimplement on libkrun/Firecracker directly.** Huge surface, and `smolvm`
  already solved boot speed, OCI rootfs, checkpoints, and secret refs. Rejected.
- **devcontainers / plain Docker.** Shared host kernel; a container escape or a
  mounted socket reaches the host. That is exactly the boundary we are trying to
  strengthen. Rejected.
- **Bake secrets into the image for convenience.** Leaks into every layer,
  checkpoint, and registry. Rejected; injection at launch instead.

## DEVIATION — synchronous, not `#[tokio::main]`

The rust-engineer skill defaults binaries to `#[tokio::main]`. airlock is
deliberately synchronous. It is orchestration glue whose every unit of work is a
blocking subprocess (`smolvm …`, `docker build`, `ssh`) or an interactive terminal
that inherits stdio / replaces the process image. Async buys no concurrency that
matters here; the one place parallelism helps — bringing up N VMs at once — is
handled with `std::thread::scope`. Adding a Tokio runtime and `async` colouring
across the codebase would be a sophisticated solution to a simple problem. Recorded
here because it is a conscious departure from the skill.
