# Installing Clyde

Setup for a single-user Linux host: the control plane, the credential broker, and
both sandbox backends.

Read this alongside [`clyde doctor`](#step-5-check-the-host), which is the
authority on whether a host has what it needs. Everything here is background for
what `doctor` reports and remedies for what it refuses.

**Before you start, three things are true and worth knowing up front:**

- **Bubblewrap is the working backend.** It runs every MVP task except dependency
  resolution, and a human drives the whole pipeline from the host with no agent
  configured anywhere
  ([D25](docs/builder/decisions.md#d25-task-execution-has-an-operator-surface-on-the-admin-socket)).
  Follow steps 1–7 and you have a working install. Note that this is
  the scaffold rather than the destination: the microVM backend is meant to be the
  default for every build task
  ([D24](docs/builder/decisions.md#d24-firecracker-is-the-default-backend-for-build-execution)),
  and until it is, build tasks run on a weaker boundary than the design intends.
- **The microVM backend can be brought up, per run.** [Step 8](#step-8-firecracker)
  builds the guest from the flake and an operator raises a single task into a
  guest with `clyde task run … --isolation microvm`. It is not the default yet,
  and three pieces are still missing — the vsock egress bridge, guest-side learn
  mode, and the raised policy floor — so `rust.resolve-deps` stays refused. No
  test in this repository has booted a guest;
  [the bring-up guide](docs/builder/microvm-bring-up.md) says what to read when
  one does not.
- **This install gives you `advisory` posture, and Clyde now says so.** Clyde
  constrains what runs *through* it — snapshots, no network, no credentials,
  baselines — but nothing here stops you or an agent on this host from running
  `cargo` directly and bypassing the pipeline entirely. `clyde doctor` reports
  that as a posture line naming each specific bypass, every task run records the
  posture it ran under, and mission review states it
  ([D26](docs/builder/decisions.md#d26-enforcement-posture-is-explicit-reported-and-recorded)).
  Closing the bypass is the warden — the agent-harness half of the MVP — and it
  is not built
  ([D23](docs/builder/decisions.md#d23-the-mvp-splits-into-the-builder-and-the-warden)).
  That is worth understanding before you rely on it, not after.

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
| Filesystem | Hardlink support on the state directory, where the blob store and snapshot trees both live. Without it, snapshots fall back to copying: slower per run, but still correct and still incremental, because the copy carries the blob's mtime ([D27](docs/builder/decisions.md#d27-snapshot-materialisation-preserves-change-ordering-in-mtime)). `clyde doctor` reports which you have. |
| For Firecracker | `/dev/kvm`, membership of the `kvm` group, and `mke2fs` from `e2fsprogs`. Today the microVM backend is opt-in per run with `--isolation microvm`; under [D24](docs/builder/decisions.md#d24-firecracker-is-the-default-backend-for-build-execution) it becomes the default requirement for every build task. |

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

And the closure manifests, which are what make a root's closure bindable:

```sh
sudo ln -sfn "$(nix build .#runtimeRootManifests --print-out-paths --no-link)" \
  /nix/var/nix/gcroots/clyde/manifests
```

This one is not optional. A runtime root is a `buildEnv` — a tree of symlinks
into other store paths — so without the manifest the daemon binds the root and
nothing it points at, every binary inside the sandbox is a dangling symlink, and
the task dies with `bwrap: execvp /nix/var/.../bin/cargo: No such file or
directory`. clyded refuses the run with a diagnostic naming this key rather than
letting it reach that point.

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

Do not read the sysctl as the whole answer. Ubuntu 24.04 ships
`/etc/apparmor.d/bwrap-userns-restrict` **already loaded and enforcing**, so its
own `/usr/bin/bwrap` keeps working while the sysctl reads `1` and the nix-store
`bwrap` is denied. That mixed signal is the expected shape of the problem, not a
contradiction. Test the binary Clyde will actually run:

```sh
"$(nix develop --command bash -c 'echo "$CLYDE_BWRAP"')" \
  --unshare-user --uid 0 --gid 0 --ro-bind / / /bin/true \
  && echo "the nix bwrap can unshare"
```

### The minimal fix: a profile for the store path

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
```

Verify with the `bwrap` check above, not with `unshare` — a profile naming
`bwrap` does nothing for `/usr/bin/unshare`, which stays denied.

It lives in `/etc`, so it survives a reboot.

Note the trade-off honestly: the glob permits `userns create` for any
`bwrap`-shaped path in the nix store, not just the one you configured. Narrowing
it to a single store path means updating the profile on every toolchain bump.

### Better: adapt the profile your distribution ships

If your distribution already carries a `bwrap` profile, start from it rather than
from the sketch above:

```sh
ls /etc/apparmor.d/ | grep -iE 'bwrap|userns'
```

Ubuntu's `bwrap-userns-restrict` is the profile to copy. It is tighter than the
sketch, which leaves bwrap's children unconfined: it grants `allow capability` to
bwrap itself — where the uid-map, mount and `pivot_root` work happens, all before
the payload is exec'd — and then `px`-transitions the payload into a stacked child
profile carrying `audit deny capability`, so bwrap cannot be used to launder the
userns restriction. Clyde's payload runs as uid 1000 under seccomp and
no-new-privs and needs no capability, so the child profile costs nothing. The one
thing it forbids is a nested `bwrap` inside the sandbox, which the builder does
not do.

Two changes are required, and one of them is not optional:

1. **Re-attach it** to `/nix/store/*/bin/bwrap`. AppArmor attaches by executable
   path; the shipped attachment is the literal `/usr/bin/bwrap`.
2. **Rename both profiles.** Profile names are global across
   `/etc/apparmor.d/`, so a second `profile bwrap` or `profile unpriv_bwrap`
   collides with the file still shipped by the distribution. Rename the parent,
   the child, and the `px` target that names them both.

```
# /etc/apparmor.d/nix-bwrap — Ubuntu's bwrap-userns-restrict, re-attached
abi <abi/4.0>,
include <tunables/global>

profile nix_bwrap /nix/store/*/bin/bwrap flags=(attach_disconnected) {
  # …body copied verbatim from /etc/apparmor.d/bwrap-userns-restrict…
  allow px /** -> nix_bwrap//&unpriv_nix_bwrap,
  include if exists <local/nix_bwrap>
}

profile unpriv_nix_bwrap flags=(attach_disconnected) {
  # …body copied verbatim…
  allow pix /** -> &unpriv_nix_bwrap,
  audit deny capability,
  include if exists <local/unpriv_nix_bwrap>
}
```

Ignore the "disabled by default … use `aa-enforce` to enable it" comment in the
shipped file's header: it is upstream's, and Ubuntu 24.04 ships the profile
enabled. Reload and re-run the `bwrap` check from the top of this step.

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
nix develop --command bash -c 'echo "$CLYDE_BWRAP"'
```

Put that path in `sandbox.bwrap` (step 6).

## Step 4: cgroup v2 delegation

Build and test tasks (T2 and above) are **refused outright** without a delegated
cgroup v2 subtree. There is no degraded mode and no opt-in flag (D22), so treat
this as a hard prerequisite rather than a tuning step.

On a systemd machine it usually works already, because `user@$UID.service` is
delegated by default:

```sh
systemctl show user@$(id -u).service -p Delegate     # want Delegate=yes
test -w /sys/fs/cgroup/user.slice/user-$(id -u).slice/user@$(id -u).service/cgroup.subtree_control \
  && echo delegated
```

The controllers Clyde needs are `memory`, `pids`, and `cpu`, and the file that
answers that is the one inside the delegated cgroup — not the root set, which
says what the kernel has rather than what reaches you:

```sh
slice=/sys/fs/cgroup/user.slice/user-$(id -u).slice/user@$(id -u).service
cat $slice/cgroup.controllers        # what is available to the subtree
cat $slice/cgroup.subtree_control    # what is enabled for children
```

If the two checks disagree — `Delegate=yes` but nothing writable, or the
reverse — trust the writable test. It is what Clyde probes: `doctor` opens the
`cgroup.subtree_control` of clyded's own cgroup, taken from `/proc/self/cgroup`,
and never reads the `Delegate=` property. The property is a statement of intent
by the unit that spawned the session; the writable file is the delegation
itself.

Note also that the query above has no `--user`. `user@$UID.service` is a *system*
unit, so `systemctl --user show user@$UID.service` asks the per-user manager
about a unit it does not manage and cannot see; `show` answers for an unknown
unit with property defaults, and `Delegate` defaults to `no`. That spelling
prints `Delegate=no` on every host, delegated or not.

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
ok   session bus          user bus at /run/user/1000/bus
ok   runtime roots        closures bound — fetch: 41 programs, 62 store paths; rust: …
ok   bubblewrap           bubblewrap at /nix/store/…/bin/bwrap
ok   nix                  nix at …
ok   hardlinks            hardlinks supported on …

ok    workspace environment
ok    build and test tasks
NO    dependency resolution
NO    brokered publishing
```

`ok` on the first two summary lines is the meaningful signal: it means clyded will
admit a bubblewrap-backed task rather than refuse it on host capability. `kvm`,
`firecracker`, `mke2fs`, and `guest images` stay absent until you do
[step 8](#step-8-firecracker), and `brokered publishing` turns `ok` once
`clyde-brokerd` is running ([step 7](#step-7-run-the-daemon-and-the-broker)).

Once [D26](docs/builder/decisions.md#d26-enforcement-posture-is-explicit-reported-and-recorded)
lands, `doctor` also reports the **posture** and, when it is `advisory`, names the
specific bypass — a `cargo` on `PATH`, no hosted actor, or both. That line is not
a tooling-hygiene warning. It is the difference between "project code cannot run
outside Clyde" and "project code happens not to have run outside Clyde yet", and
it is the one line worth reading before deciding what this install guarantees.

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

[`config.example.toml`](config.example.toml) in the repository root is a complete
host configuration for a builder-only deployment, commented with why each setting
exists. Copy it and replace every `REPLACE_ME`:

```sh
sudo install -Dm644 config.example.toml /etc/clyde/config.toml
sudo $EDITOR /etc/clyde/config.toml
```

Every key in it — including the sections left commented out — is checked against
the loader, so it is a working file rather than a sketch. The parts you must fill
in are the pinned `bwrap` path and the three runtime roots; everything else has a
usable default or is only needed for brokered push and the warden.

Validate before relying on it. This loads the configuration, probes the host, and
exits:

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

The microVM backend runs a build task inside a hardware-virtualised guest, which
is the boundary hostile dependency code should execute behind
([D24](docs/builder/decisions.md#d24-firecracker-is-the-default-backend-for-build-execution)).
It is **opt-in per run** today: `rust.check` and `rust.test.unit` still name
`NamespaceSandbox` as their policy floor, and an operator raises it per task with
`--isolation microvm`. `rust.resolve-deps` requires it and is still refused,
because the vsock egress bridge it needs is unbuilt.

[docs/builder/microvm-bring-up.md](docs/builder/microvm-bring-up.md) is the
walkthrough, including what to read when a guest does not boot. This step is the
host setup it assumes.

### Host prerequisites

```sh
ls -l /dev/kvm                       # present?
lsmod | grep kvm                     # kvm_intel or kvm_amd loaded?
grep -oE '\b(vmx|svm)\b' /proc/cpuinfo | head -1   # does the CPU support it?

sudo usermod -aG kvm $USER           # then log out and back in
```

If `/dev/kvm` is missing on a CPU that reports `vmx` or `svm`, `clyde doctor`
says so explicitly: that is a device that was not exposed, not hardware that
cannot virtualise. Inside a VM, enable nested virtualisation on the hypervisor.

Firecracker is not in the devShell, so install it separately. `mke2fs` is — it
comes from `e2fsprogs` in the flake — and the backend needs both:

```toml
[sandbox]
firecracker = "/usr/local/bin/firecracker"
mke2fs = "/nix/store/...-e2fsprogs-1.47.4-bin/bin/mke2fs"
```

Resolve the flake's `mke2fs` rather than typing a path, and give it a GC root —
the daemon runs outside the devShell, so an unrooted store path can be collected
out from under it:

```sh
nix develop -c sh -c 'echo $CLYDE_MKE2FS'
sudo ln -sfn "$(nix develop -c sh -c 'echo $CLYDE_MKE2FS')" \
  /nix/var/nix/gcroots/clyde/mke2fs
```

The distribution's own `/usr/sbin/mke2fs` works too, and needs no GC root. The
only hard requirement is `-d` support — e2fsprogs 1.43 or newer — so any current
version will do. Pinning the flake's copy is the more consistent choice, since
the flake is the source of truth for tooling; pinning the distribution's is the
more stable one.

Without `mke2fs` the backend does not register at all, because every guest image
is built with it, unprivileged: no loop mount and no root anywhere in the path.

### Build the guest

The kernel and the root images come from the flake, so image identity follows
closure identity:

```sh
nix build .#guestVm
sudo ln -sfn "$(readlink -f result)" /nix/var/nix/gcroots/clyde/guest-vm
ln -sfn /nix/var/nix/gcroots/clyde/guest-vm ~/.local/share/clyde/vm
```

The second link is what creates the state directory's `vm` entry — nothing
creates it for you, and the daemon does not go looking for a build result. The
first is a GC root, for the same reason step 2 adds one for the runtime roots:
the `vm` symlink is not a GC root, so pointing it straight at the store path
leaves several gigabytes of images one `nix-collect-garbage` away from vanishing
under a configured daemon.

That produces the layout the backend expects:

```
<state-dir>/vm/vmlinux              uncompressed guest kernel, ELF
<state-dir>/vm/rootfs/workspace.img
<state-dir>/vm/rootfs/rust.img      one erofs image per runtime root
<state-dir>/vm/rootfs/fetch.img
```

The first build compiles a kernel and takes a while. The images are large — the
rust root is a couple of gigabytes, uncompressed deliberately
([R12](docs/builder/decisions.md#r12-the-guest-root-image-is-uncompressed-erofs)) —
and they need a GC root like every other store path the daemon depends on.

### How a run is put together

The runtime root is attached read-only as the erofs root device. Every mount with
a host source becomes a block device, because Firecracker has no filesystem
passthrough of any kind: the source snapshot is an ext4 image built per run, and
the mission cache is one ext4 image created once and attached read-write to every
run of that mission. The guest finds each by **filesystem label**, since device
names are positional.

Every VM gets a vsock device — logs have nowhere else to go, and the 8250 console
stalls under build output — and the host binds a listener on the egress port
**only** for a profile that permits egress
([D7 amendment](docs/builder/decisions.md#amendment-the-guest-channel-is-a-single-vsock-multiplexed-by-port)).
There is no network device in any configuration.

### What is not built

- **The vsock egress bridge.** A task whose profile permits egress is refused by
  preflight rather than run without it. This is why `rust.resolve-deps` still
  does not run.
- **Guest-side learn mode.** `clyde access learn` runs on the namespace backend.
- **The raised policy floor**, which is what would make the microVM the default
  rather than an operator's per-run choice.

No test in this repository has booted a guest. The host side is covered by tests,
including real image builds and the whole guest channel against a fake guest, but
the first real boot is the first real boot — see the
[bring-up guide](docs/builder/microvm-bring-up.md).

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

Approve it, then confirm an access baseline. **A build task with no confirmed
baseline is refused** — deliberately, so there is no implicit wide-scope first run
([D18](docs/builder/decisions.md#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift)).
`propose` computes the baseline from the static build closure and shows it;
`confirm` puts it in force:

```sh
clyde mission approve <mission-id>

clyde access propose --workspace <workspace-id> --task rust.check crates/thing
clyde access confirm --workspace <workspace-id> --task rust.check crates/thing
```

The proposal separates the two tiers, and the distinction is the point: subtree
grants are first-party code and are not drift-sensitive, so ordinary editing
inside them never prompts again; pins are everything outside them and each
carries a reason.

Then watch and close:

```sh
clyde mission status <mission-id>
clyde audit show --limit 50
clyde audit verify                # the hash chain, including its recorded head
clyde mission review <mission-id> # end-to-end review; verifies the chain too
clyde mission close <mission-id>
```

Every command takes `--json` for a stable machine-readable shape, and `clyde tui`
gives the same surfaces interactively.


### Posture

`clyde doctor` ends with the deployment's posture and names every bypass it can
see:

```
posture  advisory: project code can be run outside Clyde entirely
  - a project build toolchain is on PATH (cargo at /nix/store/…/bin/cargo), so project code can be built outside Clyde
  - no agent is hosted, so the driver's environment is the host's
```

Read that as the honest summary of what this install defends against. A hostile
dependency is fully contained — it runs against a read-only snapshot with no
network and no credentials, whoever asked for the build. A careless or hostile
*driver* is not, because nothing stops it running `cargo` itself.

Posture is derived and never configured. There is no key that makes an advisory
deployment report as enforcing, and it participates in no admission decision — it
changes what is reported, never what is permitted
([D26](docs/builder/decisions.md#d26-enforcement-posture-is-explicit-reported-and-recorded)).
It is recorded on each task run rather than looked up later, so a deployment that
gains the warden mid-mission does not make the earlier work look as though it
were enforced.

### Running a task

`clyde task run` works on both surfaces
([D25](docs/builder/decisions.md#d25-task-execution-has-an-operator-surface-on-the-admin-socket)),
and picks between them from where it is running rather than from a flag:

```sh
clyde task run rust.check crates/thing
clyde task run rust.check crates/thing --mission <mission-id>
```

On the host there is no session token, so this is an **operator** request on the
admin socket, authenticated by `SO_PEERCRED`. Inside a workspace environment a
token is present and the same command is an **actor** request. `--mission` is
optional while exactly one mission is active; with several, Clyde lists them
rather than guessing, because running against the wrong mission charges the wrong
budget and uses the wrong access baseline.

The surfaces differ in who authenticates and in what the record says. They do not
differ in what is allowed: the same request is admitted against the same lease,
policy, budget, and baseline either way, and the acting principal is recorded on
the run so review does not have to infer it:

```
task.requested … path=crates/core principal=operator principal_detail=operator(uid 1000)
```

Those are two separate fields. `actor` is the lease's actor — who the work is
attributed to. `principal` is who actually asked.

What is **not** affected: `clyde approve` still refuses inside a sandbox, and that
refusal is load-bearing — it is what makes agent self-approval impossible
([D2](docs/builder/decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel)).
The two refusals protect different properties, and only that one was ever
intended.

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `doctor`: user namespaces BLOCK, AppArmor | Ubuntu 23.10+ default | [Step 3](#step-3-user-namespaces-and-apparmor) |
| `doctor`: cgroup delegation BLOCK, `subtree_control` not writable | clyded outside a login session | `loginctl enable-linger $USER` |
| `doctor`: session bus MISS | clyded has no `XDG_RUNTIME_DIR` — started as a system unit, or under sudo | Start it in a login session: `systemctl --user`, or a terminal. Build tasks are refused without it |
| `doctor`: runtime roots BLOCK, programs would not resolve | `sandbox.runtime_root_manifests` unset, so only the root itself is bound | [Step 2](#step-2-materialise-the-runtime-roots) |
| `doctor`: cgroup delegation BLOCK, `/sys/fs/cgroup` read-only | You are in a container | Run clyded on the host. Nothing inside can lift it. |
| Task refused, `IsolationUnavailable` | No backend meets the task's `min_isolation` | Check `doctor`; T2+ needs bubblewrap **and** delegation (D22) |
| Task refused, `CgroupLimitsUnavailable` | Delegation went away after start | [Step 4](#step-4-cgroup-v2-delegation) |
| `rust.resolve-deps` refused | Requires microVM isolation, and its vsock egress bridge is unbuilt | Expected. [Step 8](#step-8-firecracker) |
| Task refused, `firecracker cannot honour this specification: egress profile …` | The guest egress bridge is not built | Expected; run the task on the namespace backend |
| `no block device carries the label …` in a guest run | The image was built without a label, or the mount table changed shape | [Bring-up guide](docs/builder/microvm-bring-up.md) |
| Guest run fails, `the VM stopped without reporting` | The guest died before reporting | Read `<state-dir>/run/sandboxes/<id>.console.log` |
| Push refused, `RemoteNotAllowlisted` | `push.remotes` unset | [Step 6](#step-6-host-configuration). Without it every push is refused before a human is asked. |
| Push refused, protected branch | `protected_branch_patterns` matched | Refused before a human is asked, by design |
| `bwrap: execvp <path>: No such file or directory` | The runtime root closure is unbound: `sandbox.runtime_root_manifests` unset | [Step 2](#step-2-materialise-the-runtime-roots). A `buildEnv` root is symlinks into store paths the closure must name |
| Socket bind fails | State directory path over the 107-byte socket limit | Shorter `--state-dir` |
| Build script fails on missing git metadata | `.git` is never in a build sandbox (D21) | Known limitation; `doctor` lists it |
| Config change had no effect | Set in a layer that may not set it | `clyded --check`; repository layers may only narrow (D14, D20) |

Runtime roots being garbage-collected mid-run shows up as a runtime-root error.
Add the GC roots from [step 2](#step-2-materialise-the-runtime-roots).
