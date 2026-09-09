# Tasks, Policy, Baselines, and Egress

What a task is, what policy decides about it, what a build task is allowed to read, and how network reachability is enforced.

The matrix exists to prevent Clyde collapsing back into "run arbitrary shell in a container". A mission or lease may authorize `rust.check`, but policy still determines exactly how it runs and in which environment — which is what stops a broad lease being converted into arbitrary execution semantics.

## Policy dimensions

Each task profile defines: **environment**, **trust class**, **minimum isolation level**, **input scope**, **writable outputs**, **egress profile**, **credential policy**, **cache policy**, **access baseline requirement**, **human approval policy**, **agent autonomy policy**, and **audit level**.

## Trust classes

| Class | Meaning |
|---|---|
| **T0** | Read/edit only. No project code execution, no credentials, no network. Safe for broad autonomy within a lease. |
| **T1** | Low-authority editing support. Trusted built-in tools or agent-authored edit helpers. Project build/test/install code is not executed. May operate on the live workspace within lease scope, in the workspace environment — whose runtime root has text and code manipulation tooling and **no** project build toolchain. |
| **T2** | Untrusted offline execution. Project code may execute; no external network, no credentials. The main inner-loop class for compile, unit test, and build. |
| **T3** | Constrained external execution. Project code may execute with tightly scoped network or synthetic services; no raw credentials. Escalation usually required unless pre-approved. |
| **T4** | Authority operations. No untrusted project code in the same context; brokered credentials and privileged effects — push, sign, publish, real identity access. Strong approval and audit requirements. |

## Runtime classes

A task policy names the **minimum** acceptable class. The sandbox manager may select something stronger and never weaker.

| Class | What it is | Used for |
|---|---|---|
| **R0** | In-process trusted operation | control-plane actions: diff computation, commit preparation, policy resolution |
| **R1** | Trusted tool runner | trusted binaries that inspect or transform files without executing repo code |
| **R2** | Namespace sandbox — bubblewrap: user/pid/ipc/uts/cgroup/net namespaces, read-only binds, tmpfs scratch, seccomp, cgroup v2 limits ([D5](decisions.md#d5-bubblewrap-first-behind-a-sandboxbackend-trait)) | the workspace environment, and build/test during Part 1a bring-up. **Not** acceptable for any task with an egress profile other than `none`, nor for T3 tasks |
| **R3** | MicroVM — Firecracker, no guest network device, vsock-bridged egress ([D24](decisions.md#d24-firecracker-is-the-default-backend-for-build-execution)) | **the default for every build task**: compile, test, dependency fetch, browser, arbitrary repo execution |
| **R4** | Credential broker — a separate process that never runs project code | push, signing, publish, credential-mediated actions |

## Global policy defaults

Unless a task explicitly says otherwise:

- source inputs come from immutable snapshots, scoped to the cargo build closure of the requested path
- repo access is subtree-scoped where possible
- outputs are written to explicit output channels
- the host home directory is never mounted; nor are `~/.ssh`, `~/.gnupg`, browser profiles, cloud config, or container runtime sockets, into **any** sandbox
- the egress profile is `none` — a loopback-only network namespace with no proxy socket bound at all
- credentials are unavailable
- caches are read-only, except the per-mission build cache
- tasks that execute project code require a confirmed access baseline, and receive the granted subtrees minus absolute exclusions, plus any confirmed pins
- `.git` is never an input to a build task ([D21](decisions.md#d21-git-is-never-available-to-build-tasks)), and cgroup v2 limits are mandatory for T2 and above ([D22](decisions.md#d22-no-degraded-resource-limits-for-untrusted-execution))
- every task is attributable to mission + lease + actor + session, and records the posture in force

## The MVP catalog

The catalog is **closed** and encoded as a Rust enum, so an unknown task type is unrepresentable rather than a runtime lookup failure.

| Task | Product | Trust | Environment | Min runtime | Input | Egress | Cache | Approval |
|---|---|---|---|---|---|---|---|---|
| `workspace.read` | warden | T0 | workspace | R0/R2 | live, lease-scoped | `none` | none | none |
| `workspace.edit` | warden | T0/T1 | workspace | R2 | live, lease-scoped | `none` | none | none |
| `repo.search` | warden | T0 | workspace | R2 | live, lease-scoped | `none` | none | none |
| `rust.check` | builder | T2 | build | R2 → R3 | snapshot, baselined | `none` | mission-scoped | none, baseline required |
| `rust.test.unit` | builder | T2 | build | R2 → R3 | snapshot, baselined | `none` | mission-scoped | none, baseline required |
| `rust.resolve-deps` | builder | T3 | build | R3 | manifests + lockfile only | `rust-registry` | writes dep bundle | human |
| `git.commit.prepare` | builder | T1 | control plane | R0 | live working tree | `none` | none | none |
| `git.push` | builder | T4 | broker | R4 | commit id + refspec | `broker` | none | human |

"R2 → R3" is a **transition, not a steady state**: [Part 1a](roadmap.md#part-1a-snapshots-and-the-namespace-backend) runs these on the namespace backend during bring-up because that is a floor it can meet without guest images, and [Part 1b](roadmap.md#part-1b-the-microvm-backend-as-the-default) raises both floors to `MicroVm` in the same change that makes the microVM path work. Deferring the raise would leave a weak floor in a closed catalog after the stronger backend exists, and some deployment would keep running against it.

The catalog is one closed enum in both deployments. A builder-only deployment has no policy path reaching the three `workspace.*` / `repo.*` types; they are **inert rather than removed**, because the catalog's exhaustiveness is a tested property and churning it buys nothing.

`artifact.sign` and `artifact.publish` are post-MVP. `shell.untrusted` is excluded on purpose, so the typed task path is the only path and the escape hatch cannot become the default before first-class tasks exist.

## Task semantics

### `rust.check` and `rust.build`
Compile and type-check Rust code, including proc macros and `build.rs`. Source is an immutable snapshot limited to the confirmed access baseline; writes are build outputs and logs only; no network, no credentials; read-only dependency inputs plus a per-mission writable build directory; a confirmed baseline is required and drift escalates.

Offline by construction: egress `none`, `CARGO_NET_OFFLINE=true`, `--offline --frozen`. A missing dependency must **fail** rather than trigger a fetch — that failure is the entry point to dependency resolution. `rust.build` adds an explicit artifact directory; package signing stays separate.

*Threat note:* this is where hostile transitive dependency code most plausibly executes. The controls that matter are the pre-execution code-execution inventory check and the baselined read set, not the task's apparent simplicity. It should be a standard pre-authorized inner-loop task.

### `rust.test.unit` and the integration variants
Unit tests and doctests run in isolated no-network sandboxes, writing logs, coverage, and failure artifacts. `rust.test.integration.synthetic` (T3, synthetic network only, no credentials) is preferred during autonomous loops. `rust.test.integration.external` (T3, destination-scoped external, short-lived scoped tokens only if absolutely required) always needs approval.

### `rust.resolve-deps`
Retrieves crates and toolchain inputs for later networkless compilation.

- **input snapshot**: manifests, `Cargo.lock`, and cargo configuration only — *not* the full source tree. A fetch task has no reason to see application code, and narrowing the input narrows what a compromised fetch can exfiltrate through an allowlisted registry connection
- **egress**: the `rust-registry` profile
- **outputs**: a dependency bundle artifact and a fetch manifest, and nothing else
- **credentials**: none. Private sources are out of MVP scope and are denied rather than half-supported
- **approval**: human by default. Public mirrored dependencies may be pre-approved by configuration; private git dependencies, lockfile drift, and broader network never are

Cargo runs with `--locked` so the lockfile is authoritative and resolution cannot drift during a fetch.

The **dependency bundle** is content-addressed, immutable, and mounted read-only wherever used. It records the `Cargo.lock` digest it satisfies, the crates by name, version, and content hash, the registries each came from, and the fetch manifest including every egress attempt. A build task states which bundle it used, so "what were the inputs to this build" is answerable from the task run alone.

`node.resolve-deps` follows the same pattern for npm/pnpm/yarn (post-MVP).

### `git.commit.prepare`
A trusted control-plane operation, not a driver operation. It computes the scoped diff from the live working tree, builds a commit proposal (message, author, file list, diff summary, and the tasks that passed against this tree), creates the local commit on human approval using sanitised git invocations, and stores the proposal as a mission-linked artifact.

### `git.push`
The request carries repository, commit id, remote name, and refspec. Validation, in order:

1. lease authority `may_request_publish`
2. remote is in the configured allowlist
3. branch matches allowlisted patterns; protected-branch patterns are refused outright
4. the commit exists, is reachable, and its tree matches what was approved
5. an approval decision exists whose `request_digest` equals this request's normalised digest, unexpired and unconsumed

Approval consumption and push execution are transactional. A remote is approved **by name**; the URL comes from the broker's own configuration, never from the workspace repository ([R7](decisions.md#r7-a-remote-is-approved-by-name-the-broker-resolves-the-url)).

**Hostile repository hardening.** `.git/config` and `.git/hooks` are attacker-controlled content in this threat model, and `git push` executes local hooks and honours repository configuration — so a naive implementation runs untrusted code with credentials in scope. The broker therefore pushes from a **sanitised temporary repository**: create an empty repo in broker-owned scratch; fetch the approved commit from the workspace repository by path with hooks and object-transfer hooks (`uploadpack.packObjectsHook`) disabled; verify the fetched commit id and tree match the approval; add the allowlisted remote explicitly, ignoring workspace remote configuration; push with hooks disabled and system and global git configuration neutralised (`GIT_CONFIG_SYSTEM=/dev/null`, `GIT_CONFIG_GLOBAL=/dev/null`) so `url.*.insteadOf`, `core.sshCommand`, and similar rewrites cannot redirect the transport; destroy the scratch repository.

Every trusted git invocation in Clyde uses the same sanitised helper. There is exactly one place in the codebase that constructs a git command, and it is hard to use unsafely ([R2](decisions.md#r2-clyde-git-is-a-crate)).

`git.fetch`, `artifact.sign`, and `artifact.publish` follow the same brokered pattern and are post-MVP.

### Post-MVP task families
`format.trusted` and `lint.static` (T1/R1, snapshot or scoped live input, logs or scoped repo writes, no network, no credentials — reclassified to T2/T3 if the tool executes project plugins or untrusted config); `web.build` (T2, offline, static assets and bundles); `browser.test.synthetic` and `browser.test.external` (T3, isolated browser state, synthetic or scoped test identity, never the developer's profile); `service.run.synthetic` (T3, ephemeral internal-only fake SMTP, fake OAuth, ephemeral Postgres/Redis/S3); `artifact.package` (T2, build outputs in, packages out, separate from signing and publish).

## Inner-loop versus boundary-crossing

**Safe inner loop**, allowed inside a lease without repeated approval while within scope and budget: `workspace.read`, `workspace.edit`, `repo.search`, `format.trusted`, `lint.static`, `rust.check`, `rust.build`, `rust.test.unit`, `web.build`, `artifact.package`, and the synthetic test tasks where the mission already permits them.

**Boundary crossing**, requiring escalation, special mission policy, or direct human approval: `rust.resolve-deps`, `node.resolve-deps`, the external test variants, any request for broader repo scope, any request for real external identities or credentials, credentialed `git.fetch`, `git.push`, `artifact.sign`, `artifact.publish`, and `shell.untrusted` with any network access.

Where the human *is* the driver the escalation step collapses and the approval becomes a self-confirmation — the prompt and the record stay ([D2 amendment](decisions.md#amendment-confirmation-semantics-when-the-operator-is-the-driver)).

## Access baselines

Build and test tasks run against a pinned **access baseline** ([D18](decisions.md#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift)). The primary threat is code arriving in a transitive dependency and executing during compilation, and what matters is detecting **change**.

The baseline is authoritative in **Clyde's own state**, keyed by workspace, task type, and build target. It is never stored in the repository, because repository content is untrusted and an attacker who could edit the baseline could conceal their own drift.

### Repo read set: two tiers

The threat is dependency code changing; editing the project is the work, not the threat. Treating both at the same granularity would make an agent adding a source file indistinguishable from a build script reaching somewhere new — and a control that fires on ordinary editing gets clicked through.

**Subtree grants — first-party project code, not drift-sensitive.** The mission-approved edit and read scopes, plus the build-closure subtrees within that scope: workspace root manifests, member manifests, in-repo path dependencies. Creating, renaming, moving, or deleting files inside a grant is ordinary work: materialised on the next snapshot, no prompt, no drift, no amendment. The human's review happened once, over the mission envelope's scope.

**File pins — everything outside those subtrees, drift-sensitive.** An `include_str!` target in `docs/`, a shared fixture directory, a single sibling crate a build script reads. Each is confirmed individually and carries a recorded reason, so a later reviewer can tell why the build reaches outside the project's approved scope. Rollup to a `SubtreePin` is how many pins under one out-of-scope directory are rendered.

**Absolute exclusions**, applied *before* grants and not admissible by one:

| Excluded | Reason |
|---|---|
| `target/`, `node_modules/`, `dist/`, `build/` | build output; belongs in the mission cache, not the snapshot |
| `.env`, `.env.*`, `*.pem`, `*.key`, `id_rsa*`, `credentials.json`, `secrets/` | secret-shaped; never an input to a build |
| `.git` | never available to build tasks, not pinnable ([D21](decisions.md#d21-git-is-never-available-to-build-tasks)) |
| `.clyde/state` if present | Clyde's own state must never be a build input |
| configured additions from `snapshot.exclusions` | repository config may **add** exclusions and never remove them |

A grant over `backend/auth` does not admit `backend/auth/.env.local`. This is the authoritative exclusion list; other documents refer here rather than restating it.

### Build-closure computation

Cargo cannot build a subtree in isolation. The closure for a task on `backend/auth` is:

- the workspace root `Cargo.toml` — `version.workspace`, `dependencies.*.workspace`, and `[lints] workspace` all resolve against it
- `Cargo.lock`, plus `.cargo/config.toml` and `rust-toolchain.toml` from the root, since cargo discovers them by walking upward
- **every** workspace member's `Cargo.toml`, because cargo constructs the whole workspace graph before building anything and fails if a listed member's manifest is unreadable — manifests only, not their `src/`
- full source for the target crate and its transitive in-repo path dependencies, including their `build.rs`

This routinely exceeds the lease's edit scope, and that is expected: read-only input is a confidentiality delta, not an authority one, since the write set remains the lease's edit paths. Getting closure computation wrong is the most likely cause of "Clyde can't build my project", so it deserves fixture tests across single-crate, virtual-workspace, inherited-workspace-dependency, and nested-path-dependency layouts.

The closure is the **starting point for a baseline**, not the snapshot contents.

### Code-execution inventory

Computed **before the sandbox starts**, from the lockfile and the dependency bundle: every package with a `build.rs` or a proc-macro crate type, pinned by crate, version, and source content hash. Pre-execution ordering is the point — a newly arrived build script is caught before it runs, not after.

### Enforcement

**Repo reads are enforced by materialization.** The snapshot contains the granted subtrees minus exclusions, plus the pinned files, and nothing else, so a read outside fails with `ENOENT`. No tracing, no privilege, identical on both sandbox backends. Because grants are subtrees, newly created first-party files are picked up automatically on the next snapshot — which is what keeps the inner loop free of prompts.

**Inventory is enforced pre-execution**, from the lockfile and bundle before the sandbox starts.

### Establishing a baseline

1. **Static proposal — the normal path.** Clyde computes the build closure and proposes it as the initial baseline for human confirmation.
2. **Learn mode — the fallback.** For reads static analysis cannot see (`include_str!` to an arbitrary path, a `build.rs` reading repo files), a human runs the task with auto-admit within the mission's read scope while Clyde observes actual reads, producing a proposal for review.

Learn mode is itself a privilege: human-invoked on the admin channel only, never reachable by an actor, never a default and never a fallback Clyde selects on its own, scoped to a single run, marked distinctly in the audit log, and without effect until a human confirms it. The static-proposal path exists specifically so learn mode is rarely needed, since a learn run is a wide-scope execution of exactly the code being constrained.

**Observation mechanism.** On the namespace backend, host-side `inotify` (`IN_OPEN`/`IN_ACCESS`) on the materialised tree — unprivileged and adequate, subject to watch limits on large trees. On the microVM backend, host-side inotify cannot see guest reads, so observation runs **inside the guest** with `fanotify` over the mounted snapshot, reporting the read set back over the job channel; guest root is unproblematic because the VM is the boundary. Either way the observation reports how many directories it could not watch, so an incomplete observation cannot be mistaken for a complete one ([R3](decisions.md#r3-learn-mode-observation-uses-the-inotify-crate)).

A task with no baseline for its target is **refused**, with the static proposal offered. There is no implicit wide-scope first run.

### Drift

In enforce mode, denial and drift are the same event: a build wanting something outside grants ∪ pins fails, and Clyde renders that as an escalation naming the path rather than surfacing a raw cargo error. Under `ENOENT` inference this is a heuristic; exact denied-path reporting needs per-open enforcement ([OQ5](decisions.md#oq5-per-open-enforcement-of-the-build-snapshot)).

**Not drift:** a new, renamed, moved, or deleted file inside a granted subtree; a new module, test, or fixture inside one; a new in-repo path dependency on a crate *inside* the approved scope.

**Drift:** a read outside grants ∪ pins — including a new in-repo path dependency on a crate *outside* the approved scope, which is a genuine widening of what the mission reaches into; a new code-executing crate; a version change to one; a content-hash change at the same version, which is a registry-tampering signal.

The two classes are presented differently because they carry very different signal. **Path drift** should be uncommon once grants are set. **Dependency drift** is the one this control exists for, checked before execution and reported as an inventory diff:

```text
Lockfile: 4 additions, 1 version change, 0 source changes
Code execution at build time:
  + serde_derive_internals 0.29.1   (new proc-macro crate)
  ~ ring 0.17.8 -> 0.17.9           (build.rs content changed)
  ! zstd-sys 2.0.10                 (same version, different source hash)
```

A lockfile addition of a crate that never executes code is a materially different risk from one that runs a build script, and the human should not have to work that out from crate names. The `!` case is a tampering signal and should be visually distinct. The prompt shows the **diff against the pinned baseline**, not the whole baseline, so it stays readable as the dependency graph grows.

Configuration may pre-approve the low-risk classes for a mission; a repository may narrow that but never widen it. Git dependencies and unknown registries are never pre-approvable.

### CLI

Admin channel only: `clyde access show`, `clyde access propose`, `clyde access learn`, `clyde access review`, `clyde access confirm`, `clyde access reset`.

### Not baselined in the MVP

Environment-variable reads and subprocess execs. Each would be a real improvement and neither is needed for the change-detection property.

## Failure classification

Task outcomes carry a structured `TaskFailureClass` from Part 1a, because the escalation flow keys off it and because "Clyde is broken" must never be reported as "your code is broken":

| Class | Meaning |
|---|---|
| `Success` | — |
| `ProjectCodeError` | compilation or test failure in project code |
| `MissingDependencies` | cargo cannot proceed offline with the present cache; drives the fetch escalation |
| `PolicyDenied` | blocked by lease or policy before execution |
| `EgressBlocked` | the proxy denied a destination. For a `none`-profile task this is a **finding**, not a routine error |
| `GitMetadataUnavailable` | the build tried to read `.git`, which is never available ([D21](decisions.md#d21-git-is-never-available-to-build-tasks)). Diagnosed specifically, because it is neither drift nor a bug in the user's code |
| `ResourceExhausted` | memory, CPU, wall clock, or task count |
| `SandboxFailure` | backend or host problem, not the project's fault |
| `Internal` | — |

Classification is derived from exit status plus structured cargo diagnostics — invoke with `--message-format=json` and match on diagnostic codes rather than scraping human-readable output, which changes between toolchain versions.

## The egress model

"network: none" and "network: registry-only" appear throughout the design as policy values. This section makes them mechanical.

### Three constraints shape the design

1. **No privilege.** clyded runs as the developer's own user. Rootless network namespaces cannot create veth pairs into the host, so the usual approach — put the sandbox on its own interface and filter with `nftables` — is unavailable without root or a setuid helper.
2. **One mechanism, all backends.** It must work identically under bubblewrap and Firecracker, or the plumbing gets rewritten mid-roadmap. That matters more now the microVM is the default rather than the exception.
3. **The allowlist decision must be trusted.** If it is enforced by code inside the sandbox, hostile project code can remove it, and the approval prompt is lying to the user.

### Mechanism

**Sandbox side.** Every sandbox, for every profile including `none`, gets an **unshared network namespace with loopback only**: no route to the host network, no reachable DNS server, no interface but `lo`. For profiles other than `none`, two more things: a Unix domain socket bind-mounted into the sandbox and connected to the host-side proxy, and a small trusted forwarder from the runtime root, started as the sandbox's first process, listening on `127.0.0.1:<port>` inside the namespace and bridging accepted connections to that socket. Clients are configured with `http_proxy`/`https_proxy` and the cargo equivalents pointing at that port, so the sandbox sees exactly one reachable endpoint and it is a proxy.

**Host side.** The Clyde egress proxy runs in clyded (trusted). It accepts HTTP `CONNECT` only — no plain HTTP forwarding; resolves and matches the CONNECT target against the profile's host allowlist; allows or refuses, recording every attempt either way as an `EgressAttempt` on the task run; enforces a per-task byte budget and connection count charged to the lease; and emits a `FetchManifest` artifact summarising destinations, bytes, and refusals.

Because the proxy is host-side and the sandbox has no other route, the allowlist is enforced in trusted code. Compromising the in-sandbox forwarder gains nothing: it can only reach what the proxy already permits.

### TLS: pass-through by default

For every profile except `model-api`, the proxy sees only the `CONNECT` target and forwards opaque bytes; TLS is end-to-end. This is deliberate for `rust-registry` in particular: intercepting dependency traffic would make Clyde a plaintext-handling component in the path of dependency *content*, undermining the content-hash pinning that baselines rely on.

The one carve-out is `model-api`, which exists only to serve the workspace environment and is therefore a warden concern; see [the model-API channel](../warden/design.md#the-model-api-channel) and [D11](../warden/decisions.md#d11-workspace-environment-model-api-egress-goes-through-the-clyde-proxy).

### Profiles

The set is closed. A task policy names one; a lease carries one as its ceiling.

| Profile | Product | Reachable | Used by |
|---|---|---|---|
| `none` | builder | nothing — loopback only, no proxy socket | `rust.check`, `rust.test.unit`, `workspace.read`, `repo.search` |
| `rust-registry` | builder | `crates.io`, `static.crates.io`, `index.crates.io`, plus configured mirrors | `rust.resolve-deps` |
| `broker` | builder | not a sandbox profile — the broker's own egress, outside any sandbox | `git.push` |
| `custom` | builder | explicit host allowlist from an approval | escalations only, never a default |
| `model-api` | warden | configured model API host(s), TLS terminated, auth injected host-side | the workspace environment |

`none` is the default for every task type; anything else must be named explicitly in the task policy. `rust-registry` host lists come from configuration and `.clyde/policy.toml` may only narrow them — a repository cannot add a registry host. `custom` requires a human approval whose `request_digest` covers the exact host list, so approving one destination does not approve another later.

### Profile ordering

Lease derivation and escalation checks need a partial order:

```text
none  <  model-api  <  custom(H)         for any H
none  <  rust-registry
rust-registry  <  custom(H)              iff registry hosts ⊆ H
```

`model-api` and `rust-registry` are **incomparable**: neither is a superset of the other. A lease holding one cannot derive a child holding the other; that requires an escalation evaluated against the mission, not a derivation. This ordering is a pure function and is unit-tested alongside the derivation rules.

### Backend realisation

**Bubblewrap.**

```text
bwrap --unshare-net --unshare-pid --unshare-ipc --unshare-uts --unshare-cgroup \
      --ro-bind <runtime-root-closure> ... \
      --ro-bind <snapshot> /work \
      --bind <mission-cache> /cache \
      --bind <proxy-socket> /run/clyde/egress.sock \   # omitted for profile `none`
      --tmpfs /tmp --proc /proc --dev /dev \
      --clearenv --new-session --die-with-parent ...
```

The forwarder is exec'd first and the task process is its child, so the forwarder dies with the sandbox. For profile `none` the socket is simply not bound: there is no configuration flag that disables egress — **the absence of the socket is the absence of egress**, which is the property worth having.

**Firecracker.** The guest has no network device at all, under any profile, and the `VmConfig` type cannot express one. The forwarder inside the guest bridges `127.0.0.1:<port>` to a **vsock** connection to the host, where the proxy accepts it. The sandbox-visible contract is identical — one loopback proxy endpoint — so task images, cargo configuration, and policy are unchanged across backends. No tap device, no bridge, no host firewall rules, and therefore no root requirement in either backend.

Firecracker permits **one** vsock device per VM, and with the microVM as the inner-loop default every build task needs a channel out for streamed logs and structured cargo diagnostics — the 8250 serial console is the only alternative and it is slow enough that bulk output can stall the guest. So the vsock carries three things, separated by port ([D7 amendment](decisions.md#amendment-the-guest-channel-is-a-single-vsock-multiplexed-by-port)):

| Port | Carries | Present |
|---|---|---|
| job control | argv, environment, cwd, drive labels in; exit status out | always |
| log stream | stdout, stderr, cargo JSON diagnostics | always |
| egress bridge | proxied connections | host binds a listener **only** when the profile permits egress |

This weakens an earlier claim deliberately rather than quietly: under `none` the guest previously had no channel of any kind, and now has a channel with no listener on the egress port. What remains true and testable is that no guest network device exists in any configuration; that no host listener exists on the egress port for a `none`-profile VM; and that a VM booted for one egress profile never serves a task under another, because a vsock device cannot be removed after boot. The last point constrains any future pooling: pools partition by egress profile as well as by runtime root and size class.

Under bubblewrap, profile `none` remains the genuinely stronger form — the absence of a socket. The two backends make slightly different structural claims for the same profile, and the documentation and approval UX should not flatten them into one sentence.

### What egress control does not guarantee

Stated plainly here so the approval UX can state it too.

- **Destination scoping, not content control.** An allowlisted host can be sent arbitrary bytes. What the model provides is that the channel is narrow, that every connection is recorded, and — for `model-api` — that the credential cannot be stolen and reused elsewhere.
- **No protection against a malicious allowlisted host.** If a permitted registry serves hostile content, egress control does not help; that is what the snapshot, isolation, and dependency-policy layers are for.
- **DNS is resolved host-side.** The sandbox cannot make DNS queries at all, which is a benefit, but it also means the proxy's view of a hostname is authoritative for the allowlist decision. Allowlist entries are hostnames, and a compromised host resolver is out of scope.
- **Byte budgets are coarse.** They bound bulk exfiltration and runaway downloads; they do not detect low-volume signalling.

### Audit output

Every task run with a profile other than `none` produces `EgressAttempt` records — timestamp, target host and port, decision, bytes in and out, and the reason for any refusal — and a `FetchManifest` artifact giving the aggregate view, referenced from the task result and shown in mission review.

A refused attempt is a first-class signal, not a log line. `rust.check` should never attempt egress; if it does, that is either a misconfiguration or a hostile dependency probing for a way out, and it must surface in mission review either way.
