# 0002 — Host integration: shares, identity, home provisioning

Date: 2026-07-02
Status: accepted

## Context

The v0.1 sandbox was isolated but spartan: you logged in as **root/bash**, nothing
of your host came with you, and sharing a working directory meant hand-writing a
`[mounts] volumes` entry. Three asks drove this pass: (1) easily copy/bind host
dirs and host a set of repos by default, (2) log in as a normal user in fish with
passwordless sudo (no `su`), (3) bring dotfiles and `~/.claude` along seamlessly.

Several of these trade isolation for convenience, so the defaults were chosen with
the user (a security-conscious team): **isolation-first**.

## Decisions

### Identity: `dev` + fish via `setpriv`, not `su`

`smolvm exec` runs as root. To land the user as the non-root `dev` user in their
login shell **without** losing the launch-injected secrets, airlock bakes
`/usr/local/bin/airlock-login` which uses `setpriv --reuid=dev --regid=dev
--init-groups` to change uid/gid **without scrubbing the environment**, then fixes
`HOME`/`USER`/`SHELL` and execs the shell (or command).

`su`/`runuser`/`sudo -u` were rejected: each resets or filters the environment
(login-shell semantics, sudoers `env_delete`), which would drop injected secrets —
especially arbitrary `.env` var names that no fixed `--preserve-env` list covers.
`setpriv` is the one tool that changes privilege and touches nothing else. All
interactive paths (`shell`/`claude`/`exec`) go through it; SSH already logs in as
`dev` and `AcceptEnv *` lets forwarded secrets through.

### Sharing: copy by default, bind opt-in

`[workspace]` shares the project dir (and any `repos`) into the VM. The default
mode is **copy** — a one-time tar-stream snapshot into the VM overlay, so the
agent's edits never touch the host. **bind** (live `-v` mount) and **bind-ro** are
opt-in per profile or per-run (`--bind`/`--copy`). Copy respects an `exclude` list
(default `node_modules`, `target`, …) so snapshotting stays fast. This keeps the
microVM boundary intact by default while making "work on my repo live" one flag
away.

### Home provisioning: bake a safe subset by default

`~/.claude` and dotfiles default to **bake** — a secret-filtered subset baked into
the image (settings/agents/skills, `.gitconfig`, etc.), with a hard denylist
(`.ssh`, `.aws`, `.gnupg`, `.netrc`, `.git-credentials`, `.credentials.json`,
`*.key/pem`, `.env*`). Claude authenticates via the injected `ANTHROPIC_API_KEY`;
no OAuth credential ever enters the sandbox. **copy** mode (opt-in) copies the real
`~/.claude`/dotfiles into the VM (still a VM-local copy) for users who want a
fully logged-in box and accept credentials living inside their own sandbox.

## Consequences / notes

- Copy is a *snapshot at `up`*; later host edits are not reflected (that is the
  isolation guarantee). Use `bind` for live work, or re-`up`.
- The guest `/etc/hosts` is empty (smolvm), so `sudo` warned `unable to resolve
  host`. `airlock-login` and `airlock-sshd` add the hostname as root before use.
- Bind mounts and copy-mode `~/.claude` are deliberate isolation holes; they are
  opt-in and documented, never the default.
