# Pitfalls

Gotchas for humans and agents working on airlock. Cite file:line.

- **Secrets must never enter the base image.** `image.rs` stages a build context;
  anything written there lands in an OCI layer forever. Credentials, `.env`
  values, and kubeconfig are injected at launch via `smolvm --secret-env` /
  `--secret-file` only. If you find yourself `COPY`-ing a token in the Dockerfile,
  stop.

- **`smolvm` is the wrapper script, not `smolvm-bin`.** Always invoke `smolvm`
  (it sets `LD_LIBRARY_PATH` for the bundled libkrun). Calling `smolvm-bin`
  directly fails with a library-not-found error. `smolvm.rs` locates the binary
  by name on `PATH`.

- **`machine run` is ephemeral; `machine exec` persists.** A fleet member is a
  persistent `machine create` + `start`. Never model a long-lived VM with
  `machine run` — all its state is discarded on exit.

- **Port-forward (`-p`) is set at create/start, not after.** To change a member's
  SSH port you must `machine update` a *stopped* machine (or recreate). The fleet
  index is the source of truth for which host port maps to which VM.

- **`.smolmachine` packs reject secret refs by design.** `pack create` on a VM
  that had `--secret-env` refs strips them; restoring re-injects from the profile.
  Do not expect a checkpoint to carry live credentials — that is intentional.

- **Egress is off by default in smolvm.** If Claude Code inside the VM cannot reach
  `api.anthropic.com`, the profile's `network` policy is probably `off`. Claude
  needs `all` or an allowlist including the Anthropic API.

- **`pack create --from-vm` only works on registry-sourced VMs.** VMs booted from a
  local `docker save` archive (airlock's default fast path) are flattened on boot
  and have no manifest to re-pull, so smolvm refuses to pack them. `checkpoint.rs`
  guards this: it requires `[image] registry`. Local state still persists via
  stop/start. See `docs/decisions/0001` and `image.rs`/`fleet.rs` for the
  registry-backed path.

- **`smolvm pack create -o X` writes two files.** `X` is the stub *binary*; the
  restorable payload is `X.smolmachine`. `machine create --from` needs the payload
  (magic `SMOLPACK`), not the stub. `checkpoint::strip_smolmachine`/`add_smolmachine`
  normalise the requested path so the payload lands where the user asked.

- **smolvm's image puller runs inside the agent VM, not on the host.** A registry on
  the host's `localhost:5000` is not reachable as `localhost` from the guest, and
  registry pulls require `--net`. Use a registry reachable from the guest (a real
  one like ghcr.io, or the host gateway address) for registry-backed profiles.
