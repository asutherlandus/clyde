# Installing Clyde

Setup for a single-user Linux host: the control plane, the credential broker, and
both sandbox backends.

Read this alongside [`clyde doctor`](#step-5-check-the-host), which is the
authority on whether a host has what it needs. Everything here is background for
what `doctor` reports and remedies for what it refuses.

**Before you start, two things are true and worth knowing up front:**

- **Bubblewrap is the working backend.** It runs every MVP task except dependency
  resolution. Follow steps 1–7 and you have a working install.
- **Firecracker cannot be brought up yet.** Its host prerequisites are real and
  documented in [step 8](#step-8-firecracker), but three pieces of the guest side
  are unimplemented, so `rust.resolve-deps` stays refused on every host. The
  section says exactly what is missing rather than offering a recipe that stops
  halfway.

## Contents

1. [Requirements](#requirements)
2. [Step 1: get the source and build](#step-1-get-the-source-and-build)
3. [Step 2: materialise the runtime roots](#step-2-materialise-the-runtime-roots)
4. [Step 3: user namespaces and AppArmor](#step-3-user-namespaces-and-apparmor)
5. [Step 4: cgroup v2 delegation](#step-4-cgroup-v2-delegation)
6. [Step 5: check the host](#step-5-check-the-host)
7. [Step 6: host configuration](#step-6-host-configuration)
8. [Step 7: run the daemon and the broker](#step-7-run-the-daemon-and-the-broker)
9. [Step 8: Firecracker](#step-8-firecracker)
10. [Verifying the install](#verifying-the-install)
11. [Troubleshooting](#troubleshooting)

## Requirements

| | |
|---|---|
| OS | Linux. Clyde is Linux-first and uses user namespaces, cgroup v2, and seccomp directly. |
| Kernel | cgroup v2 unified hierarchy; unprivileged user namespaces permitted. |
| Nix | With flakes enabled. The flake is the source of truth for tooling (D6, and [AGENTS.md](AGENTS.md)); nothing here assumes a host-global toolchain. |
| Filesystem | Hardlink support on the state directory, or snapshots fall back to copying. |
| For Firecracker only | `/dev/kvm`, and membership of the `kvm` group. |

Everything else — the Rust toolchain, bubblewrap, sqlite, git — comes from the
devShell. Do not install them globally; a host-global `cargo` on `PATH` is
exactly what D6 keeps out of the workspace environment.

## Step 1: get the source and build

```sh
git clone <repository> clyde
cd clyde
nix develop                       # the only supported entry point
cargo build --release
```

The four binaries land in `target/release/`:

| Binary | Role |
|---|---|
| `clyded` | The control plane. Two sockets, missions, leases, tasks, egress proxy. |
| `clyde` | CLI and terminal client. Operator and actor surfaces. |
| `clyde-brokerd` | The credential broker. Separate process, holds the credential. |
| `clyde-forward` | The in-sandbox egress forwarder. Bind-mounted into sandboxes. |

The flake does not package these yet — `packages` exposes the runtime roots
only — so install them by copying:

```sh
sudo install -Dm755 target/release/clyded        /usr/local/bin/clyded
sudo install -Dm755 target/release/clyde         /usr/local/bin/clyde
sudo install -Dm755 target/release/clyde-brokerd /usr/local/bin/clyde-brokerd
sudo install -Dm755 target/release/clyde-forward /usr/local/libexec/clyde-forward
```

`clyde-forward` goes to `libexec` because it is never invoked by a human: clyded
bind-mounts it read-only into a sandbox that has an egress profile.

## Step 2: materialise the runtime roots

A runtime root is a nix closure, not an image (D6). Three exist, and a task
executes against exactly one:

| Root | Contents | Used by |
|---|---|---|
| `workspace` | Shell, coreutils, ripgrep, fd, sd, jq, yq, ast-grep, python3, diff/patch | The agent's editing environment. **No build toolchain** — that is asserted by `nix flake check`. |
| `rust` | The Rust toolchain | `rust.check`, `rust.test.unit` |
| `fetch` | Cargo and what it needs to resolve dependencies | `rust.resolve-deps` |

Build them and keep the store paths — they are what host configuration names:

```sh
nix build .#workspace --print-out-paths --no-link
nix build .#rust      --print-out-paths --no-link
nix build .#fetch     --print-out-paths --no-link
```

Add GC roots so a `nix-collect-garbage` cannot delete a root out from under a
configured daemon:

```sh
sudo mkdir -p /nix/var/nix/gcroots/clyde
for root in workspace rust fetch; do
  sudo ln -sfn "$(nix build .#$root --print-out-paths --no-link)" \
    "/nix/var/nix/gcroots/clyde/$root"
done
```

Then configuration can name the stable symlinks instead of raw store paths.

Verify the workspace root has no build toolchain in it — this is the assertion
that makes typed tasks the only path to project execution:

```sh
nix flake check     # includes the runtime-root closure assertion
```

## Step 3: user namespaces and AppArmor

Bubblewrap needs to create an unprivileged user namespace. **On Ubuntu 23.10 and
later this is denied by default**, and it is the single most likely thing to block
a first run:

```sh
sysctl kernel.apparmor_restrict_unprivileged_userns    # 1 means restricted
unshare --user --map-root-user true                    # fails if blocked
```

The restriction applies to any binary without a permitting AppArmor profile, and
a nix-store `bwrap` has none.

### The narrow fix: a profile for the store path

Store paths are content-addressed and change whenever the toolchain moves, so the
profile needs a glob:

```
# /etc/apparmor.d/nix-bwrap
abi <abi/4.0>,
include <tunables/global>

profile nix-bwrap /nix/store/*/bin/bwrap flags=(unconfined) {
  userns,
  include if exists <local/nix-bwrap>
}
```

```sh
sudo apparmor_parser -r /etc/apparmor.d/nix-bwrap
unshare --user --map-root-user true && echo "user namespaces work"
```

It lives in `/etc`, so it survives a reboot.

Check first whether your distribution already ships a profile for its own
`bwrap` — adapting that one is lower-risk than the sketch above:

```sh
ls /etc/apparmor.d/ | grep -iE 'bwrap|userns'
```

Note the trade-off honestly: the glob permits `userns create` for any
`bwrap`-shaped path in the nix store, not just the one you configured. Narrowing
it to a single store path means updating the profile on every toolchain bump.

### The blunt alternative

```sh
echo 'kernel.apparmor_restrict_unprivileged_userns=0' \
  | sudo tee /etc/sysctl.d/60-clyde-userns.conf
sudo sysctl --system
```

This re-enables unprivileged user namespaces for **every** binary on the machine,
which is precisely the attack surface the restriction exists to close. For a
project whose thesis is least privilege, prefer the profile.

### Pin the binary the profile names

So a `PATH` change cannot silently select a different `bwrap` than the one the
profile permits:

```sh
nix develop --command command -v bwrap
```

Put that path in `sandbox.bwrap` (step 6).

## Step 4: cgroup v2 delegation

Build and test tasks (T2 and above) are **refused outright** without a delegated
cgroup v2 subtree. There is no degraded mode and no opt-in flag (D22), so treat
this as a hard prerequisite rather than a tuning step.

On a systemd machine it usually works already, because `user@$UID.service` is
delegated by default:

```sh
systemctl --user show user@$(id -u).service -p Delegate     # want Delegate=yes
test -w /sys/fs/cgroup/user.slice/user-$(id -u).slice/user@$(id -u).service/cgroup.subtree_control \
  && echo delegated
```

The controllers Clyde needs are `memory`, `pids`, and `cpu`:

```sh
cat /sys/fs/cgroup/cgroup.controllers
```

Two cases where delegation is missing:

- **`clyded` runs outside a login session** — a system service, or `ssh` without
  one. Fix with `loginctl enable-linger $USER` and run it under the user manager.
- **You are in a container.** Nothing inside the container can lift this; see
  [Troubleshooting](#troubleshooting).

## Step 5: check the host

`clyde doctor` works without a running daemon — bring-up is exactly when the
daemon is not running:

```sh
clyde doctor
```

It reports each prerequisite as available, **blocked** (present but unusable, with
a remedy), or **unavailable** (absent). The distinction matters because the
remedies differ, and `doctor` selects the remedy from what encloses it — a
container boundary is reported as one rather than as a session misconfiguration
(R10).

What you want to see:

```
ok   user namespaces      unprivileged user namespaces permitted (max 15000)
ok   cgroup v2            cgroup v2 with controllers: …, cpu, …, memory, pids, …
ok   cgroup delegation    cgroup v2 delegated at /sys/fs/cgroup/user.slice/…
ok   bubblewrap           bubblewrap at /nix/store/…/bin/bwrap
ok   nix                  nix at …
ok   hardlinks            hardlinks supported on …

ok    workspace environment
ok    build and test tasks
NO    dependency resolution
NO    brokered publishing
```

`ok` on the first two summary lines is the meaningful signal: it means clyded will
admit a bubblewrap-backed task rather than refuse it on host capability. `kvm`
and `firecracker` staying absent is expected — see
[step 8](#step-8-firecracker) — and `brokered publishing` turns `ok` once
`clyde-brokerd` is running ([step 7](#step-7-run-the-daemon-and-the-broker)).

## Step 6: host configuration

Layering is defaults → host → user → repository, and **a repository may only
narrow** (D14). Anything security-relevant — which sandbox binary runs, which
runtime root a task gets, push remotes, broker credentials — is host or user
configuration only and is rejected with a diagnostic if a repository tries to set
it (D20).

| Layer | Path |
|---|---|
| Host | `/etc/clyde/config.toml`, or `$CLYDE_HOST_CONFIG` |
| User | `$XDG_CONFIG_HOME/clyde/config.toml`, else `~/.config/clyde/config.toml` |
| Repository | in the workspace, loaded per workspace as untrusted content |

A missing file is fine. An unreadable or malformed one is an error, and unknown
keys are rejected rather than ignored — a typo in a policy file must not read as
a restriction that silently is not there.

```toml
# /etc/clyde/config.toml

[sandbox]
# Pin the exact bwrap the AppArmor profile permits.
bwrap = "/nix/store/…-bubblewrap-0.11.2/bin/bwrap"
# The GC-rooted symlinks from step 2.
runtime_root_workspace = "/nix/var/nix/gcroots/clyde/workspace"
runtime_root_rust      = "/nix/var/nix/gcroots/clyde/rust"
runtime_root_fetch     = "/nix/var/nix/gcroots/clyde/fetch"
# Bind-mounted read-only into any sandbox with an egress profile.
forwarder = "/usr/local/libexec/clyde-forward"

[agent]
# Resolved from inside the workspace runtime root, never from PATH (D6).
command = "claude"
args = []

[egress]
# Hosts the `model-api` profile may reach. This is the only profile for which
# Clyde terminates TLS, so it is deliberately explicit (D7 amendment, D11).
model_api_hosts = ["api.anthropic.com"]
model_api_auth_header = "authorization"

[push]
# Without these, every push is refused before a human is even asked.
remotes = ["origin"]
branch_patterns = ["feature/*", "clyde/*"]
# Refused whatever the allowlist says.
protected_branch_patterns = ["main", "master", "release/*"]

[broker]
socket = "/run/clyde/brokerd.sock"
allow_ssh_agent = false
ssh_key = "/home/you/.ssh/id_ed25519_clyde"

[broker.remotes]
# The broker resolves a remote from here and never from the workspace
# repository's own configuration, which is attacker-controlled content.
origin = "git@github.com:you/example.git"

[limits.build]
max_wall_clock = "20m"
max_memory_bytes = 8589934592
max_cpu_percent = 400

[mission.defaults]
max_expiry = "4h"
max_task_runs = 200

[fetch]
# Nothing is pre-approved by default: the first fetch reaches a human.
pre_approve_additions = false
pre_approve_version_changes = false
```

Validate it before relying on it. This loads the configuration, probes the host,
and exits:

```sh
clyded --check
```

Malformed configuration fails here rather than at the first task.

### State directory

Defaults to `$CLYDE_STATE_DIR`, else `$XDG_DATA_HOME/clyde`, else
`~/.local/share/clyde`, else `/var/lib/clyde`. Override with `--state-dir`.

```
db.sqlite          the store
blobs/  snapshots/ content-addressed snapshot store
deps/              dependency bundles
missions/          per-mission caches
logs/tasks/<id>/   per-task logs
ca/                the Clyde CA, for the model-api profile only
run/clyded.sock        actor socket — may be bind-mounted into a sandbox
run/clyded-admin.sock  human socket — never mounted into any sandbox
run/brokerd.sock       the broker
vm/                Firecracker guest images (see step 8)
```

The two sockets are the mechanism, not a policy: because the admin socket is
never mounted into a sandbox, an agent cannot approve its own request (D2).

Keep the state directory path short. Unix socket paths have a hard 107-byte
limit, and a long `--state-dir` will fail at bind with a message naming it.

## Step 7: run the daemon and the broker

```sh
clyded --log info
clyde-brokerd --log info    # only needed for git.push
```

Under systemd, as user units — this also gives you the delegated cgroup from
step 4:

```ini
# ~/.config/systemd/user/clyded.service
[Unit]
Description=Clyde control plane

[Service]
ExecStart=/usr/local/bin/clyded --log info
Restart=on-failure

[Install]
WantedBy=default.target
```

```ini
# ~/.config/systemd/user/clyde-brokerd.service
[Unit]
Description=Clyde credential broker
After=clyded.service

[Service]
ExecStart=/usr/local/bin/clyde-brokerd --log info
Restart=on-failure

[Install]
WantedBy=default.target
```

```sh
systemctl --user daemon-reload
systemctl --user enable --now clyded clyde-brokerd
loginctl enable-linger $USER     # so they survive logout
```

The broker is a separate process on purpose: it holds the credential, validates
every request on its own account, and reads the approval record itself rather
than trusting the caller.

## Step 8: Firecracker

Only `rust.resolve-deps` needs this. It is the one MVP task that executes with
network reachable, so it requires microVM isolation and is refused rather than
downgraded on a host that cannot provide it (D9). Everything else runs under
bubblewrap.

### Host prerequisites

These are real, and you can complete them today:

```sh
ls -l /dev/kvm                       # present?
lsmod | grep kvm                     # kvm_intel or kvm_amd loaded?
grep -oE '\b(vmx|svm)\b' /proc/cpuinfo | head -1   # does the CPU support it?

sudo usermod -aG kvm $USER           # then log out and back in
```

If `/dev/kvm` is missing on a CPU that reports `vmx` or `svm`, `clyde doctor`
says so explicitly: that is a device that was not exposed, not hardware that
cannot virtualise. Inside a VM, enable nested virtualisation on the hypervisor.

Firecracker is not in the devShell, so install it and name it:

```toml
[sandbox]
firecracker = "/usr/local/bin/firecracker"
```

### What the backend expects

The daemon looks for guest images under the state directory:

```
<state-dir>/vm/vmlinux              uncompressed guest kernel
<state-dir>/vm/rootfs/workspace.img
<state-dir>/vm/rootfs/rust.img      one per runtime root, named <kind>.img
<state-dir>/vm/rootfs/fetch.img
```

The rootfs is attached read-only as the root device, each mount with a host
source becomes a drive, and a vsock device exists **only** when the egress
profile permits egress — under profile `none` the guest has no channel of any
kind to the host, and no network device in any case.

### What is missing

Three pieces, and all three are guest-side:

1. **No image build.** Nothing in the flake builds `vmlinux` or the rootfs
   images from the runtime-root closures. `nix flake check` covers the closures;
   turning one into a bootable ext4 image is unwritten.
2. **No job contract.** `FirecrackerBackend::start` boots the VM with the drives
   and `init=/init`, and never conveys the task's `argv`, environment, or working
   directory to the guest — nor is there a path for an exit status to come back
   other than the VM's own exit code. A guest `/init` would need a defined way to
   learn what to run; that interface does not exist yet.
3. **No vsock in the forwarder.** `clyde-forward` speaks to a Unix socket. The
   guest side of the vsock bridge described in the
   [network egress model](docs/network-egress-model.md#firecracker-phase-2b) is
   not implemented.

What *is* implemented and tested: backend selection by policy with no silent
downgrade, the VM configuration as a pure function of the sandbox spec, the
read-only-root and no-network-device properties as type-level guarantees, and the
refusal path when KVM is absent.

So supplying `/dev/kvm` and a `firecracker` binary will not make
`rust.resolve-deps` run. It stays refused — which is the correct behaviour, and
why the refusal is tested directly. Closing those three gaps is the next
deliverable after the MVP.

## Verifying the install

```sh
clyde doctor                                  # ok for workspace + build tasks
clyde workspace register /path/to/rust/project
clyde workspace list
```

Propose a mission. This prints the **exact envelope** that would be issued,
including the caveats an approval has to state rather than imply:

```sh
clyde mission create \
  --workspace <workspace-id> \
  --edit crates/thing \
  --task rust.check \
  --expires-in 2h \
  "tidy up the thing crate"
```

Approve it, then watch and close:

```sh
clyde mission approve <mission-id>
clyde mission status <mission-id>
clyde audit show --limit 50
clyde audit verify                # the hash chain, including its recorded head
clyde mission review <mission-id> # end-to-end review; verifies the chain too
clyde mission close <mission-id>
```

Every command takes `--json` for a stable machine-readable shape, and `clyde tui`
gives the same surfaces interactively.

Two things to expect:

- **`clyde task run` is refused on the host.** Tasks are the actor surface; a
  human gets the operator commands, and the message says so. This is the same
  separation that makes self-approval impossible (D2).
- **A task only runs once an agent is hosted.** `mission approve` starts the
  agent, and `agent.command` is resolved from inside the workspace runtime root
  rather than from `PATH` (D6) — so the agent binary has to be part of
  `runtimeRoots.workspace` in [`nix/runtime-roots.nix`](nix/runtime-roots.nix).
  Until it is added there, the operator surface is what runs on a real host, and
  the task pipeline is covered by the test suite.

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `doctor`: user namespaces BLOCK, AppArmor | Ubuntu 23.10+ default | [Step 3](#step-3-user-namespaces-and-apparmor) |
| `doctor`: cgroup delegation BLOCK, `subtree_control` not writable | clyded outside a login session | `loginctl enable-linger $USER` |
| `doctor`: cgroup delegation BLOCK, `/sys/fs/cgroup` read-only | You are in a container | Run clyded on the host. Nothing inside can lift it. |
| Task refused, `IsolationUnavailable` | No backend meets the task's `min_isolation` | Check `doctor`; T2+ needs bubblewrap **and** delegation (D22) |
| Task refused, `CgroupLimitsUnavailable` | Delegation went away after start | [Step 4](#step-4-cgroup-v2-delegation) |
| `rust.resolve-deps` refused | Requires microVM isolation | Expected. [Step 8](#step-8-firecracker) |
| Push refused, `RemoteNotAllowlisted` | `push.remotes` unset | [Step 6](#step-6-host-configuration). Without it every push is refused before a human is asked. |
| Push refused, protected branch | `protected_branch_patterns` matched | Refused before a human is asked, by design |
| Socket bind fails | State directory path over the 107-byte socket limit | Shorter `--state-dir` |
| Build script fails on missing git metadata | `.git` is never in a build sandbox (D21) | Known limitation; `doctor` lists it |
| Config change had no effect | Set in a layer that may not set it | `clyded --check`; repository layers may only narrow (D14, D20) |

Runtime roots being garbage-collected mid-run shows up as a runtime-root error.
Add the GC roots from [step 2](#step-2-materialise-the-runtime-roots).
