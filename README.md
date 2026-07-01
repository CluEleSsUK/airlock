# airlock

Sandbox **Claude Code** (and other coding agents) in fast, disposable **microVMs** —
a stronger alternative to devcontainers, built on
[`smolvm`](https://github.com/smol-machines/smolvm).

Each sandbox is a real VM with its own kernel behind a hardware virtualization
boundary (KVM). An agent that goes rogue inside the box cannot read your
`~/.ssh`, exfiltrate `~/.aws`, `git push --force`, or `rm -rf` your host — yet the
VM boots in well under a second and comes preloaded with Claude Code and your
toolchains.

```
airlock init        # scaffold airlock.toml
airlock build       # bake an image: Ubuntu + Claude Code + go/node/rust/kubectl/gh
airlock up -n 3     # boot a fleet of 3 isolated sandboxes
airlock claude 1    # drop into Claude Code inside sandbox #1
```

## Why not a devcontainer?

A container shares the host kernel. A container escape, a leaked Docker socket, or
a careless bind-mount reaches the host. For an **autonomous agent** with shell
access that is the wrong trust boundary. airlock gives each agent its own kernel
under KVM: the host is treated as something the guest must never reach except
through the narrow channels airlock opens (a forwarded SSH port, an egress
allowlist, and exactly the secrets a profile injects).

See [`docs/decisions/0001-architecture-and-trust-model.md`](docs/decisions/0001-architecture-and-trust-model.md).

## How it works

airlock is a thin orchestration layer over the `smolvm` microVM runtime. It owns
workflow and policy; smolvm owns virtualization (libkrun + KVM, OCI images as the
rootfs, `<200ms` boot, checkpoints, secret refs).

- **Toolchains are baked** into a base OCI image (Ubuntu + Claude Code + Go,
  Node/TypeScript, Rust, `kubectl`, `gh`, an sshd, and your non-secret Claude
  settings).
- **Secrets are injected at launch**, never baked: your `.env`, GitHub token,
  Anthropic API key, and kubeconfig ride smolvm's launch-time secret refs / a
  read-only mount, so they never land in an image layer or a checkpoint.
- **A fleet** of many VMs comes from one profile, each with a unique name,
  a unique forwarded SSH port, and its own overlay disk — they never step on
  each other.

## Prerequisites

| Need | Why |
|---|---|
| Linux with `/dev/kvm` and your user in the `kvm` group | the hypervisor |
| [`smolvm`](https://github.com/smol-machines/smolvm) on `PATH` | the microVM engine |
| `docker` or `podman` | building the base image |
| `ssh` + `ssh-keygen` | connecting in / per-profile keys |
| `gh` (optional) | `airlock login github` + `GH_TOKEN` injection |

Run `airlock status` to check all of these at once.

## Install

```bash
cargo install --path .
# or
cargo build --release && cp target/release/airlock ~/.local/bin/
```

Install smolvm (pinned release, checksum-verified) if you don't have it:

```bash
VER=1.3.3
gh release download "v$VER" --repo smol-machines/smolvm --dir /tmp/smolvm
cd /tmp/smolvm && sha256sum -c checksums.sha256
tar xzf "smolvm-$VER-linux-x86_64.tar.gz" -C ~/.local/opt
ln -sf ~/.local/opt/smolvm-$VER-linux-x86_64/smolvm ~/.local/bin/smolvm
```

## Quickstart

```bash
cd my-project
airlock init                 # writes airlock.toml
airlock login github         # optional: gh device-flow login
export ANTHROPIC_API_KEY=... # optional: injected as-is into the sandbox
airlock build                # bake the base image (minutes, first time only)
airlock up -n 2              # create + start 2 sandboxes
airlock ls                   # see the fleet
airlock claude 1             # use Claude Code in sandbox #1
airlock shell 2              # or a plain shell in sandbox #2
airlock stop --all           # stop everything (per-VM state persists)
airlock start --all          # resume, state intact
airlock restore snap.smolmachine           # boot any .smolmachine as a new member
airlock rm --all             # tear the fleet down
```

## Persistence & checkpoints

- **Restart with state.** `airlock stop` then `airlock start` preserves each VM's
  filesystem (smolvm overlay) — the everyday "pause and resume my agent" path. No
  registry needed.
- **Portable checkpoints.** `airlock checkpoint <vm>` packs a VM into a
  `.smolmachine` you can `airlock restore` anywhere. smolvm can only pack VMs
  booted from a **registry** image, so this needs a registry-backed profile — set
  `[image] registry = "ghcr.io/you"` and `airlock build` (which then pushes). In
  the default fast local-archive mode, `checkpoint` explains this and points you at
  stop/start. `airlock restore` works on any valid `.smolmachine` regardless.

## Connecting in

| Command | Path | Notes |
|---|---|---|
| `airlock claude <vm>` | `smolvm exec` | drops straight into Claude Code — **always works** |
| `airlock shell <vm>` | `smolvm exec` | interactive login shell |
| `airlock exec <vm> -- <cmd>` | `smolvm exec` | run one command |
| `airlock ssh <vm> [-- cmd]` | real sshd | a genuine SSH endpoint (scp, editors); needs networking |

`<vm>` is a full name (`airlock-demo-01`) or an ordinal (`1`). The exec-based paths
are the guaranteed connect method (secrets are injected fresh into the session and
never persisted). SSH is the optional "real endpoint" — it forwards your secrets
with `SendEnv` so they reach the session without touching guest disk.

## Configuration (`airlock.toml`)

Every field has a default; an empty file is valid. Key sections:

```toml
name = "my-project"

[image]
base = "ubuntu:24.04"
[image.toolchains]        # all default on
claude = true
rust = true; go = true; node = true; kubectl = true; gh = true
[image.versions]          # pin toolchains (optional)
go = "1.26.0"; node = "22"; kubectl = "1.31.0"; rust = "stable"

[resources]
cpus = 4
memory = 8192             # MiB (elastic via balloon)

[network]
policy = "all"            # "off" | "all" | "allow"
# allow_hosts = ["api.anthropic.com", "github.com", "registry.npmjs.org"]

[secrets]                 # injected at launch, never baked
env_file = ".env"
github = true                                # GH_TOKEN from `gh auth token`
anthropic_api_key_env = "ANTHROPIC_API_KEY"  # host env var → guest
# kubeconfig = "~/.kube/config"              # read-only mount into the guest

[ssh]
base_port = 2200          # member NN forwards host port base_port+NN → guest :22
user = "dev"

[mounts]
volumes = ["./:/home/dev/project"]   # HOST:GUEST[:ro]
```

## Fleet model

`airlock up -n N` appends `N` new members named `<profile>-NN`. Each gets a
distinct, probed-free host SSH port and its own smolvm overlay. State lives in a
per-profile `fleet.json`; the built image and SSH keypair live under the XDG data
dir. Running several profiles at once? Give each a different `ssh.base_port`.

## Security model (short version)

- Secrets never enter an image layer or a `.smolmachine` checkpoint — enforced by
  a build-context denylist and by smolvm stripping secret refs from packs.
- The guest is treated as untrusted. Its only routes to the host are the SSH port
  airlock forwards and the egress the profile's `network` policy allows.
- `git`/`ssh` inside the box can use your host SSH agent (`--ssh-agent`) so private
  keys sign challenges without ever entering the guest.

Full threat model and rejected alternatives: [`docs/`](docs/).

## Development

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## License

MIT OR Apache-2.0.
