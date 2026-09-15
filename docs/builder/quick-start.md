# Builder Quick Start

From an empty terminal to one real `cargo check` executed by Clyde under an approved mission, a confirmed access baseline, and a verifiable audit trail.

This is a walkthrough of the commands, in the order they work. [INSTALL.md](../../INSTALL.md) is the reference for host setup and for *why* each prerequisite matters; [design.md](design.md) and [tasks-and-policy.md](tasks-and-policy.md) explain the model the commands operate on. Every command below also takes `--json` for a stable machine-readable shape, and `clyde tui` exposes the same surfaces interactively.

## What this proves, and what it does not

It proves the pipeline: a mission envelope approved before anything runs, a build closure turned into a confirmed baseline, an immutable snapshot, an offline `cargo check` in a sandbox with no credentials, structured failure classification, artifacts, and a hash-chained audit trail that `clyde audit verify` checks against its recorded head.

It does not prove the isolation boundary unless the host can provide one. Without unprivileged user namespaces and cgroup v2 delegation, tasks are refused rather than run weakened ([D22](decisions.md#d22-no-degraded-resource-limits-for-untrusted-execution)), and on a host where a `cargo` is on `PATH` the deployment posture is `advisory`: Clyde constrains what runs *through* it and cannot stop the driver building outside it ([D26](decisions.md#d26-enforcement-posture-is-explicit-reported-and-recorded)). `rust.resolve-deps` is refused on every host today — the vsock egress bridge its profile needs is unbuilt, so the task is refused at preflight rather than run without the egress it was promised ([D24](decisions.md#d24-firecracker-is-the-default-backend-for-build-execution), [INSTALL step 8](../../INSTALL.md#step-8-firecracker)) — so step 4 brings dependencies in by import instead.

## 0. Build

```sh
git clone <repository> clyde && cd clyde
nix develop                        # the only supported development entry point
cargo build --release
```

The host binaries are `clyde`, `clyded`, `clyde-brokerd`, and `clyde-forward`, and [INSTALL step 1](../../INSTALL.md#step-1-get-the-source-and-build) installs them. The build also produces `clyde-init`, which runs as PID 1 inside a microVM guest and reaches one inside the flake-built image rather than through an install step. Everything below assumes the flake devShell, which supplies `bwrap`, `sqlite`, and the Rust toolchain. Nothing should be installed globally — a host-global `cargo` inside the workspace runtime root is exactly what [D6](decisions.md#d6-runtime-roots-are-nix-closures-not-oci-images) keeps out.

## 1. Materialise the runtime roots and write host configuration

Runtime roots are nix closures, one per task family, and configuration names them ([D6](decisions.md#d6-runtime-roots-are-nix-closures-not-oci-images)):

```sh
sudo mkdir -p /nix/var/nix/gcroots/clyde
for root in workspace rust fetch; do
  sudo ln -sfn "$(nix build .#$root --print-out-paths --no-link)" \
    "/nix/var/nix/gcroots/clyde/$root"
done
```

The GC roots matter: a `nix-collect-garbage` deleting a root out from under a configured daemon shows up later as a runtime-root error mid-task.

Add the closure manifests as well, and name them as `sandbox.runtime_root_manifests`:

```sh
sudo ln -sfn "$(nix build .#runtimeRootManifests --print-out-paths --no-link)" \
  /nix/var/nix/gcroots/clyde/manifests
```

Without them a root's closure is taken to be the root alone. A runtime root is a `buildEnv` — a tree of symlinks into other store paths — so binding only the root leaves every binary inside the sandbox a dangling symlink.

Then copy the example host configuration, fill in the pinned `bwrap` path and the three roots, and validate it:

```sh
sudo install -Dm644 config.example.toml /etc/clyde/config.toml
sudo $EDITOR /etc/clyde/config.toml
clyded --check
```

`clyded --check` loads the layered configuration and probes the host, so malformed configuration fails here rather than at the first task. Unknown keys are rejected, never ignored. On Ubuntu 23.10+ the pinned `bwrap` needs an AppArmor profile before it can unshare — [INSTALL step 3](../../INSTALL.md#step-3-user-namespaces-and-apparmor) is the part most first runs get stuck on.

## 2. Check the host, then start the daemon

```sh
clyde doctor
```

Run it before the daemon: bring-up is exactly when `clyded` is *not* running. It distinguishes **blocked** (present but unusable, with a remedy) from **unavailable**, and ends with the deployment's posture and every bypass it can see. `ok` on user namespaces and cgroup delegation is the signal that tasks will be admitted.

```sh
clyded --log info
```

Under systemd, run it as a user unit — that is also what gives you the delegated cgroup subtree ([INSTALL steps 4 and 7](../../INSTALL.md#step-4-cgroup-v2-delegation)).

## 3. Register the project

```sh
clyde workspace register /path/to/rust/project
clyde workspace list
```

Registration is identity, not authority: it mints a workspace id that missions, baselines, and bundles are keyed by. Use that id for `--workspace` below.

## 4. Give the build something to compile

Build tasks are offline by construction — egress profile `none`, `CARGO_NET_OFFLINE=true`, and `--frozen` — so a missing dependency fails with `MissingDependencies` instead of quietly fetching one. Since `rust.resolve-deps` is not runnable yet, the path in is an import of a host-produced source directory:

```sh
cd /path/to/rust/project
cargo vendor "$PWD/vendor"                # a host operation, outside Clyde, and it needs network
clyde deps import "$PWD/vendor" --lockfile "$PWD/Cargo.lock"
clyde deps list
```

Absolute paths, because the import is resolved by the daemon rather than by your shell.

The import is an admin-channel operation. It refuses a directory that does not satisfy the whole lockfile — a partially satisfying bundle would fail later with a confusing build error rather than here with a clear one — and it records the lockfile digest, the crates by name, version, and content hash, and the **code-execution inventory**: every package with a `build.rs` or a proc-macro crate type. That bundle is content-addressed, immutable, and mounted read-only; the mission's `CARGO_HOME` is seeded from it by hardlink because cargo writes lock files into `CARGO_HOME` even offline ([D3](decisions.md#d3-build-caches-are-per-mission-and-writable-dependency-caches-are-read-only)).

Note what step this is: a human running `cargo vendor` on the host. It is the transitional form of the fetch/compile split, and the posture line is honest about it.

## 5. Propose and approve a mission

```sh
clyde mission create \
  --workspace <workspace-id> \
  --edit src \
  --task rust.check \
  --task rust.test.unit \
  --expires-in 2h \
  "make the refresh-token tests pass"

clyde mission approve <mission-id>
```

`create` prints the **exact envelope** that would be issued, including the caveats an approval has to state rather than imply — nothing is decided after approval. `--task` is repeatable; omitted, the mission allows the non-crossing set (`workspace.read`, `workspace.edit`, `repo.search`, `rust.check`, `rust.test.unit`). Push authority is present only when the mission explicitly allows `git.push`, because that is what sets `may_request_publish` on the lease. There is one active mission per workspace ([D16](decisions.md#d16-one-active-mission-per-workspace)), which is why `--mission` on a later run is usually unnecessary.

## 6. Confirm an access baseline

A build task whose `(workspace, task, target)` has no confirmed baseline is **refused**, with the static proposal offered. There is no implicit wide-scope first run ([D18](decisions.md#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift)):

```sh
clyde access propose  --workspace <workspace-id> --task rust.check src
clyde access review   --workspace <workspace-id> --task rust.check src
clyde access confirm  --workspace <workspace-id> --task rust.check src
```

The proposal separates the two tiers, and the split is the point: **subtree grants** cover first-party code inside the mission's approved scope and are not drift-sensitive, so ordinary editing there never produces a prompt; **pins** are everything outside them and each carries a reason. The closure routinely exceeds the edit scope — a workspace member needs the root `Cargo.toml`, `Cargo.lock`, every member manifest, and in-repo path-dependency sources.

Then confirm the bundle's inventory against that baseline, using the artifact id from `clyde deps list`:

```sh
clyde deps confirm-inventory <artifact-id> \
  --workspace <workspace-id> --task rust.check --target src
```

Without it, a first build is refused on unconfirmed inventory drift — correctly. Fetching a hostile crate is harmless until something executes it, and this confirmation is what sits in between. `clyde access learn` exists for reads static analysis cannot see (`include_str!` of an arbitrary path, a `build.rs` reading repo files); it is a privilege, admin-channel only, never reachable by an actor, single-run, and recorded distinctly.

## 7. Run the first task

```sh
clyde task run rust.check src
clyde task status <task-run-id>
clyde task logs <task-run-id> --stream stderr
clyde task list --mission <mission-id>
```

On the host there is no session token, so this is an **operator** request on the admin socket, authenticated by `SO_PEERCRED` ([D25](decisions.md#d25-task-execution-has-an-operator-surface-on-the-admin-socket)). Inside a workspace environment the same command carries a token and is an **actor** request. Admission is identical either way — same lease, policy, budget, baseline — and the record names the principal, so review never has to infer who asked:

```
task.requested … path=src principal=operator principal_detail=operator(uid 1000)
```

Underneath, the run you just did: recomputed the code-execution inventory **before** the sandbox started, built a content-addressed snapshot of the baseline and nothing else, ran `cargo check` against it read-only with a loopback-only network namespace and no proxy socket bound at all, wrote logs and outputs to the mission's own scratch and cache, and appended to the audit chain.

## 8. When a run is refused or fails

A denial is structured: what was denied, which constraint denied it, and what the narrower or escalated alternative is. The classes worth recognising on a first run:

| What you see | What it means | Next step |
|---|---|---|
| `AccessBaselineMissing` | no confirmed baseline for this target | step 6 |
| `MissingDependencies` | the offline build cannot proceed with the present bundle | re-import a bundle that satisfies the lockfile; `rust.resolve-deps` when it exists |
| `AccessBaselineDrift` | a read outside grants ∪ pins, or the code-execution inventory changed and is unconfirmed | read the diff — path drift and dependency drift are presented differently — then re-confirm |
| `IsolationUnavailable` / `CgroupLimitsUnavailable` | no backend meets the task's minimum isolation, or delegation went away | `clyde doctor`; [INSTALL step 4](../../INSTALL.md#step-4-cgroup-v2-delegation). Refusal is the correct behaviour |
| `GitMetadataUnavailable` | the build tried to read `.git`, which is never a build input ([D21](decisions.md#d21-git-is-never-available-to-build-tasks)) | drop the build-time git metadata dependency; this is neither drift nor a bug in your code |
| `EgressBlocked` | the proxy refused a destination | for an egress-`none` task this is a **finding**, not a routine error |

`ResourceExhausted` is a limit hit; `SandboxFailure` and `Internal` are Clyde's fault, not yours — which is the whole reason outcomes carry a class: "Clyde is broken" must never be reported as "your code is broken".

Boundary crossings raise an approval rather than failing outright:

```sh
clyde approvals list
clyde approvals approve <approval-id>            # or --for-mission, or deny
```

Where the human is also the driver, the escalation collapses and an approval is a **self-confirmation** — the prompt and the record are unchanged, and the prompt still has to explain why the previous profile failed.

## 9. Audit, review, close

```sh
clyde audit show --mission <mission-id> --high-signal --limit 50
clyde audit verify
clyde mission review <mission-id>
clyde mission close <mission-id>
```

`review` is the level you actually want: objective, files changed, tasks run with pass/fail and policy digests, escalations and outcomes, approvals, egress attempts, budget consumed, posture in force, and the closing diff, with drill-down into the timeline. It verifies the chain and says loudly if it does not verify. `close` records the closing diff — the out-of-scope-edit record, which on a host without a hosted workspace environment is *detected* here rather than prevented at the kernel.

## Driving it from something that is not a human

The actor socket (`<state-dir>/run/clyded.sock`) speaks MCP over line-framed JSON-RPC ([D10](decisions.md#d10-mcp-is-the-primary-actor-facing-api), [D19](decisions.md#d19-mcp-over-the-actor-socket-is-line-framed-json-rpc)): `run_task`, `task_status`, `task_logs`, `list_artifacts`, `list_capabilities`, `request_escalation`, `request_publish`, `commit_prepare`, `mission_status`. It is authenticated per request by a session token, and only hashes are stored.

Be aware of the current limit: session tokens are issued when Clyde hosts an agent, and there is no admin method for issuing one to an arbitrary external driver yet. So the operator surface above is how you drive the builder today; the actor path is exercised by the test suite rather than by a CLI command. The admin socket is never mounted into any sandbox and is mode `0600` with `SO_PEERCRED` checks, which is what makes self-approval structurally impossible rather than prohibited — `clyde approve` and `clyde access confirm` refuse to run inside a sandbox ([D2](decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel)).

Brokered `git.push` ([D8](decisions.md#d8-brokered-gitpush-uses-the-developers-existing-credential-inside-the-broker-only)) needs `clyde-brokerd` plus `[push]`, `[broker]`, and `[broker.remotes]` in host configuration — without `push.remotes` every push is refused before a human is even asked — and its request surface is the actor's `request_publish`, so it is not reachable from the operator CLI yet. Its security properties, including the hostile-`.git/config`-and-hooks hardening, are implemented and asserted against a fixture repository whose hooks and configuration are hostile.

## Where things live

State defaults to `$CLYDE_STATE_DIR`, else `$XDG_DATA_HOME/clyde`, else `~/.local/share/clyde`, else `/var/lib/clyde`:

```
db.sqlite            the store, including the audit chain
blobs/  snapshots/   the content-addressed snapshot store
deps/                read-only dependency bundles
missions/            per-mission caches, destroyed at closeout
logs/tasks/<id>/     per-task logs
run/clyded.sock      actor socket          run/clyded-admin.sock  human socket
```

Keep the path short: Unix socket paths have a hard 107-byte limit, and a long `--state-dir` fails at bind with a message naming it.

## Next

- [manual-builds.md](manual-builds.md) — the compile/test loop this walkthrough leaves you in, and the dependency question it defers
- [design.md](design.md) — the architecture, requirements, and interaction flows
- [tasks-and-policy.md](tasks-and-policy.md) — the task catalog, trust and runtime classes, baselines, the egress model
- [mission-and-lease.md](mission-and-lease.md) — missions, leases, budgets, escalation
- [schema.md](schema.md) — entities, state machines, invariants
- [roadmap.md](roadmap.md) — what is built, and what [Part 1b](roadmap.md#part-1b-the-microvm-backend-as-the-default) changes about steps 4 and 7
- [decisions.md](decisions.md) — the decisions this walkthrough is the surface of
