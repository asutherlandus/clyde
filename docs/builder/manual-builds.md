# Manual Builds and Tests

Compiling and testing a Rust project through Clyde by hand, with no agent configured anywhere.

[quick-start.md](quick-start.md) walks a host from nothing to one `cargo check`. This document is the loop *after* that: what actually happens on each run, how to scope a run, how to read what comes back, and the three or four things that will genuinely trip you up. Everything here is the **operator** surface ([D25](decisions.md#d25-task-execution-has-an-operator-surface-on-the-admin-socket)) — `clyde` on the host, on the admin socket, authenticated by `SO_PEERCRED`.

Firecracker is not needed for any of it. `rust.check` and `rust.test.unit` run on the bubblewrap backend today ([INSTALL step 8](../../INSTALL.md#step-8-firecracker) is the standing exception, and it only gates `rust.resolve-deps`).

## Before you start

You need [INSTALL.md](../../INSTALL.md) steps 1–7 done, `clyded` running, and this from `clyde doctor`:

```
ok    workspace environment
ok    build and test tasks
```

The second line is the one that matters. It means clyded will admit a bubblewrap-backed task rather than refuse it on host capability. `kvm`, `firecracker`, and `dependency resolution` staying `NO` is expected and does not block anything below.

If `build and test tasks` is `NO`, stop and fix it — the two usual causes are the AppArmor user-namespace restriction ([INSTALL step 3](../../INSTALL.md#step-3-user-namespaces-and-apparmor)) and missing cgroup v2 delegation ([step 4](../../INSTALL.md#step-4-cgroup-v2-delegation)). Both are refusals by design, not degradations ([D22](decisions.md#d22-no-degraded-resource-limits-for-untrusted-execution)); there is no flag that runs the task anyway.

## What one run actually does

Worth holding in your head, because every surprise below follows from it:

1. You edit files in your **real project directory**, with your own editor. Clyde does not host you and does not intercept your writes.
2. `clyde task run` recomputes the dependency bundle's code-execution inventory — **before** anything starts.
3. It builds a fresh content-addressed **snapshot** of the live tree, containing the confirmed access baseline and nothing else, minus the absolute exclusions.
4. It bind-mounts that snapshot **read-only** at `/work`, with `cwd=/work`, plus a writable per-mission cache at `/cache` holding `CARGO_TARGET_DIR` and `CARGO_HOME`.
5. It runs cargo with `--frozen` and `CARGO_NET_OFFLINE=true`, in a loopback-only network namespace with no proxy socket bound at all, under cgroup limits, with no credentials.
6. It classifies the exit, stores stdout and stderr as artifacts, and appends to the audit chain.

Two consequences to internalise now. **Your `target/` directory is never the one being written** — build output lives in the mission cache and is destroyed at mission closeout ([D3](decisions.md#d3-build-caches-are-per-mission-and-writable-dependency-caches-are-read-only)). And **the snapshot is rebuilt every run**, so an edit is picked up with no action from you; mtimes are carried forward from the previous snapshot so incremental compilation stays correct ([D27](decisions.md#d27-snapshot-materialisation-preserves-change-ordering-in-mtime)).

### What is never in the snapshot

Applied before grants and not admissible by one:

| Excluded | Rule |
|---|---|
| `.git` anywhere | git is never a build input ([D21](decisions.md#d21-git-is-never-available-to-build-tasks)) |
| any `target/`, `node_modules/`, `dist/`, `build/` directory component | build output |
| `.env`, `.env.*`, `credentials.json`, `.netrc`, `.npmrc` | secret-shaped |
| `*.pem`, `*.key`, `*.p12`, `*.pfx`, `*.jks` | key material |
| `secrets/`, `.ssh/`, `.gnupg/`, `id_rsa*`, `id_ed25519*` | secret-shaped |
| symlinks | bundles and snapshots hold content, not references |

The `build/` rule is not hypothetical for Rust: `rustversion` and `thiserror` both keep probe sources in a directory literally named `build/`. It does not matter for crates that arrive as a dependency bundle — exclusions do not apply there — but it matters a great deal if you try to put a vendor tree in the snapshot. See [Dependencies](#dependencies-decide-this-first).

## Dependencies: decide this first

This is the gate. Everything else is mechanical.

Build tasks are offline **by construction**, not by convention: no route out of the sandbox, `CARGO_NET_OFFLINE=true`, and `--frozen`. A missing crate fails with `MissingDependencies` rather than quietly fetching one. And `rust.resolve-deps` — the task whose whole job is to fetch — is refused on every host today, because the vsock egress bridge its profile needs is unbuilt; the task is refused at preflight rather than run without the egress it was promised ([D24](decisions.md#d24-firecracker-is-the-default-backend-for-build-execution)).

So which case are you in?

### Case A — no external dependencies

A project whose `Cargo.lock` contains only its own members and path dependencies. Nothing to import, nothing to confirm; skip straight to [setup](#one-time-setup-per-project).

**This is the case the test suite covers**, in `tests/fixtures/`. If you are validating a fresh install, start here — `tests/fixtures/single-crate` is a two-file project with an empty lockfile and is the shortest path to a green run. Prove the pipeline on it before you fight the dependency question on a real project.

Register the fixture directory **as its own workspace**, not as a target inside the Clyde repo — see [register the cargo workspace root](#register-the-cargo-workspace-root).

### Case B — registry dependencies

You need a dependency bundle, and the only way to make one today is a host-side `cargo vendor` — a human running cargo outside Clyde, with network. That is the transitional shape of the fetch/compile split, and the `advisory` posture line is honest about it.

```sh
cd /path/to/project
cargo vendor "$PWD/vendor"                                    # outside Clyde; needs network
clyde deps import "$PWD/vendor" --lockfile "$PWD/Cargo.lock"  # absolute paths
clyde deps list
```

Absolute paths, because the daemon resolves them, not your shell. The import refuses a directory that does not satisfy the whole lockfile — a partially satisfying bundle would fail later as a confusing build error instead of here as a clear one. What it records: the lockfile digest, every crate by name, version and content hash, and the **code-execution inventory** — every package carrying a `build.rs` or a proc-macro crate type.

The bundle is content-addressed and immutable. At run time it is mounted read-only at `/deps`, and the mission's `CARGO_HOME` is seeded from it by hardlink (cargo writes lock files into `CARGO_HOME` even offline, so that copy has to be writable).

**Then cargo has to be told to use it.** Nothing writes this for you. Add a source replacement to the project's `.cargo/config.toml`, pointing at the in-sandbox mount path:

```toml
# .cargo/config.toml
[source.crates-io]
replace-with = "clyde-bundle"

[source.clyde-bundle]
directory = "/deps"
```

`.cargo/config.toml` is part of the build closure, so it is pinned into the snapshot automatically once you re-propose the baseline.

Three things to know about this, stated plainly:

- **It is derived from reading the code, not from a run.** Nothing in this repository has executed a real cargo build against a bundle — every isolation and build claim in the test suite is asserted over generated sandbox specifications, and every fixture is dependency-free. Treat the stanza above as the intended shape and expect to iterate. `clyde task logs <id> --stream stderr` after a `MissingDependencies` will tell you exactly which crate cargo could not resolve.
- **An absolute `directory = "/deps"` breaks host-side cargo**, which has no `/deps`. If you also build on the host, keep the stanza out of the committed config and apply it only for Clyde runs, or accept that the two build paths diverge.
- **`cargo vendor` output containing symlinks loses them** — `deps import` copies content and skips symlinks deliberately. Cargo's `.cargo-checksum.json` verification will notice.

Do not instead try to put `vendor/` in the snapshot as ordinary source. The exclusion rules will silently drop `vendor/rustversion/build/` and `vendor/thiserror/build/` (the `build/` directory rule), the `.pem` and `.key` fixtures that several crates ship, and every symlink — and cargo's checksum verification will fail on files it can see are missing, with an error that points nowhere useful.

### Case C — you want `rust.resolve-deps`

Refused, on every host, correctly. [INSTALL step 8](../../INSTALL.md#step-8-firecracker) lists the three missing guest-side pieces and [Part 1b](roadmap.md#part-1b-the-microvm-backend-as-the-default) is the spec. Use case B.

## One-time setup per project

### Register the cargo workspace root

```sh
clyde workspace register /path/to/project
clyde workspace list
```

Registration is identity, not authority. It mints the workspace id that missions, baselines, and bundles are keyed by. Registering the same root twice returns the existing id rather than minting a second.

**The root you register must be the root cargo would pick**, because the snapshot is mounted at `/work` with `cwd=/work` and cargo reads the manifest it finds there. A cargo project nested inside a larger repository — including one the outer `[workspace]` table lists under `exclude` — has to be registered on its own:

```sh
# right: the fixture is its own cargo project
clyde workspace register /path/to/clyde/tests/fixtures/single-crate

# wrong: registers the Clyde repo and builds the Clyde workspace
clyde workspace register /path/to/clyde
clyde task run rust.check tests/fixtures/single-crate
```

The wrong form fails in a way that is worth recognising, because the error does not name the cause. The closure walks up from the target to the outermost manifest carrying a `[workspace]` table and stops there — it does **not** consult that table's `exclude` list — so the baseline is computed against the outer workspace. For the Clyde repo that means a root manifest of `Cargo.toml`, all thirteen member manifests pinned, and only `tests/fixtures/single-crate` as source. Cargo then runs at `/work`, reads the outer manifest, and tries to build thirteen members whose sources are not in the snapshot; `-p solo` fares no better, since an excluded package is not a member of that workspace at all.

Nested roots do not conflict: nothing rejects registering a directory inside an already-registered workspace, and the two carry independent missions, baselines, and bundles.

### Open a mission

Nothing runs outside a mission, including yours. There is one active mission per workspace ([D16](decisions.md#d16-one-active-mission-per-workspace)), which is why `--mission` is rarely needed later.

```sh
clyde mission create \
  --workspace <workspace-id> \
  --edit . \
  --task rust.check \
  --task rust.test.unit \
  --expires-in 8h \
  "manual build and test session"

clyde mission approve <mission-id>
```

`create` prints the exact envelope that would be issued; `approve` issues it verbatim, so nothing is decided after approval. Approving does **not** start an agent — with no `[agent]` section in host configuration there is nothing to start, and that is a supported configuration.

Two choices worth making deliberately:

- **`--edit .`** grants the whole repository as a subtree, which is almost always what you want when the human at the keyboard *is* the driver. Narrowing it does not constrain you — you are editing outside Clyde regardless — it only means more of the build closure lands as individually confirmed pins. Narrow it when you want the review artifact to say something; leave it wide when you want to get work done.
- **`--expires-in`** is clamped to the configured `max_expiry` (8h in `config.example.toml`). A manual session should ask for hours, not the 2h default; renewing mid-build is a nuisance.

### Confirm an access baseline, per task and per target

A build task whose `(workspace, task, target)` has no confirmed baseline is **refused**, with the static proposal offered. There is no implicit wide-scope first run ([D18](decisions.md#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift)).

The unit is the triple. `--task` on every `clyde access` subcommand **defaults to `rust.check`**, so a test run needs its own baseline confirmed explicitly:

```sh
clyde access propose --workspace <ws> --task rust.check     .
clyde access confirm --workspace <ws> --task rust.check     .

clyde access propose --workspace <ws> --task rust.test.unit .
clyde access confirm --workspace <ws> --task rust.test.unit .
```

`propose` computes the closure and shows the two tiers; `review` prints a pending one again; `confirm` puts it in force. The tiers are the point: **subtree grants** come from the mission's approved scope and are not drift-sensitive, so ordinary editing inside them never prompts again. **Pins** are everything outside — the root `Cargo.toml`, `Cargo.lock`, `.cargo/config.toml`, `rust-toolchain.toml`, every member manifest cargo walks up to find — and each carries a reason.

If you are in [case B](#case-b--registry-dependencies), confirm the bundle inventory too, using the artifact id from `clyde deps list`:

```sh
clyde deps confirm-inventory <artifact-id> \
  --workspace <ws> --task rust.check --target .
```

Without it the first build is refused on unconfirmed inventory drift. That refusal is load-bearing: fetching a hostile crate is harmless until something executes it, and this confirmation is what sits in between.

## The inner loop

Edit in your editor. Then:

```sh
clyde task run rust.check .
clyde task run rust.test.unit .
```

That is the whole loop. No re-snapshotting step, no sync, no re-confirmation while the closure holds its shape.

With **more than one mission active across all workspaces**, this is refused rather than guessed at — running against the wrong mission charges the wrong budget and uses the wrong baseline. The error lists the candidates; disambiguate with either flag:

```sh
clyde task run rust.check . --workspace <workspace-id>
clyde task run rust.check . --mission <mission-id>
```

`--workspace` narrows the search and still requires the result to be unique; `--mission` names one outright. One active mission overall needs neither.

The run prints the record rather than the build output:

```
task_run     tr-01J...
task         rust.check
state        completed
classification  success
exit_code    0
posture      advisory
principal    operator
backend      bubblewrap
snapshot     s-blake3:...
policy_digest  ...
artifacts    a-..., a-...
```

`posture` and `principal` are recorded **on the run**, not looked up later, so a result never has to be read against today's configuration rather than the one in force at the time.

### Scoping a run inside a cargo workspace

The path argument selects the **baseline and snapshot**. It does not scope cargo. With no options, `rust.check` runs `cargo check --frozen --message-format=json` at `/work`, which means every default workspace member.

So if you baselined one member — `clyde access confirm ... crates/thing` — and run `clyde task run rust.check crates/thing`, cargo will try to compile the *whole* workspace against a snapshot that holds only `crates/thing` plus manifests, and fail on sources that are not there. Scope cargo explicitly to match:

```sh
clyde task run rust.check crates/thing --options '{"package":"thing"}'
```

Options are per-task JSON; the task tag is added for you. The full set:

| Task | Option | Effect |
|---|---|---|
| `rust.check` | `package` | `-p <name>` |
| `rust.check` | `all_targets` (bool) | `--all-targets` |
| `rust.test.unit` | `package` | `-p <name>` |
| `rust.test.unit` | `filter` | passed after `--` as the test-name filter |

```sh
clyde task run rust.check     . --options '{"all_targets":true}'
clyde task run rust.test.unit . --options '{"filter":"parses"}'
clyde task run rust.test.unit crates/thing --options '{"package":"thing","filter":"refresh_token"}'
```

`package` and `filter` are validated against a conservative character set before they reach cargo — alphanumeric, `-`, `_`. A filter needing anything else is rejected rather than passed through.

Unit tests run `cargo test --frozen --lib --bins`. Integration tests, doctests, and benches are not in the MVP catalog, which is closed ([tasks-and-policy.md](tasks-and-policy.md#the-mvp-catalog)).

### Reading the output

```sh
clyde task logs <task-run-id> --stream stderr
clyde task logs <task-run-id> --stream stdout
clyde task status <task-run-id>
clyde task list --mission <mission-id>
```

**`rust.check` output is on stdout, as JSON.** The task runs with `--message-format=json`, so stderr carries only cargo's progress lines and stdout carries one JSON diagnostic object per line. The human-readable text you actually want is the `rendered` field:

```sh
clyde task logs <id> --stream stdout --json \
  | jq -r '.content' \
  | jq -rj 'select(.reason=="compiler-message") | .message.rendered'
```

`rust.test.unit` does not use `--message-format=json`; read its stderr and stdout as you normally would.

Both streams are also stored as mission-linked artifacts, so they survive in the audit trail after the run.

## When a run is refused or fails

A denial is structured: what was denied, which constraint denied it, and what the narrower or escalated alternative is. The classes an operator actually meets:

| Class | Means | Do |
|---|---|---|
| `AccessBaselineMissing` | no confirmed baseline for this `(workspace, task, target)` | `clyde access propose` / `confirm` — check `--task` |
| `AccessBaselineDrift` | a read outside grants ∪ pins, or the bundle's code-execution inventory changed | read the diff — path drift and dependency drift render differently — then re-propose and re-confirm |
| `MissingDependencies` | offline cargo cannot proceed with the bundle present | [case B](#case-b--registry-dependencies); the summary names the crates |
| `GitMetadataUnavailable` | the build tried to read `.git` | not drift and not a bug in your code ([D21](decisions.md#d21-git-is-never-available-to-build-tasks)); drop the build-time git dependency |
| `ProjectCodeError` | your code does not compile, or a test failed | the ordinary case |
| `ResourceExhausted` | a cgroup or wall-clock limit fired | `[limits.build]` in host configuration |
| `IsolationUnavailable` / `CgroupLimitsUnavailable` | no backend meets the task's minimum isolation, or delegation went away mid-session | `clyde doctor`; [INSTALL step 4](../../INSTALL.md#step-4-cgroup-v2-delegation) |
| `EgressBlocked` | the proxy refused a destination | for an egress-`none` build this is a **finding**, not a routine error — something in your build tried to reach the network |
| `SandboxFailure` / `Internal` | Clyde's fault, not yours | that distinction is the reason outcomes carry a class at all |

`internal` with the summary *"exited with status 1 and produced no diagnostic Clyde could classify"*, finishing in milliseconds, is the shape of the sandbox failing to start — cargo never ran. Read stderr anyway: the wrapper chain writes there, and the real message is in it.

```sh
clyde task logs <task-run-id> --stream stderr
```

A build task's chain is `systemd-run --user --scope … prlimit … bwrap … cargo`, so a failure at that speed comes from one of the first three, and the message names which. `Failed to connect to bus` is the user manager being unreachable; `Creating new namespace failed` is the AppArmor restriction from [INSTALL step 3](../../INSTALL.md#step-3-user-namespaces-and-apparmor).

**When the closure changes shape** — you add a workspace member, add a path dependency, add `include_str!` of a new file — the baseline no longer covers it and the next run drifts. Re-propose and re-confirm; it is two commands and the normal course of events, not an error:

```sh
clyde access propose --workspace <ws> --task rust.check .
clyde access confirm --workspace <ws> --task rust.check .
```

For reads static analysis genuinely cannot see — `include_str!` of a computed path, a `build.rs` reading repo files — there is `clyde access learn`. It is a privilege, not a convenience: admin channel only, never reachable by an actor, single-run, distinctly marked in the audit log, and without effect until you confirm the proposal it produces. A learn run is a wide-scope execution of exactly the code you are trying to constrain, which is why the static proposal path exists to make it rare.

**When the lockfile changes**, the bundle no longer matches its digest and the build loses its dependencies. Re-vendor, re-import, re-confirm the inventory.

## Budget, renewal, and closing out

The mission carries a budget: `max_task_runs` (50 by default), `max_duration`, `max_cpu_seconds`, `max_cache_bytes`. A manual session burns runs faster than you would guess.

```sh
clyde mission status <mission-id>
clyde mission renew <mission-id> --extend-by 4h --additional-task-runs 100
```

When you are done:

```sh
clyde audit show --mission <mission-id> --high-signal --limit 50
clyde audit verify
clyde mission review <mission-id>
clyde mission close <mission-id>
```

`review` is the surface worth using: objective, files changed, every task run with its class and policy digest, escalations, approvals, egress attempts, budget consumed, the posture in force, and the closing diff. It verifies the hash chain and says loudly if it does not verify.

`close` records the closing diff and **destroys the mission cache** — so the next mission's first build is cold. That is deliberate ([D3](decisions.md#d3-build-caches-are-per-mission-and-writable-dependency-caches-are-read-only)); if you are iterating, keep one long mission rather than opening a new one per sitting.

The closing diff is also where an edit outside the mission's approved scope shows up. On a builder-only deployment it is **detected there, not prevented at the kernel** — nothing stopped your editor writing the file. That is the `advisory` posture, stated concretely.

## What this proves, and what it does not

It proves the pipeline end to end: an envelope approved before anything ran, a build closure turned into a confirmed baseline, an immutable snapshot, an offline compile in a sandbox with no credentials and no route out, structured failure classification, artifacts, and a hash-chained audit trail verified against its recorded head.

It does not make your host enforcing. `clyde doctor` ends with the posture and names every bypass it can see:

```
posture  advisory: project code can be run outside Clyde entirely
  - a project build toolchain is on PATH (cargo at /nix/store/…/bin/cargo), so project code can be built outside Clyde
  - no agent is hosted, so the driver's environment is the host's
```

Read that as the honest summary. A hostile dependency is contained: it runs against a read-only snapshot with no network and no credentials, whoever asked for the build. A careless or hostile *driver* is not, because nothing stops it running `cargo` directly — and in this workflow, the driver is you, and you did exactly that to produce the vendor directory. Closing that bypass is the warden, and it is not built ([D23](decisions.md#d23-the-mvp-splits-into-the-builder-and-the-warden)).

Posture is derived, never configured. No key makes an advisory deployment report as enforcing, and it participates in no admission decision ([D26](decisions.md#d26-enforcement-posture-is-explicit-reported-and-recorded)).

## See also

- [quick-start.md](quick-start.md) — the one-pass bring-up this loop follows from
- [INSTALL.md](../../INSTALL.md) — host setup, and why each prerequisite exists
- [tasks-and-policy.md](tasks-and-policy.md) — the task catalog, trust classes, baselines, egress
- [mission-and-lease.md](mission-and-lease.md) — missions, leases, budgets, escalation
- [roadmap.md](roadmap.md#part-1b-the-microvm-backend-as-the-default) — what Part 1b changes about dependencies and the backend
