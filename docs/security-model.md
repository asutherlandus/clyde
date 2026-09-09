# The Security Model

The [threat model](threat-model.md) says what may be hostile, what it wants, and which product answers which vector. This is its companion: **what Clyde actually enforces, by what mechanism, and how strongly**.

It is a map rather than a specification. Task semantics, baselines, and the egress mechanism are in [builder/tasks-and-policy.md](builder/tasks-and-policy.md); the authority model is in [builder/mission-and-lease.md](builder/mission-and-lease.md); field-level invariants are in [builder/schema.md](builder/schema.md). What this document adds is the shape of the whole thing in one place, plus two things that are hard to reconstruct by reading nine documents: an honest [grading](#how-strongly-each-control-holds) of which controls are structural and which are merely checked, and a retrospective [map](#lineage-and-prior-art) of where each one's established form lies.

Prerequisites: [threat-model.md](threat-model.md) and [terminology.md](terminology.md).

## The model in one sentence

> Nothing in Clyde carries ambient authority. Every effect is a **typed task**, requested by an identified **principal** under a **lease** inside a **mission**, admitted by a pure policy function, executed in an **environment** matched to how much the code is trusted, over **inputs a human confirmed**, with the network and every credential absent unless a policy names them — and every step of that is recorded in a hash-chained log.

Everything below is that sentence, expanded and graded.

## The chain of custody

```text
  human on the admin channel                  driver holding a session token
  (SO_PEERCRED, mode 0600 socket)             (CI, or an agent, on clyded.sock)
            |                                                |
            |  mission: objective, path scope, allowed       |
            |  tasks, budget, approval policy                |
            v                                                v
     +--------------------------------------------------------------+
     |  1. AUTHORITY   mission -> lease -> session -> principal     |
     |     re-read from the store on every request                  |
     +--------------------------------------------------------------+
                                  |
     +--------------------------------------------------------------+
     |  2. ADMISSION   validate_action(), a pure function           |
     |     lifecycle -> membership -> authority flags -> path       |
     |     scope -> policy resolution -> egress ceiling ->          |
     |     credential ceiling -> host capability -> baseline ->     |
     |     budget -> approval.   Fails closed; first reason wins.   |
     +--------------------------------------------------------------+
                                  |
     +----------------+-----------+------------+--------------------+
     | 3. CONTAINMENT | 4. INPUTS | 5. EGRESS  | 6. EFFECTS         |
     | environment,   | snapshot  | loopback-  | brokered push;     |
     | isolation      | material- | only ns +  | credentials never  |
     | level,         | isation;  | host-side  | inside a sandbox   |
     | runtime root   | exclusions| proxy      |                    |
     +----------------+-----------+------------+--------------------+
                                  |
     +--------------------------------------------------------------+
     |  7. EVIDENCE   hash-chained audit, task run record, posture, |
     |     egress attempts, failure class, mission review           |
     +--------------------------------------------------------------+
```

Each numbered layer is independent: a failure in one does not silently disable another. A lease wide enough to request `rust.check` still cannot make that check see a file the baseline excludes, reach the network, or hold a credential.

## 1. Authority — who may ask

**No actor holds ambient authority within Clyde.** A human has ambient authority over their own machine, and so does an agent Clyde does not host; the lease bounds what *Clyde* will do on their behalf, and [posture](#what-this-model-does-not-enforce) reports whether anything else bounds them.

| Control | Mechanism |
|---|---|
| One active mission per workspace, human-approved | [D16](builder/decisions.md#d16-one-active-mission-per-workspace) |
| A lease is time-bounded, path-scoped, task-scoped, egress-capped, credential-capped, budgeted | `crates/clyde-core/src/entities/lease.rs` |
| A derived lease is never wider than its parent in any dimension | `crates/clyde-policy/src/derive.rs::derive_lease`, eight rules, one pure function |
| Requests are authorised by a 256-bit session token, never by process ancestry, uid, or self-declared identity | [D2](builder/decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel), [R1](builder/decisions.md#r1-the-actor-token-binds-a-connection-and-is-re-resolved-on-every-request) |
| Only token hashes are stored; the plaintext type implements neither `Serialize` nor a revealing `Debug`, so it cannot reach a log or an API response | `crates/clyde-core/src/entities/session.rs` |
| Unknown, expired, and revoked tokens are rejected identically | `ActorSession::is_valid_at` |
| Approvals happen only on the admin socket, which is mode `0600`, `SO_PEERCRED`-checked, and never mounted into any sandbox | `bin/clyded/src/server.rs`, `bin/clyded/tests/security.rs` |
| `clyde approve` refuses to run inside a sandbox — the mechanism that makes agent self-approval impossible | `server::inside_sandbox` |
| Every side-effecting record names the `Principal` that caused it, which for an operator-driven run is not the lease's actor | [D25](builder/decisions.md#d25-task-execution-has-an-operator-surface-on-the-admin-socket) |

Two details carry more weight than their size suggests. Mission and lease are **re-read from the store** at the top of `run_task`, so a revocation stops the request in flight rather than the one after it. And a socket path inside a registered workspace root is **refused at bind time**, because such a socket could be bind-mounted into a sandbox along with the project tree.

## 2. Admission — what may be asked, and on what terms

The task catalog is **closed** and encoded as a Rust enum, so an unknown task is unrepresentable rather than a runtime lookup failure. There is no `shell.untrusted`: the typed path is the only path, and the escape hatch cannot become the default before first-class tasks exist.

Policy is a pure function over values. `clyde-policy` depends on neither the store, the sandbox layer, nor `tokio` — where a decision needs knowledge of the world (what the host can isolate, whether a baseline is confirmed) that knowledge arrives as an argument. Four functions carry most of the model and are the highest-value test targets in the project: `resolve_task_policy`, `derive_lease`, `egress_profile_order`, `charge_budget`.

`validate_action` checks in a deliberate order — lifecycle, membership, authority flags, path scope, policy resolution, egress ceiling, credential ceiling, host capability, baseline, budget, approval — so the reported reason is the most fundamental one rather than a downstream symptom. Three properties of the ordering matter:

- **Fails closed.** Every branch that cannot prove admissibility denies.
- **Refuses rather than degrades.** A task needing microVM isolation on a namespace-only host is denied, not run at the weaker boundary. cgroup v2 limits are mandatory for T2 and above with no rlimits-only fallback, because a flag that weakens a stated boundary tends to end up set ([D22](builder/decisions.md#d22-no-degraded-resource-limits-for-untrusted-execution)).
- **Refuses rather than clamps, where clamping would mislead.** An operator's `--isolation` request can only *raise* the policy floor; a request below it cannot express itself. Repository configuration that would widen authority is rejected with a diagnostic rather than silently narrowed ([D14](builder/decisions.md#d14-machine-readable-policy-in-clyde-agentsmd-advisory-only)).

Configuration layers built-in → host → user → repository, and **repository configuration may only narrow**. Some keys are rejected outright because narrowing is not meaningful on them — most importantly `agent.command`, since a repository that chooses what Clyde execs has arbitrary code execution in the workspace environment before any policy applies ([D20](warden/decisions.md#d20-the-agent-command-is-host-or-user-configuration-never-repository-configuration)).

Approvals bind to a **request digest**. A decision authorises the exact normalised request it was shown, so approving one destination, remote, or host list does not approve a later one; consumption is transactional and single-use ([R8](builder/decisions.md#r8-replay-protection-is-the-brokered-operations-state)).

## 3. Containment — where untrusted code runs

Four environments exist today, each with a distinct authority level and a distinct [runtime root](terminology.md#execution). (The research environment named in [terminology.md](terminology.md#research-environment) is post-MVP.)

| Environment | Holds | Isolation | Trust of what runs there |
|---|---|---|---|
| **Control plane** | trusted Clyde operations (commit preparation) | in-process | trusted |
| **Workspace** | editing, code manipulation, a hosted agent | namespace sandbox | semi-trusted — careless, not hostile |
| **Build** | fetch, compile, test, codegen | microVM by default ([D24](builder/decisions.md#d24-firecracker-is-the-default-backend-for-build-execution)) | **hostile** |
| **Broker** | push, sign, publish | separate process, no sandbox | trusted, and holds the only credential |

One line of that table is a decision ahead of the code. [D24](builder/decisions.md#d24-firecracker-is-the-default-backend-for-build-execution) makes the microVM the default for build execution, and the built floor for `rust.check` and `rust.test.unit` is still `NamespaceSandbox` — the microVM is opt-in per run with `--isolation microvm`, and only `rust.resolve-deps` names `MicroVm` as its floor. Part 1b raises both floors in the same change that makes the microVM path work; until then, read "microVM by default" as the policy target and `crates/clyde-policy/src/catalog.rs` as the current floor.

The separation that does the most work is **runtime roots as nix closures pinned by store path** ([D6](builder/decisions.md#d6-runtime-roots-are-nix-closures-not-oci-images)). The workspace root contains no `cargo`, no `rustc`, no `node`, no browser, no `gpg`, no `ssh`, no container client — not forbidden, *absent*. That is asserted by a test over the derivation's closure and again by the daemon before it starts an environment, so a hand-configured root cannot quietly reintroduce one. Where Clyde hosts the driver, this makes `run_task` the only path to project execution ([D1](warden/decisions.md#d1-the-coding-agent-runs-inside-a-clyde-managed-workspace-environment), [D17](warden/decisions.md#d17-workspaceedit-helper-execution-is-the-workspace-environment)).

**Both backends realise the same `SandboxSpec` contract**, and anything a backend cannot honour is a preflight failure, never a silent relaxation — the rule that stops the `SandboxBackend` trait becoming the place where boundaries quietly weaken.

- **Namespace backend** (bubblewrap): `--unshare-user --unshare-pid --unshare-ipc --unshare-uts --unshare-cgroup-try --unshare-net --clearenv --new-session --die-with-parent`, read-only closure binds, tmpfs scratch, and a seccomp filter passed on a file descriptor. The filter is a **deny list over an allow-by-default base** ([R5](builder/decisions.md#r5-the-seccomp-filter-is-a-deny-list-and-is-passed-on-the-childs-stdin)): an allow-list tight enough to be worth having would break on the next toolchain bump, and a filter that gets disabled to make builds work is worth nothing. Namespaces and cgroups remain the primary control.
- **MicroVM backend** (Firecracker): the file surface is block devices only, because Firecracker has no virtio-fs, no 9p, and no passthrough. Three properties are structural rather than configured — the `VmConfig` type **cannot express a guest network device**; a read-only drive is attached `is_read_only: true` with no path that relaxes it; and everything the guest emits is length-bounded before allocation, because the writer is a VM that has executed project code ([R11](builder/decisions.md#r11-all-guest-output-leaves-over-vsock)).

Resource limits are applied by **wrapping the command** rather than in a `pre_exec` hook, so the whole mechanism stays at argv level: no `unsafe`, and the exact command Clyde runs is a value a test can assert and an audit record can print ([R4](builder/decisions.md#r4-resource-limits-are-applied-by-wrapping-the-command)).

Ephemerality is part of containment: no VM is reused across runs, and the per-mission cache is destroyed at closeout ([D3](builder/decisions.md#d3-build-caches-are-per-mission-and-writable-dependency-caches-are-read-only)).

## 4. Input scope — what that code can see

**Enforcement is by materialisation.** The snapshot contains the granted subtrees minus absolute exclusions, plus the confirmed pins, and nothing else — so a read outside it fails with `ENOENT`. No tracing, no privilege, identical on both backends ([D18](builder/decisions.md#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift)).

The [access baseline](builder/tasks-and-policy.md#access-baselines) is authoritative in **Clyde's own state**, never in the repository: repository content is untrusted, and an attacker who could edit the baseline could conceal their own drift. It has two tiers, because the threat is dependency code changing and editing the project is the work rather than the threat — **subtree grants** for first-party code inside the mission's approved scope, not drift-sensitive; **file pins** for anything outside it, each individually confirmed with a recorded reason. A control that fires on ordinary editing gets clicked through, so ordinary editing never fires it.

**Absolute exclusions are applied before grants and are not admissible by one.** A grant over `backend/auth` does not admit `backend/auth/.env.local`. The canonical list — build output, secret-shaped names and extensions, `.git`, `.clyde/state`, plus anything configuration adds — lives in `crates/clyde-policy/src/exclusions.rs` and is checked on every path component, so `crates/core/target/debug/x` is excluded as surely as `target/debug/x`. Repository configuration may add exclusions and never remove them.

Three further input properties:

- **`.git` is never an input to a build task and is not pinnable** ([D21](builder/decisions.md#d21-git-is-never-available-to-build-tasks)). `SandboxSpec::validate` rejects any spec at T2 or above carrying a `GitDirectory` mount, so it cannot be reintroduced by a spec builder. A build that tries to read it gets its own failure class rather than being mistaken for drift or for a bug in the user's code.
- **The code-execution inventory is computed before the sandbox starts**, from the lockfile and dependency bundle: every crate with a `build.rs` or a proc-macro crate type, pinned by name, version, and source content hash. Pre-execution ordering is the whole point — a newly arrived build script is caught before it runs. A same-version, different-hash entry is a registry-tampering signal and is rendered distinctly.
- **Snapshots are read-only to tasks, and materialisation carries an mtime contract** as well as a content one ([D27](builder/decisions.md#d27-snapshot-materialisation-preserves-change-ordering-in-mtime)). Hardlink materialisation shares inodes with the content store, which makes the read-only bind load-bearing rather than stylistic.

**Learn mode is itself a privilege**: human-invoked on the admin channel only, never reachable by an actor, never a default and never a fallback Clyde selects on its own, scoped to one run, marked distinctly in the audit log, and without effect until a human confirms it. A learn run is a wide-scope execution of exactly the code being constrained, which is why the static-proposal path exists to make it rare.

## 5. Egress — what it can reach

Three constraints shape the mechanism: clyded runs unprivileged as the developer's own user; it must work identically on both backends; and the allowlist decision must be enforced in trusted code, because a decision enforced inside the sandbox is one hostile project code can remove — which would make the approval prompt a lie.

So: **every sandbox, under every profile including `none`, gets an unshared network namespace with loopback only**. Where a profile permits egress, the sandbox additionally gets one reachable endpoint and it is a proxy — a Unix socket bridged by a small trusted forwarder from the runtime root under bubblewrap, or a vsock port under Firecracker. The host-side proxy accepts HTTP `CONNECT` only, matches the target against the profile's allowlist, records every attempt either way, enforces byte and connection budgets charged to the lease, and emits a fetch manifest.

Under bubblewrap, **profile `none` is the absence of the socket** — there is no flag that disables egress, and `SandboxSpec::validate` rejects a spec carrying an egress socket under `none` *and* a spec missing one under any other profile. Under Firecracker the vsock device is unconditional because logs have nowhere else to go, so `none` means no host listener on the egress port rather than no channel at all ([D7 amendment](builder/decisions.md#amendment-the-guest-channel-is-a-single-vsock-multiplexed-by-port)). The two backends make slightly different structural claims for the same profile name, and the documentation and approval UX should not flatten them into one sentence.

The profile set is **closed**, ordered by a unit-tested partial order, and `none` is the default for every task type. `rust-registry` and `model-api` are deliberately **incomparable**: a lease holding one cannot derive a child holding the other, because that is an escalation evaluated against the mission rather than a derivation.

**TLS is pass-through everywhere except `model-api`.** Intercepting dependency traffic would make Clyde a plaintext-handling component in the path of dependency *content*, undermining the content-hash pinning baselines rely on. The one carve-out is bounded by giving the CA certificate only to workspace environments — `SandboxSpec::validate` rejects a CA mount at T2 or above, so a build sandbox that does not trust the CA cannot be transparently intercepted even by Clyde ([D11](warden/decisions.md#d11-workspace-environment-model-api-egress-goes-through-the-clyde-proxy)).

A refused attempt is a first-class signal. `rust.check` should never attempt egress; if it does, that is either a misconfiguration or a hostile dependency probing for a way out, and it surfaces in mission review either way.

## 6. Authority-bearing effects — brokered, never mounted

No environment that executes untrusted project code may sign, push, or publish. The credential broker is a **separate process from Phase 0**, so the boundary was never retrofitted ([D15](builder/decisions.md#d15-three-binaries-from-phase-0)), it is capability-oriented rather than secret-oriented, and it verifies the approval record itself rather than trusting its caller ([D8](builder/decisions.md#d8-brokered-gitpush-uses-the-developers-existing-credential-inside-the-broker-only)). Exactly one Clyde-side gateway may call it.

`MountPurpose::carries_credential()` returns `false` for every variant, and `SandboxSpec::validate` rejects any spec carrying a purpose for which it returns `true` — so credential absence is a property of the type rather than a review habit. A test enumerates every spec the system can generate and asserts that none reaches into the host home, `~/.ssh`, `~/.gnupg`, or a container runtime socket.

`git.push` validates in order: lease authority, remote in the configured allowlist, branch pattern with protected patterns refused outright, commit reachable with a tree matching what was approved, and an unexpired unconsumed approval whose digest equals this request's. A remote is approved **by name** and the URL comes from the broker's own configuration, never from the workspace repository ([R7](builder/decisions.md#r7-a-remote-is-approved-by-name-the-broker-resolves-the-url)).

**Hostile-repository hardening is the interesting part.** `.git/config` and `.git/hooks` are attacker-controlled content in this threat model, and `git push` executes local hooks and honours repository configuration — so a naive implementation runs untrusted code with credentials in scope. The broker pushes from a **sanitised temporary repository**: empty repo in broker-owned scratch, fetch the approved commit by path with hooks and `uploadpack.packObjectsHook` disabled, verify commit and tree against the approval, add the allowlisted remote explicitly, push with hooks disabled and `GIT_CONFIG_SYSTEM`/`GIT_CONFIG_GLOBAL` neutralised so `url.*.insteadOf` and `core.sshCommand` cannot redirect the transport, destroy the scratch. There is exactly one place in the codebase that constructs a git command, and it is hard to use unsafely ([R2](builder/decisions.md#r2-clyde-git-is-a-crate)).

Commit creation is a **trusted control-plane operation**, not a driver one, so an agent cannot plant hooks a later credentialed git run would execute.

## 7. Evidence — what makes the rest reviewable

A boundary you cannot audit is a boundary you cannot trust.

- **Append-only hash-chained audit** with a monotonic sequence number and `prev_hash`. The store API has no update or delete path. Payload construction goes through a redaction helper, and no path serialises a credential-bearing type into a payload — enforced by not implementing `Serialize` on those types.
- **Every task run records** mission, lease, actor, principal kind, policy digest, snapshot, dependency bundle, backend actually used, the isolation it ran at and separately what an operator asked for, egress attempts, artifacts, and the posture in force. The backend is written from the selection rather than assumed at admission: a run that happened in a microVM must not be recorded as having happened in a namespace sandbox.
- **Failure is classified**, because "Clyde is broken" must never be reported as "your code is broken". `SandboxFailure`, `PolicyDenied`, `EgressBlocked`, `GitMetadataUnavailable`, `MissingDependencies`, `ResourceExhausted` and `ProjectCodeError` are distinct outcomes derived from structured cargo diagnostics rather than from scraping human-readable output.
- **Posture is recorded on every run**, so review does not have to assume today's posture applied yesterday.

## How strongly each control holds

The most useful thing this document can add. Controls are graded by what an attacker would have to defeat.

| Grade | Meaning |
|---|---|
| **Structural** | The unwanted state is unrepresentable. Defeating it means changing Clyde's source, not its configuration or its inputs. |
| **Kernel-enforced** | A boundary the kernel or hypervisor holds at runtime. Defeating it means an escape. |
| **Checked** | A trusted-code decision on a path the untrusted side cannot reach. Defeating it means a bug in that code. |
| **Recorded** | Not prevented. Visible afterwards. |

| Control | Grade |
|---|---|
| Task catalog closed; no `shell.untrusted` | structural (Rust enum) |
| No guest network device on the microVM backend | structural (`VmConfig` cannot express one) |
| No credential mount in any sandbox | structural (`MountPurpose::carries_credential`, spec validation) |
| No `.git`, no CA certificate in a build sandbox | structural (spec validation at T2+) |
| Egress socket present iff the profile permits egress | structural (spec validation, both directions) |
| Workspace runtime root has no build toolchain | structural (closure test + daemon startup check) |
| Repository configuration cannot widen | structural (rejected with a diagnostic) |
| Derived lease never wider than parent | checked (pure function, denial-first tests) |
| Admission decision | checked (pure function, fails closed) |
| Read set limited to the baseline | kernel-enforced (`ENOENT` on a path that was never materialised) |
| Write set limited to the mission cache and lease edit paths | kernel-enforced (read-only binds) |
| Untrusted execution confined | kernel-enforced (namespaces + seccomp + cgroups, or the hypervisor) |
| Egress allowlist | checked, host-side, on a path the sandbox cannot reach |
| Approval binds to a request digest, single-use | checked + transactional |
| Approval cannot be self-issued by an actor | structural (separate socket, never mounted; `clyde approve` refuses inside a sandbox) |
| Dependency drift | checked pre-execution, then escalated to a human |
| Path drift under `ENOENT` inference | checked, but the *reporting* is heuristic ([OQ5](builder/decisions.md#oq5-per-open-enforcement-of-the-build-snapshot)) |
| Out-of-scope edit by a driver Clyde does not host | recorded (the closing diff), not prevented |
| Egress *content* | recorded (destination, bytes, budget), not controlled |
| Posture | recorded, and it participates in no admission decision |

## The trusted computing base

**Trusted:** both gateways, mission and lease managers, session manager, policy engine, approval manager, snapshot manager, baseline store, sandbox manager, egress proxy and its in-sandbox forwarder, broker gateway, credential broker, audit system. Plus, beneath them: the kernel's namespace and cgroup implementation, KVM and Firecracker, bubblewrap, nix and the store paths runtime roots name, and the host resolver the proxy consults.

**Semi-trusted:** a hosted coding agent, where the warden is deployed. Not hostile the way dependency code is, and not trusted either.

**Hostile by default:** project source, dependencies, build scripts, proc macros, tests, package install hooks, browser automation hooks, arbitrary repo scripts, agent-authored utility scripts, **repository git configuration and hooks**, `.clyde/policy.toml` as an input (hence narrow-only), and any output of untrusted execution until validated.

The in-sandbox egress forwarder is worth calling out: it is *listed* as trusted, but compromising it gains nothing, because it can only reach what the host-side proxy already permits.

## What this model does not enforce

Stated plainly, so the approval UX can state it too.

**The posture caveat.** Without the warden, nothing stops a driver executing project code outside the pipeline entirely. Everything above constrains what happens *when a task runs*, and a hostile dependency has no say in who requested the build — but a careless driver can bypass the pipeline. Clyde reports this as `advisory` posture and names the specific bypass ([D26](builder/decisions.md#d26-enforcement-posture-is-explicit-reported-and-recorded)). Posture is derived, never configured, and participates in no admission decision. It is [vector 4](threat-model.md#attack-vectors-and-which-product-answers-them) in the threat model, and closing it is the single reason the warden exists as a product ([D23](builder/decisions.md#d23-the-mvp-splits-into-the-builder-and-the-warden)).

**Egress is destination scoping, not content control.** An allowlisted host can be sent arbitrary bytes. A malicious allowlisted registry is not addressed by egress control — that is what snapshots, isolation, and the dependency inventory are for. Byte budgets bound bulk exfiltration and runaway downloads; they do not detect low-volume signalling. DNS is resolved host-side, which is a benefit, and it also means a compromised host resolver is out of scope.

**Denied-path reporting is inferential.** Under materialisation enforcement, denial and drift are the same event and Clyde infers the path from `ENOENT`. Exact reporting needs per-open enforcement ([OQ5](builder/decisions.md#oq5-per-open-enforcement-of-the-build-snapshot)).

**Not baselined in the MVP:** environment-variable reads and subprocess execs. Each would be a real improvement; neither is needed for the change-detection property.

**The baseline lives only in Clyde state.** It is therefore invisible to code review, not shared with a team or a fresh clone, and lost with that state. Export and import is worth adding post-MVP.

**Escapes are out of scope.** No guarantee against a kernel or hypervisor escape, and no claim of fully reproducible builds for every ecosystem on day one.

**Unbuilt, and refused rather than degraded:** the vsock egress bridge, which is why `rust.resolve-deps` is refused at preflight rather than run without the egress its profile promised; guest-side `fanotify` learn mode; and the raised policy floor that makes the microVM the default in practice as well as in policy.

**And the one that matters most today: no test has booted a guest, and no test spawns a sandbox on either backend.** Every isolation claim above is asserted over generated specifications, argv, filter bytes, and VM configuration rather than against a running kernel. `rust.check` has run once inside a real bubblewrap boundary, and that first run found four bugs the suite could not see. Expect the same on the microVM path. See [what has and has not been exercised](../README.md#what-has-and-has-not-been-exercised) and [the bring-up guide](builder/microvm-bring-up.md).

## Checking a claim

Every property above is meant to be verifiable rather than believed.

| Claim | Where to check it |
|---|---|
| Nothing carries a credential into a sandbox | `bin/clyded/tests/security.rs::no_specification_the_system_can_generate_carries_a_credential` |
| No build sandbox sees `.git`, the live workspace, or the CA | same file, `no_build_sandbox_receives_git_or_the_live_workspace` and `no_build_or_fetch_sandbox_receives_the_ca_certificate` |
| Only the mission cache is writable | `snapshots_and_bundles_are_read_only_and_only_the_cache_is_writable` |
| The admin socket is in no specification | `the_admin_socket_is_never_in_any_specification` |
| A token never reaches an argv or an environment | `a_token_never_appears_in_an_argv_or_an_environment` |
| A workspace root with a toolchain is refused | `a_workspace_runtime_root_containing_a_toolchain_is_refused` |
| Egress defaults to `none`; only push touches a credential | `crates/clyde-policy/src/catalog.rs` tests |
| Admission fails closed, in order | `crates/clyde-policy/src/resolve.rs` tests, denial cases first |
| Derivation never widens | `crates/clyde-policy/src/derive.rs` tests |
| Exclusions beat grants | `crates/clyde-policy/src/exclusions.rs` tests |
| Posture is derived, never configured, and never admits | `crates/clyde-core/src/entities/posture.rs` tests |
| Hostile git configuration and hooks do not execute | `crates/clyde-git/tests/hostile_repository.rs::a_hostile_repository_executes_nothing_during_a_brokered_push`, and `bin/clyded/tests/publish.rs` end to end |

## Lineage and prior art

**This section is a map added afterwards, not a derivation.** The decisions in [builder/decisions.md](builder/decisions.md) were made from the [threat model](threat-model.md) outward, and none of the work below was consulted while making them. It is recorded for two reasons: a reviewer who recognises a shape should be able to find its name, and a control with forty years of literature behind it deserves more confidence than one invented here.

**It is not a compliance claim.** Clyde is measured against none of these frameworks, produces no attestations, and has been assessed by nobody. Every gap in [what this model does not enforce](#what-this-model-does-not-enforce) is real regardless of what a row in the table below says.

### What each control instantiates

| Clyde control | Established form |
|---|---|
| No ambient authority; a lease *is* a capability; a derived lease is never wider than its parent | The **object-capability model** — Dennis and Van Horn (1966); the discipline as set out in Mark Miller's *Robust Composition* (2006). Lease derivation is capability **attenuation** under its usual name. Commit preparation being a control-plane operation rather than a driver one avoids Hardy's **confused deputy** (1988). |
| Authority by session token rather than process ancestry or uid; mission and lease re-read on every request | **NIST SP 800-207**, *Zero Trust Architecture* (2020) — per-request authorisation, no authority inherited from position, and the policy decision point separated from the enforcement point. |
| The control plane as sole mediator; `validate_action` fails closed | The **reference monitor** — Anderson (1972). *Complete mediation* and *fail-safe defaults* are two of the eight principles in Saltzer and Schroeder (1975), which most of the [design principles](builder/design.md#design-principles) restate. |
| The [trusted computing base](#the-trusted-computing-base) section | **TCSEC** (DoD 5200.28-STD, 1985) vocabulary, near-verbatim. |
| A closed typed task catalog; no `shell.untrusted` | The **Clark-Wilson integrity model** (1987) — an enumerated set of well-formed transformation procedures rather than arbitrary operations over the data. |
| The admin channel separate from the actor channel; `clyde approve` refusing inside a sandbox | **Separation of duty** — Clark-Wilson; NIST SP 800-53 AC-5; the multi-party authorisation pattern in Google's *Building Secure and Reliable Systems* (2020). |
| A broker exposing `git_push(commit, refspec)` and never `get_ssh_key()` | **Safe proxies**, from the same book. The same philosophy as a hardware security module: the key never leaves, and callers send it operations. |
| Snapshot inputs, offline compilation, ephemeral sandboxes, no VM reuse | **SLSA** build levels (Google, now OpenSSF) — hermetic, isolated builds; and the hermeticity discipline of Nix and Bazel, which the [runtime roots](builder/decisions.md#d6-runtime-roots-are-nix-closures-not-oci-images) already borrow. |
| Dependencies pinned by name, version, and content hash; same version with a different hash as a tampering signal | **in-toto** (NYU, CNCF) and **The Update Framework** supply the detection primitive. Clyde has the pinning and the diff; it does not yet have the attestations, which is [Phase 7](builder/roadmap.md). |
| Append-only, hash-chained audit with no update or delete path | NIST SP 800-53 **AU-9** and **AU-10**; the tamper-evident Merkle log as deployed by **Certificate Transparency** (RFC 6962). |
| The allowlist enforced host-side because in-sandbox enforcement is removable by the code it constrains | **Chrome's broker-process model** — the untrusted renderer asks, a trusted broker decides. The [reasoning in the egress model](builder/tasks-and-policy.md#three-constraints-shape-the-design) is the same reasoning. |
| A microVM per untrusted task | **Firecracker** as AWS uses it for Lambda and Fargate (Agache et al., NSDI 2020) — the deployment that made per-task hardware virtualisation ordinary rather than exotic. |
| Namespaces plus a seccomp deny list | Chrome's Linux sandbox; and bubblewrap itself, which comes from Flatpak. |
| Four environments graded by how much the code in them is trusted | **Qubes OS** — security by compartmentalisation. |

### The fetch/compile split has a specific ancestor

Separating the one task that may reach the network from every task that executes project code, and passing content-addressed dependencies between them, is what **Nix fixed-output derivations** do: network access is confined to a step whose output is pinned by hash, and every step that builds gets no network at all. Bazel's repository-rule and action split is the same shape. `rust.resolve-deps` producing an immutable, content-addressed [dependency bundle](builder/tasks-and-policy.md#rustresolve-deps) that `rust.check` consumes read-only and offline is that pattern applied to cargo.

### Standards and public-sector precedent

Nothing here obliges Clyde, but the direction is not idiosyncratic. **NIST SP 800-218** (the Secure Software Development Framework, v1.1) has a practice — PO.5 — for maintaining secure *environments* for software development, and PW.4 for verifying third-party components; both were driven by **Executive Order 14028** (2021). **NIST SP 800-161r1** covers supply-chain risk management for the same reasons. **SP 800-53 Rev. 5** supplies the control vocabulary the tables above borrow: AC-3, AC-5, AC-6, AU-9, AU-10, SI-7, and the SR family. On the public guidance side, the **CISA/NSA/ODNI Enduring Security Framework** developer guide (2022) recommends build isolation and dependency verification directly, and **CISA's Secure by Design** work supplies the framing that [posture reporting](builder/decisions.md#d26-enforcement-posture-is-explicit-reported-and-recorded) serves — secure defaults plus transparency about what is and is not guaranteed. The **EU Cyber Resilience Act**, in force since 2024 with its main obligations phasing in through 2027, is the regulatory form of the same argument.

### Why the threat model is not hypothetical

**xz-utils** (CVE-2024-3094, 2024) was a backdoor injected during the *build*, absent from the source tree a reviewer would read, and reachable only because build tooling executes arbitrary code. It is the clearest published instance of the claim in [Why Rust is a special case](threat-model.md#why-rust-is-a-special-case) — that the commands widely perceived as low risk are not — and of why the [code-execution inventory](builder/tasks-and-policy.md#code-execution-inventory) is checked before execution rather than after. `build.rs` and proc macros give cargo the same exposure with fewer steps.

### What has no clean precedent

Three things here look local, and retrofitting a citation onto them would be dishonest.

- **The two-tier grant/pin split** ([D18](builder/decisions.md#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift)). The load-bearing insight is a usability one — a control that fires on ordinary editing gets clicked through, so first-party edits must be structurally incapable of raising a prompt while dependency change remains drift-sensitive. Access-control literature generally treats granularity as a security dial rather than an attention budget.
- **Posture** ([D26](builder/decisions.md#d26-enforcement-posture-is-explicit-reported-and-recorded)). Deriving, reporting, and recording the fact that your own guarantee does not hold — and naming the specific bypass — is not a pattern with an established name. The nearest relative is the security-function versus assurance distinction in Common Criteria, which is a stretch.
- **The [structural / kernel-enforced / checked / recorded](#how-strongly-each-control-holds) grading.** It rhymes with the hierarchy of controls from safety engineering — eliminate before engineering before administrative — and with "make illegal states unrepresentable" from typed functional programming, rather than with anything in a security standard.

## Related reading

[threat-model.md](threat-model.md) for what is being defended against; [builder/tasks-and-policy.md](builder/tasks-and-policy.md) for the task catalog, baselines, and the egress mechanism in full; [builder/mission-and-lease.md](builder/mission-and-lease.md) for the authority model; [builder/schema.md](builder/schema.md) for entity invariants; [builder/decisions.md](builder/decisions.md) and [warden/decisions.md](warden/decisions.md) for why each control has the shape it does; [competitive-alternatives.md](competitive-alternatives.md) for what existing tools enforce instead.
