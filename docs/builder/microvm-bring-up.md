# MicroVM Bring-Up

How to run a build task inside a Firecracker guest, on a host with KVM, and what to look at when it does not work.

This is the [quick start](quick-start.md) with the isolation boundary swapped: same mission, same baseline, same task, executed inside a microVM instead of a namespace sandbox ([D24](decisions.md#d24-firecracker-is-the-default-backend-for-build-execution)). Read the quick start first — everything up to running a task is identical, and nothing here replaces it.

## What has been exercised, and what has not

**No test in this repository has booted a guest.** The host side is covered: the VM configuration and the job are asserted as values, images are built by real `mke2fs` invocations in the test suite, and the whole guest channel — hello, job, log streaming, report — runs end to end against a fake guest over the same sockets Firecracker uses. What no test can reach is Firecracker itself and the guest init running under a real kernel.

That is the same position the namespace backend was in before its first real run, and that run found four bugs the suite could not see ([changelog](../../CHANGELOG.md)). Expect to find some here. The console log named below is the first place to look.

## 1. Host prerequisites

```sh
ls -l /dev/kvm                 # present, and you are in the kvm group
lsmod | grep kvm               # kvm_intel or kvm_amd loaded
id -nG | tr ' ' '\n' | grep kvm
```

If `/dev/kvm` is missing on a CPU that reports `vmx` or `svm`, load the module (`sudo modprobe kvm_intel`); if the device exists but is unreadable, `sudo usermod -aG kvm $USER` and log out and back in. `clyde doctor` diagnoses both cases and names the remedy that applies to this host ([R10](decisions.md#r10-a-remedy-that-cannot-work-is-a-defect-not-a-nicety)).

Firecracker itself is not in the flake devShell — install it and note the path.

## 2. Build the guest

The kernel and the root images come from the same flake as everything else, so image identity follows closure identity ([D6](decisions.md#d6-runtime-roots-are-nix-closures-not-oci-images), [R12](decisions.md#r12-the-guest-root-image-is-uncompressed-erofs)):

```sh
nix build .#guestVm                   # writes ./result, an indirect GC root
```

The first build compiles a kernel and is slow; afterwards it is cached. The images are large — the rust root is a couple of gigabytes, uncompressed on purpose ([R12](decisions.md#r12-the-guest-root-image-is-uncompressed-erofs)).

Then give the daemon a `vm` directory. **Nothing creates it for you**: the state directory has no `vm` entry until this link exists, and the daemon does not go looking for a build result.

```sh
sudo mkdir -p /nix/var/nix/gcroots/clyde
sudo ln -sfn "$(readlink -f result)" /nix/var/nix/gcroots/clyde/guest-vm
ln -sfn /nix/var/nix/gcroots/clyde/guest-vm ~/.local/share/clyde/vm
```

Two links rather than one, for the same reason the [quick start](quick-start.md) uses a GC root for the runtime roots: the state directory's `vm` symlink is **not** a GC root, so pointing it straight at the store path leaves the images one `nix-collect-garbage` away from vanishing under a configured daemon. `./result` protects them only while it exists.

`~/.local/share/clyde` is the default state directory; substitute yours if it is configured elsewhere. Check the result:

```sh
ls -L ~/.local/share/clyde/vm ~/.local/share/clyde/vm/rootfs
```

The backend looks for exactly this layout, and reports a missing piece by name in `clyde doctor`:

```
<state-dir>/vm/vmlinux              uncompressed guest kernel, ELF
<state-dir>/vm/rootfs/workspace.img
<state-dir>/vm/rootfs/rust.img      one erofs image per runtime root
<state-dir>/vm/rootfs/fetch.img
```

## 3. Configure

Both keys are required, and the backend does not register without them:

```toml
[sandbox]
firecracker = "/usr/local/bin/firecracker"
mke2fs = "/nix/store/...-e2fsprogs-1.47.4-bin/bin/mke2fs"
```

`mke2fs` builds every guest image from a directory, unprivileged — no loop mount and no root anywhere in the path. Resolve the flake's copy rather than typing a path, and root it, because the daemon runs outside the devShell:

```sh
nix develop -c sh -c 'echo $CLYDE_MKE2FS'
sudo ln -sfn "$(nix develop -c sh -c 'echo $CLYDE_MKE2FS')" \
  /nix/var/nix/gcroots/clyde/mke2fs
```

The distribution's `/usr/sbin/mke2fs` also works and needs no GC root; `-d` support is the only requirement, so e2fsprogs 1.43 or newer.

```sh
clyded --check
clyde doctor
```

`doctor` reports `kvm`, `firecracker`, `mke2fs`, and `guest images` as separate rows, because they fail differently and their remedies are different: a missing binary is an install step and a missing image is a `nix build` plus a symlink.

## 4. Run a task in a guest

Everything up to here is the quick start: register the workspace, import dependencies, approve a mission, confirm an access baseline and the bundle inventory. Then:

```sh
clyde task run rust.check src --isolation microvm
```

`--isolation` is an operator option and it can **only raise** ([D25](decisions.md#d25-task-execution-has-an-operator-surface-on-the-admin-socket)). `min_isolation` in the task policy is a floor; asking for more than the floor needs no exception to the no-downgrade rule, and asking for less is refused. An actor holding a session token cannot use it at all — the flag is rejected before the request is made.

This is deliberately not the default yet. `rust.check` and `rust.test.unit` still carry `min_isolation: NamespaceSandbox`; raising the floor is the deliverable that closes [Part 1b](roadmap.md#part-1b-the-microvm-backend-as-the-default), and it should happen after a real host has run the loop, not before.

Compare the two backends on the same task:

```sh
clyde task run rust.check src                       # namespace sandbox
clyde task run rust.check src --isolation microvm   # guest
clyde task status <task-run-id>
```

The recorded backend comes from the selection rather than from an assumption, so `clyde task status` and the audit record say which one actually ran, and the `TaskStarted` audit event carries both the isolation the task ran at and what the operator asked for.

## 5. What a working run looks like

- `clyde task logs <id> --stream stdout` carries cargo's JSON diagnostics, streamed out of the guest over vsock while the task ran ([R11](decisions.md#r11-all-guest-output-leaves-over-vsock)). Nothing is read back out of a guest-written image.
- A compile error is a task outcome with a failure classification. A guest that could not run the task at all is a `SandboxFailure` naming the stage it failed at — mount, drive discovery, privilege drop, exec. That distinction is the whole point of the job contract.
- The second run in the same mission is much faster than the first: the mission cache image persists inside the mission's cache directory and is attached read-write to every run, while only the source snapshot image is rebuilt.

## 6. When it does not work

Per-run files live in the sandbox runtime directory (`<state-dir>/run/sandboxes` by default):

| File | What it holds |
|---|---|
| `<id>.json` | the exact VM configuration Firecracker was given |
| `<id>.console.log` | the guest's serial console: kernel boot messages and the init's own diagnostics |
| `<id>.work.img` | the per-run source image |
| `<state-dir>/run/vsock/<id>.vsock_*` | the control and log sockets |

The console log is the first thing to read. It is a diagnostic channel and never the log transport — the 8250 UART is slow enough that build output through it stalls the guest, which is why task output goes over vsock instead.

Failures worth recognising:

- **The guest boots and finds no drives.** `CONFIG_VIRTIO_MMIO_CMDLINE_DEVICES` is missing from the kernel. The flake asserts it at build time, so this should be impossible with a flake-built kernel and is the first thing to check with any other one.
- **`no block device carries the label clyde-work`**, listing what the guest could see. The image was built without the label, or the mount table changed shape. Labels are the contract; device names are positional and are never used.
- **The VM stops without reporting.** The host says so rather than inventing an exit status. Check the console log for a kernel panic or an init failure before the control channel was established.
- **The task cannot write to `/cache`.** The cache image's root is owned by whoever ran `mke2fs`; the init chowns the mount point to the task uid. A failure here shows up as a permission error from cargo.
- **`egress profile … needs the vsock egress bridge`.** Expected: `rust.resolve-deps` is refused on this backend today. See below.

## Not built yet

Three pieces of [Part 1b](roadmap.md#part-1b-the-microvm-backend-as-the-default) are deliberately absent, and each fails closed rather than degrading:

1. **The egress bridge.** The guest half of `clyde-forward` does not speak vsock, so a task whose profile permits egress is refused by preflight rather than run without the egress it was promised. This is what `rust.resolve-deps` needs.
2. **Learn mode inside the guest.** Host-side `inotify` cannot see guest reads; guest-side `fanotify` is unbuilt. `clyde access learn` still runs on the namespace backend.
3. **The policy floor.** `rust.check` and `rust.test.unit` still name `NamespaceSandbox` as their minimum, so the microVM is opt-in per run.

Also unmeasured: the latency target in [§9 of Part 1b](roadmap.md#9-parity-and-measured-latency) — no more than 2s p50 and 4s p95 over the namespace backend on a warm cache — has nothing to compare against yet. The kernel is currently built with everything compiled in, which makes it larger than it needs to be; if boot time turns out to dominate, that is the first thing to trim.
