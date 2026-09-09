# Warden: Implementation Decision Log

The four binding decisions that apply only where Clyde hosts the coding agent, plus the one open question that belongs to them. Every other decision is in the [builder's log](../builder/decisions.md), which is also where [D23](../builder/decisions.md#d23-the-mvp-splits-into-the-builder-and-the-warden) — the split these four sit on the far side of — lives.

Identifiers are stable and continuous across both logs: this file holds **D1**, **D11**, **D17**, and **D20**.

**Status legend** — **accepted**: decided, in force. **provisional**: decided for now, expected to be revisited at a named point. **open**: identified but not yet decided.

## D1: The coding agent runs inside a Clyde-managed workspace environment

**Status:** accepted — narrowed by [D23](../builder/decisions.md#d23-the-mvp-splits-into-the-builder-and-the-warden)

**Decision**
The primary coding agent process, and each sub-agent, runs inside a Clyde-managed workspace-environment sandbox, not on the host. That sandbox receives the lease-scoped repository subtrees bind-mounted read-write; the remainder of the repository read-only where the build closure requires it; `.git` read-only; the actor-facing Clyde socket; an isolated scratch directory; and no host home directory, no credentials, and no egress except the Clyde proxy allowlist ([D11](#d11-workspace-environment-model-api-egress-goes-through-the-clyde-proxy)).

**Rationale**
If the agent runs on the host with ordinary filesystem and network access, it can invoke `cargo` directly and Clyde's typed-task pipeline becomes advisory. Snapshot isolation would then protect only against hostile dependencies reached *through* Clyde, not against agent-directed execution — which is [attack vector 4](../threat-model.md#attack-vectors-and-which-product-answers-them). Typed tasks must be the only available execution path for project code, and that is a property of the agent's environment, not of the agent's good behaviour.

**Consequences**
- Hosting an agent requires enough workspace-environment plumbing to launch a process, not just a socket API.
- Edit scope is enforced by **mount topology** rather than by proxying every file write through clyded. A lease's writable paths are exactly its read-write binds.
- The separate "edit-helper runtime" described in earlier drafts collapses into the workspace environment itself; see [D17](#d17-workspaceedit-helper-execution-is-the-workspace-environment).
- Each actor session gets its own workspace-environment sandbox with its own mount scope, which is how sub-agent narrowing is enforced.
- Clyde cannot enumerate every command the agent runs inside its own environment. Auditing of editing work is diff-based, not command-based, unless the optional exec-logging shim is enabled ([OQ3](#oq3-exec-logging-shim-in-the-workspace-environment)).

### Narrowing: the property is toolchain absence, not Clyde hosting
[D23](../builder/decisions.md#d23-the-mvp-splits-into-the-builder-and-the-warden) moves this decision out of the builder. The rationale above is not withdrawn — a driver that can invoke `cargo` directly does make the typed pipeline advisory — but it proves that the *build toolchain* must be absent from the driver's environment, not that Clyde must be the thing providing that environment. A host with no Rust toolchain has the same property.

So the decision is restated: **typed tasks are the only path to the build toolchain. The warden provides that property; the builder verifies and reports it** ([D26](../builder/decisions.md#d26-enforcement-posture-is-explicit-reported-and-recorded)).

The residual gap is stated rather than papered over: a driver with network access can fetch a toolchain, so toolchain absence is only durable when egress is also controlled — which it is not for a driver Clyde does not host. The builder alone is therefore fully effective against a hostile dependency and advisory against a hostile driver.

## D11: Workspace-environment model-API egress goes through the Clyde proxy

**Status:** accepted

**Decision**
The workspace environment's only egress is the Clyde proxy under an egress profile allowlisting the configured model API host(s). All attempts are logged.

**Rationale**
One egress mechanism for the whole system, and the proxy is needed by the builder for its own reasons regardless. The alternative — unrestricted egress for the workspace environment — leaves one environment with an unaudited exfiltration path.

### Credential handling
The agent holds **no** model API credential. The proxy terminates TLS for `model-api` hosts and injects the `Authorization` header host-side ([D7 amendment](../builder/decisions.md#amendment-selective-tls-termination-for-model-api-only)).

This resolves what would otherwise be a contradiction: the workspace environment is specified as holding no credentials, yet something must authenticate. Injecting the key into the sandbox was the simpler option and was rejected in favour of the agent never possessing it, so that a compromised or prompt-injected agent cannot exfiltrate a reusable credential — only spend the channel while it holds a lease.

**Consequences**
- The proxy is a **builder** component, delivered in Phase 1, not a warden component.
- Clyde holds a CA whose certificate the workspace environment trusts. That is a real trusted-surface increase, scoped to model hosts only. Build and fetch sandboxes never receive the certificate.
- The proxy sees prompt and completion plaintext. It must not log bodies; metadata only, and there is no flag that enables body logging.
- Model egress needs request and byte budgets, since the agent can spend the credential without holding it.
- **Honest caveat**, to be stated in the docs and the UX: an allowlisted API endpoint that accepts arbitrary request bodies is still an exfiltration channel for a misbehaving agent. What this buys is that it is the *only* such channel, that it is logged, and that the credential itself cannot be stolen and reused elsewhere.

## D17: `workspace.edit` helper execution *is* the workspace environment

**Status:** accepted — consequence of [D1](#d1-the-coding-agent-runs-inside-a-clyde-managed-workspace-environment)

**Decision**
Earlier drafts described helper-driven `workspace.edit` as a separate low-authority sandbox that Clyde launches on request. Since the agent now runs inside exactly such a sandbox, that separate runtime is redundant. The workspace environment *is* the edit-helper runtime, and the agent running a codemod there requires no task request.

**What is preserved**
Every property that made the separate runtime a requirement still holds, because they are now properties of the agent's own environment: a separate runtime root from build/test/fetch ([D6](../builder/decisions.md#d6-runtime-roots-are-nix-closures-not-oci-images)); text and code manipulation tooling and no project build toolchain; writable paths limited to lease scope, enforced by mount topology; no raw credentials, no host home, no container runtime socket; and no egress beyond the model-API allowlist.

**Consequences**
- `workspace.edit` remains a task *type* in the catalog for audit and policy purposes, but in the MVP it describes a class of activity rather than a Clyde-launched sandbox.
- Edit auditing is diff-based, computed at snapshot and task boundaries. An optional exec-logging shim in the workspace runtime root can add command-level records; it is a nice-to-have, not a boundary.
- A command that needs the project build toolchain simply cannot run in the workspace environment — the tooling is absent. That is the enforcement, and it is stronger than policy text.

## D20: The agent command is host or user configuration, never repository configuration

**Status:** accepted

**Decision**
What binary clyded execs as the agent — command, arguments, and environment allowlist — comes from host or user configuration only. `.clyde/policy.toml` cannot set, extend, or override it.

**Rationale**
Repository content is untrusted. A repo that can choose what Clyde executes as the agent has arbitrary code execution in the workspace environment on `clyde mission create`, before any policy applies. This follows from [D14](../builder/decisions.md#d14-machine-readable-policy-in-clyde-agentsmd-advisory-only) but is worth stating separately because it is the highest-consequence instance of it.

**Consequences**
- The config loader treats `agent.*` keys in repository config as a **hard rejection**, not a narrowing check.
- The same rule covers runtime root selection: a repository cannot choose which runtime root a task executes against.

## Open questions

### OQ3: Exec-logging shim in the workspace environment

**Status:** open

Whether to ship the optional command-level exec logger described in [D17](#d17-workspaceedit-helper-execution-is-the-workspace-environment) inside the MVP window, or defer it. It improves audit fidelity for editing work and is **not** a security boundary. **Proposed:** defer to post-MVP.

[OQ5](../builder/decisions.md#oq5-per-open-enforcement-of-the-build-snapshot) belongs to the builder.
