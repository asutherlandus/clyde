# Changelog

## Unreleased — the microVM guest, buildable and runnable per task

The Firecracker backend had a host side and no guest. It now has both, and an
operator can raise a single task into a hardware-virtualised guest with
`clyde task run rust.check src --isolation microvm`.

**Nothing here has booted a guest.** The host side is covered — the VM
configuration and job are asserted as values, guest images are built by real
`mke2fs` invocations in the test suite, and the whole guest channel runs end to
end against a fake guest over the same sockets Firecracker uses — but Firecracker
itself and the init under a real kernel are untested, which is the same gap that
hid four bugs before the first bubblewrap run.
[docs/builder/microvm-bring-up.md](docs/builder/microvm-bring-up.md) is the
walkthrough and the debugging guide.

Two choices were recorded before any of it was written, because both are
expensive to change afterwards:

- **[R11](docs/builder/decisions.md#r11-all-guest-output-leaves-over-vsock): all
  guest output leaves over vsock.** Logs, cargo's JSON diagnostics, and artifacts
  stream out on the multiplex; nothing is read back out of a guest-written image.
  The argument for `debugfs` extraction was that it invents no protocol, and that
  argument was spent the moment the D7 amendment put a control port and a log
  port on every VM. One output path also means a guest that dies mid-run has
  already delivered what it emitted.
- **[R12](docs/builder/decisions.md#r12-the-guest-root-image-is-uncompressed-erofs):
  the guest root is uncompressed erofs.** A nix closure is read randomly at every
  `exec`, which is block-compressed squashfs's weakest pattern, and every run in a
  mission reads the same image, so it belongs in the host page cache as mapped
  pages.

### Added

- **A guest kernel and root images, built by the flake.** `nix build .#guestVm`
  produces the directory the backend expects: an uncompressed ELF `vmlinux` and
  one erofs image per runtime root, built from the same closures the namespace
  backend binds, so runtime-root identity is stable across backends. The kernel
  derivation asserts the options whose absence produces a guest that boots and
  does nothing useful — `VIRTIO_MMIO_CMDLINE_DEVICES` above all, since Firecracker
  has no device tree on x86_64 and passes its virtio devices on the command line.
- **`clyde-guest-api`, the job contract.** A versioned type shared by host and
  guest carrying argv, environment, working directory, the drive-label → mount
  map, and the task uid. Drives are addressed by **filesystem label**, never by
  device name: guest device names are positional and `drive_id` is invisible in
  the guest, so a positional contract breaks silently the first time the mount
  table changes shape. The label parse is in the shared crate, so the label the
  host writes at `mkfs` time and the one the guest looks for cannot drift apart.
- **`clyde-init`, the guest init.** Mounts its drives by label, takes its job over
  vsock rather than from the kernel command line — a VM booted before its job
  exists cannot carry that job in its boot arguments, which is the option a future
  warm pool needs left open — drops out of guest root before exec, streams output
  back while the task runs, unmounts its writable drives so the next run reads a
  clean image, and resets the VM.
- **Block-image construction.** Firecracker has no filesystem passthrough of any
  kind, so every mount with a host source becomes an image. The source snapshot is
  built per run with `mke2fs -d`, unprivileged; the mission cache is one image
  created once and attached read-write to every run of that mission, living inside
  the mission's cache directory so closeout deletes it along with everything else.
- **`--isolation` on the operator task surface.** It can only *raise*:
  `min_isolation` is a floor, and asking for more than the floor needs no
  exception to the no-downgrade rule. An actor holding a session token cannot use
  it at all. The isolation a task ran at, and separately what an operator asked
  for, are both recorded in the audit trail.
- **`clyde doctor` reports `mke2fs` and `guest images`** as rows of their own,
  because they fail differently from a missing `firecracker` binary and their
  remedies are different in kind — an install step versus a `nix build` and a
  symlink.

### Changed

- **Every VM now has a vsock device**, whatever its egress profile. The old rule —
  no channel at all under profile `none` — is superseded by the
  [D7 amendment](docs/builder/decisions.md#amendment-the-guest-channel-is-a-single-vsock-multiplexed-by-port):
  logs have nowhere else to go, and the 8250 UART stalls under build output. The
  enforceable claim moves to the host's socket table: **no listener is bound on
  the egress port unless the profile permits egress**, and the guest still has no
  network device in any configuration. The test that pinned the superseded rule is
  replaced by one that asserts the new one.
- **The recorded backend comes from the selection** rather than being assumed at
  admission. A run that happened in a microVM was previously recorded as having
  happened in a namespace sandbox, which matters because the recorded backend is
  evidence.
- **A task's exit status and a guest failure are different values.** A task that
  ran and failed produces an exit status; a guest that could not run the task
  produces a `SandboxFailure` naming the stage — mount, drive discovery, privilege
  drop, exec. A VM that stops without reporting is neither, and is never readable
  as a pass.

### Fixed

- **`clyde mission renew` could not renew an expired mission, silently left the
  mission's own expiry behind, and could undo a revocation.** Three defects in one
  function that had no test coverage at all. The extension was added to the
  *current* expiry, so renewing a lease that had already lapsed produced a lease
  born expired and a validation error naming two timestamps
  (`expiry … is not after issue time …`); it now runs from the later of now and
  the current expiry. The mission's expiry was assigned on a local copy and then
  persisted with the cache-directory setter, which re-reads the record and
  discards it, so the mission stayed expired while its lease was renewed; there is
  now a `set_mission_expiry` store method. And renewal picked up any
  non-superseded primary lease, including the revoked lease of a closed or revoked
  mission, issuing an active replacement — revocation is final, and both the
  terminal mission and the revoked lease are now refused by name.

- **`clyde doctor` reported configured paths as unconfigured.** Without a
  reachable `clyded` the fallback probe loaded no configuration, so the runtime
  roots row read `MISS — no runtime roots are configured` on a host whose roots
  were configured correctly, and the remedy told the operator to set keys that
  were already set. The fallback now loads the same host and user files the
  daemon would ([R9](docs/builder/decisions.md#r9-clyde-doctor-works-without-a-daemon)),
  the discovery and layering moved into `clyde-policy` so both binaries share one
  answer, and the report names its source — `reported by the running clyded` or
  `probed directly`, since "not configured" and "not visible from here" are
  different claims that rendered identically.

- **The Firecracker console would have stalled the guest.** `start` piped
  Firecracker's stdout and stderr and nothing ever read them; an 8250 console
  whose reader never drains it blocks the guest, which is the exact failure the
  console exists to diagnose. It now goes to `<id>.console.log` in the sandbox
  runtime directory, which is also the first thing the bring-up guide tells you to
  read.

### Not built

The vsock egress bridge (so `rust.resolve-deps` stays refused rather than running
without the egress it was promised), guest-side `fanotify` learn mode, and the
raised policy floor that would make the microVM the default rather than an
operator's per-run choice. The latency target in
[Part 1b §9](docs/builder/roadmap.md#9-parity-and-measured-latency) has nothing to
compare against until a guest actually boots.


## Earlier unreleased — the first real bubblewrap run, and what it found

`rust.check` has now executed inside a real isolation boundary: `cargo check`
under bubblewrap, against a read-only snapshot at `/work`, offline, on a host with
unprivileged user namespaces, cgroup v2 delegation, and a user session bus.

Getting there took four fixes. All four were on the spawn path, and none was
visible to the test suite, because the suite asserts over the constructed argv and
sandbox specification and never executes one. Each fix exposed the next. The
durable answer is an integration test that actually spawns a sandbox, gated on
host capability — it would have caught all four at once, and it does not exist
yet.

- **Fixed: every T2 build task failed before `bwrap` was reached.** The
  bubblewrap backend cleared the whole environment for the wrapper chain, but a
  build task's chain begins with `systemd-run --user --scope`, which needs
  `XDG_RUNTIME_DIR` (or `DBUS_SESSION_BUS_ADDRESS`) to resolve the per-user
  manager's bus. With neither, it exits 1 in milliseconds, and the task surfaced
  as `internal` — "exited with status 1 and produced no diagnostic Clyde could
  classify". Those two variables now reach the wrapper for trust classes that get
  a cgroup scope; `bwrap --clearenv` still applies to the payload, so nothing
  reaches project code. This was unconditional, which is why it went unnoticed:
  the command construction is a pure function and every test asserted over the
  argv, never over the spawn.
- **Fixed: a build task's cargo was a placeholder.** `cargo_argv` computed
  `/nix/store/placeholder/bin/cargo`, discarded it, and pushed the bare name
  `cargo` as `argv[0]`. Under `bwrap --clearenv` there is no `PATH` for `execvp`
  to search, so every build and test task died with `bwrap: execvp cargo: No such
  file or directory`. It now resolves cargo from the task's runtime root, which is
  what `rust.resolve-deps` already did. Relatedly, `cargo_program` no longer falls
  back to the bare name when a runtime root has no `bin/cargo`: it names the root,
  its path, and the configuration key, because the fallback could only ever
  reproduce the same opaque failure inside the sandbox.
- **Fixed: a runtime root's closure was never bound.** `Daemon::new` passed
  `None` as the manifest directory, so `RuntimeRoot::read` fell back to "the
  closure is the root itself". A runtime root is a `buildEnv` — a tree of symlinks
  into other store paths — so the sandbox bound the root and nothing it pointed
  at, and every binary in it was a dangling symlink: `bwrap: execvp
  /nix/var/nix/gcroots/clyde/rust/bin/cargo: No such file or directory`. New host
  configuration key `sandbox.runtime_root_manifests` names the
  `nix build .#runtimeRootManifests` output, which the flake already produced and
  nothing consumed. `cargo_program` also now refuses up front when the resolved
  program falls outside the bound closure, naming the key — the failure is a
  configuration error and should not be discovered as an `execvp` ENOENT.
- **Fixed: a task was handed the GC-root path, which no sandbox binds.**
  Configuration names `/nix/var/nix/gcroots/clyde/rust`, a symlink into the store
  that exists on the host only. What a sandbox binds is the *closure*, and a
  closure is store paths — so `argv[0]` addressed through the GC root was
  unreachable inside the sandbox however complete that closure was, producing the
  same `bwrap: execvp …: No such file or directory`. `RuntimeRoot::read` now
  canonicalises the root, which is also its identity under D6; the GC root is an
  anchor against garbage collection, not a name a task can be given.
- **New: two `doctor` probes, for the two failures above.** Both were invisible
  to a report that said `build and test tasks: ok`, which is the one line bring-up
  is supposed to be able to trust.
  - **session bus** — whether `systemd-run --user` can resolve the per-user
    manager, from `DBUS_SESSION_BUS_ADDRESS` or `$XDG_RUNTIME_DIR/bus`. Distinct
    from `cgroup delegation`, which asks whether a delegated subtree is writable:
    both must hold, and a host can have one without the other. It now counts
    toward `can_run_build` and is named in `build_blockers`, so delegation is not
    blamed for a bus problem.
  - **runtime roots** — whether every program in each configured root still
    resolves once only that root's closure is bound. Reports how many programs
    would dangle and names the first, with the remedy pointing at
    `sandbox.runtime_root_manifests`.

  Both are inspection only, consistent with the rest of `probe`: no subprocess,
  nothing created, no privilege.
- **Fixed: `doctor` reported version skew as a failed probe.** `clyde doctor`
  prefers the running daemon's report, so a `clyde` newer than its `clyded`
  rendered the new rows from keys the daemon never sent — `MISS` with a `-`
  detail, which reads as a broken host rather than a stale binary. An absent key
  is now marked `?` and says to reinstall clyded from the same build.
- **New: [`docs/builder/manual-builds.md`](docs/builder/manual-builds.md)** — the
  operator loop for compiling and testing by hand, with no agent configured. What
  one run actually does, scoping a run inside a cargo workspace, the dependency
  situation, and how to read a refusal.

### Known, unfixed

- `find_workspace_root` walks up to the outermost manifest carrying a
  `[workspace]` table and does **not** consult that table's `exclude` list, so a
  cargo project nested under an excluded path resolves its closure against the
  outer workspace. `tests/fixtures/single-crate` under this repository yields the
  root `Cargo.toml` and all thirteen members. Cargo disagrees — it treats an
  excluded package as its own workspace root — so the snapshot and the build
  disagree about what is being built. Registering the nested project as its own
  Clyde workspace avoids it.

## Earlier unreleased — documentation: `docs/` partitioned by product

Documentation only; no code changed.

- The two products are now named. **Part 1 is the builder**; **Part 2 is the
  warden**. Commit history and code comments predating this use the old names,
  and D23 is retitled accordingly.
- The warden is deliberately *not* called "the sandbox". In this codebase
  *sandbox* always names the isolation mechanism — `SandboxBackend`, `SandboxSpec`,
  "no credential in any sandbox mount table" — and reusing it for the product made
  sentences like "the sandbox creates a sandbox for the agent" unwriteable. `warden`
  collides with nothing, pairs with `builder` as an agent-noun, and names the right
  thing: the builder bounds *what runs*, the warden bounds *who may ask*. It is
  custody of **authority**, not of a prisoner — the hosted agent is semi-trusted,
  careless rather than hostile, and hostile code remains the builder's problem.
- `docs/` is partitioned to match: `docs/builder/` and `docs/warden/`, with the
  vocabulary, threat model, and competitive survey shared at `docs/`. Each product's
  documents describe only that product, so the Part-1/Part-2 caveats that ran
  through every earlier document are gone; where the builder's guarantees depend
  on the warden being present, the statement is made once, as posture.
- The decision log is split by owning product. Identifiers are unchanged and
  continuous across both files: `docs/warden/decisions.md` holds D1, D11, D17,
  D20, and OQ3; `docs/builder/decisions.md` holds everything else.
- Twenty-two documents become nine, and the set shrinks by roughly two thirds.
  Requirements, the high-level design, the component architecture, technology
  choices, and the sequence flows merge into `docs/builder/design.md`; the task
  policy matrix and the network egress model into `docs/builder/tasks-and-policy.md`;
  the roadmap and the five phase specs into `docs/builder/roadmap.md`. No decision,
  exit criterion, or security property was dropped in the merge.

## Earlier unreleased — documentation: the Part 1 / Part 2 split

Design and planning only; no code changed.

- **D23** splits the MVP into two products: Part 1, the sandboxed build pipeline,
  and Part 2, the agent harness. They defend against different attackers — a
  hostile dependency inside the workload, and a careless or injected agent driving
  it — which need an execution boundary and an authority boundary respectively.
  Containing the agent turns out not to be load-bearing against the supply-chain
  threat, which is what makes the pipeline separable and independently useful.
- **D24** makes Firecracker the default backend for every build task rather than
  only for dependency resolution, and sequences Part 1 as 1a (namespace backend,
  operator-driven) then 1b (microVM default). Records the consequences that follow
  from Firecracker's device model: block devices only, a per-mission cache image,
  a versioned job contract, guest-side learn mode, and artifact extraction.
- **D25** adds an operator task surface on the admin socket, so the build pipeline
  is drivable — and testable — with no agent configured. Records the acting
  principal on every side-effecting record.
- **D26** makes enforcement posture explicit: `enforcing` or `advisory`, derived
  rather than configured, reported by `doctor`, recorded on every task run, and
  stated in mission review. Part 1 alone cannot stop a driver bypassing the
  pipeline, and this is the guard against describing it as though it could.
- Amendments recorded rather than made quietly: D1 narrowed to "typed tasks are
  the only path to the build toolchain", D2 gaining self-confirmation semantics,
  D5 superseded as the end state, D7 weakened from "no channel of any kind" to a
  port-level claim over one vsock, D8's broker value made posture-dependent, D22
  narrowed to hosts without KVM, and OQ5 reframed now that virtio-fs is known to
  be unavailable under Firecracker.
- New spec: [Part 2: The Agent Harness](docs/warden/spec.md). Phase 1's
  agent-hosting deliverable moved into it; Phase 2's sub-phases renamed to Parts
  1a and 1b.

## Unreleased — the Clyde Next MVP

The MVP slice, Phases 0 through 4, implemented against the design document set in
`docs/`.

### Phase 0 — foundations

- Nix flake as the source of truth for tooling, with the three runtime roots as
  nix closures and a `checks` output that asserts over the closure of
  `runtimeRoots.workspace` that no project build toolchain is reachable from it.
  Adding `cargo` to that root fails `nix flake check`.
- Every entity in the schema reference as a validated Rust type: newtype
  identifiers, state machines as transition functions returning `Result`, and
  redaction that is structural — credential-bearing types do not implement
  `Serialize`, so they cannot reach a log line or an audit payload by accident.
- The four functions the security model rests on, as pure functions with denial
  cases tested first: task policy resolution, lease derivation, egress profile
  ordering, and budget charging.
- Configuration layering in which the repository layer may only narrow, and a
  widening value is a rejection with a diagnostic rather than a silent clamp.
- Storage with an append-only, hash-chained audit log whose head is recorded
  separately, because truncating the tail of a bare chain is otherwise
  undetectable.
- `clyde doctor`, which distinguishes "user namespaces unavailable" from "blocked
  by AppArmor" and works without a running daemon.
- `clyde doctor` names remedies that can actually work where it is running: a
  read-only cgroupfs is reported as the container boundary it is rather than as a
  session misconfiguration, and a missing `/dev/kvm` on a CPU reporting `vmx` or
  `svm` is reported as a device that was not exposed rather than as absent
  hardware. Enclosure detection feeds the remedy text only; a test asserts it
  cannot change what the host is permitted to run.
- Fifteen test fixtures, each stating the property it asserts in its own README.

[INSTALL.md](INSTALL.md) covers host setup for both backends, including what is
missing on the Firecracker side and why supplying KVM is not enough.

### Phase 1 — mission, lease, and approval

- Two sockets, and the separation is the mechanism rather than a policy: the
  admin socket is never mounted into a sandbox, so an agent cannot approve its
  own request. `clyde approve` refuses inside a sandbox and says why.
- Mission proposal produces the exact envelope that will be issued, including the
  caveats the approval UX must state rather than imply.
- Capability tokens delivered by file, mode 0400, never in an argv or an
  environment. Unknown, expired, and revoked tokens are rejected identically.
- The egress proxy, the Clyde CA, and the in-sandbox forwarder. Profile `none` is
  the absence of a socket rather than a flag.
- `--json` for every command, driven through the real binaries by an integration
  test, because the CLI is also the integration-test harness (D13) and a renamed
  field is a broken caller.

### Phase 2 — snapshots and isolation

- One `SandboxSpec`, two backends. Bubblewrap argv construction, seccomp filters,
  limit wrapping, and Firecracker VM configuration are all pure functions over a
  spec, so what Clyde runs is a value a test can assert on.
- Access baselines with the two-tier model: subtree grants for first-party code
  and individually confirmed pins for everything outside them. Editing inside a
  grant produces zero drift and zero prompts, which is the property the whole
  control depends on.
- Structured failure classification that recognises host and policy conditions
  before anything is attributed to the user's code.

### Phase 3 — dependency resolution

- `rust.resolve-deps` with a manifests-only input snapshot and the
  `rust-registry` egress profile, refused on a host that cannot provide microVM
  isolation rather than downgraded.
- The code-execution inventory, checked before the sandbox starts, with a
  same-version content change reported distinctly from an upgrade.

### Phase 4 — the credential broker

- A broker that holds the credential in one field of one struct and validates
  every request on its own account, including reading the approval record rather
  than trusting the caller.
- Brokered push from a sanitised temporary repository. A fixture with eight
  hostile hooks, a transport rewrite pointing at a decoy remote, an
  object-transfer hook, and content filters executes nothing.
- The terminal client, and mission review, which verifies the audit chain as part
  of the review and sorts refused egress attempts to the top.
