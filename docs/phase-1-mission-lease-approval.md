# Phase 1: Mission, Lease, and Approval Core

## Goal

Make bounded delegation real: a human creates and approves a mission, Clyde issues a lease, an agent runs inside a workspace-environment sandbox under that lease, and every side-effecting action is attributable to it.

Phase 1 deliberately contains no project code execution. Its purpose is the delegation boundary.

Decisions in force: [D1](decisions.md#d1-the-coding-agent-runs-inside-a-clyde-managed-workspace-environment), [D2](decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel), [D10](decisions.md#d10-mcp-is-the-primary-actor-facing-api), [D11](decisions.md#d11-workspace-environment-model-api-egress-goes-through-the-clyde-proxy), [D13](decisions.md#d13-cli-through-phases-1-3-tui-in-phase-4), [D16](decisions.md#d16-one-active-mission-per-workspace), [D17](decisions.md#d17-workspaceedit-helper-execution-is-the-workspace-environment).

## Deliverables

### 1. clyded with two sockets

- `clyded.sock` — actor API. MCP plus JSON-RPC. Requires a valid session token. May be bind-mounted into sandboxes.
- `clyded-admin.sock` — human API. Mode `0600`, `SO_PEERCRED`-checked, never mounted into any sandbox.

Operations available **only** on the admin socket: mission create, approve, deny, revoke, renew, workspace register, and any read of another actor's data.

Startup must fail closed: if either socket path is world-writable, already bound by an unexpected owner, or inside a registered workspace directory, the daemon refuses to start.

### 2. Mission lifecycle

Create → propose envelope → human approval → active → closeout, with the full state machine from the [schema reference](schema-reference.md#mission-states) and the one-active-mission-per-workspace constraint ([D16](decisions.md#d16-one-active-mission-per-workspace)).

Mission proposal takes the human's objective plus configuration defaults and produces a concrete envelope: edit scope, read scope, allowed tasks, egress profile, budget, expiry, escalation rules. The proposal is shown for approval as the exact envelope that will be issued — no field is decided after approval.

Closeout, in one transaction: revoke leases, revoke sessions, tear down sandboxes, compute and store the closing diff, delete the mission cache, write the summary.

### 3. Lease issuance, derivation, validation, revocation

Wire the Phase 0 pure functions to storage and enforce them on every request. `validate_action` runs before any side effect, and its `PolicyDecision` is recorded whether it allows or denies.

Renewal issues a **replacement** lease and marks the old one `superseded`, rather than mutating expiry in place, so the audit trail shows the extension as an event.

Revocation is immediate for new work and fans out to derived leases and sessions in the same transaction.

### 4. Session tokens

256-bit random tokens, stored hashed, delivered to `/run/clyde/session-token` (`0400`) inside the sandbox, never in `argv` or environment, never logged, never returned by any query ([D2](decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel)).

Every actor request resolves token → session → lease → mission before any other check. An unknown, expired, or revoked token is rejected identically, with no information about which.

### 5. Workspace-environment sandbox

The first real sandbox in the project, per the [composition table](agent-and-workspace-environment.md#sandbox-composition): lease-scoped read-write binds, read-only context, read-only `.git`, `runtimeRoots.workspace` closure, actor socket, token file, egress socket, tmpfs scratch, no host home, no credentials.

It uses the Phase 2a `SandboxBackend` trait, so this is where bubblewrap bring-up actually happens — but only for the low-authority workspace environment, not for project code execution. That distinction keeps Phase 1 honest: a weaker boundary here holds a semi-trusted agent, not hostile dependency code.

### 6. Egress proxy

The proxy and forwarder from the [network egress model](network-egress-model.md), needed in Phase 1 for the `model-api` profile ([D11](decisions.md#d11-workspace-environment-model-api-egress-goes-through-the-clyde-proxy)). Deliver:

- host-side CONNECT proxy with per-profile host allowlists
- `clyde-forward` in the runtime root, bridging `127.0.0.1:<port>` to the bound socket
- `EgressAttempt` recording for allowed and denied attempts alike
- per-task byte, request, and connection budgets charged to the lease

Plus the `model-api` termination path ([D7 amendment](decisions.md#amendment-selective-tls-termination-for-model-api-only)):

- CA generation at first run; private key host-only, mode `0600`, never in a sandbox, artifact, or audit payload
- CA certificate mounted read-only into workspace environments, with `SSL_CERT_FILE` and equivalents set; **not** mounted into build or fetch sandboxes
- TLS termination for `model-api` hosts only, with `Authorization` injected host-side and normal upstream certificate verification
- pass-through with no interception for every other profile
- metadata-only logging for terminated connections: host, request path, status, byte counts, timing. No body logging, and no flag that enables it

Profile `none` is implemented by not binding the socket. There is no runtime flag that disables egress.

### 7. Agent hosting and MCP server

#### Transport
Newline-delimited JSON-RPC directly over `clyded.sock` ([D19](decisions.md#d19-mcp-over-the-actor-socket-is-line-framed-json-rpc)). One message codec shared with the admin surface. Message size limits and backpressure must be explicit, since an actor is untrusted input.

#### Launching the agent
The command clyded execs comes from host or user configuration only, never from repository configuration ([D20](decisions.md#d20-the-agent-command-is-host-or-user-configuration-never-repository-configuration)):

```toml
[agent]
command = "claude"
args = ["--mcp-socket", "/run/clyde/clyded.sock"]
env = ["TERM", "LANG"]        # allowlist; the sandbox environment is otherwise cleared
```

The agent binary must be reachable inside the sandbox, which means it comes from the workspace runtime root or an explicitly mounted read-only path recorded in the sandbox spec. It is not copied out of the host's `PATH` implicitly.

If the configured command is absent, clyded fails mission activation with a clear diagnostic rather than starting a sandbox with nothing in it.

#### Tool gating by phase
In Phase 1, `run_task` accepts only T0/T1 workspace task types. In Phase 1, `run_task` accepts only T0/T1 workspace task types; requesting `rust.check` returns a structured denial naming the phase-gated capability. That denial path is worth building now, because it is the same code path Phase 3 uses for escalations.

### 8. Approval manager

Approval requests carry a `request_digest` over the exact normalised request, an expiry, and the alternatives the policy engine suggested. Decisions are `ApproveOnce`, `ApproveForMission`, or `Deny`, and can only be made on the admin socket by a human actor.

`ApproveOnce` consumption is single-use and transactional: consuming an approval and performing the approved action either both happen or neither does.

### 9. CLI

Operator commands (admin socket): `workspace register`, `mission create|status|approve|deny|revoke|renew|close`, `approvals list|approve|deny`, `audit show`, `doctor`.

Actor commands (actor socket, token): `task run`, `task status`, `task logs`, `mission status`.

Every command supports `--json` with a stable shape from Phase 1, since the CLI is also the integration-test harness ([D13](decisions.md#d13-cli-through-phases-1-3-tui-in-phase-4)).

`clyde approve` refuses to run if it detects it is inside a sandbox, and says why.

### 10. Audit

The full [minimum event set](schema-reference.md#minimum-event-set-for-phases-0-4) for missions, leases, sessions, sandboxes, policy decisions, egress attempts, and approvals, hash-chained, with `audit show` rendering a mission timeline.

## Diff-based edit auditing

With edit scope enforced by mount topology ([D1](decisions.md#d1-the-coding-agent-runs-inside-a-clyde-managed-workspace-environment)), Clyde records what changed rather than each write. Phase 1 computes and stores a workspace diff:

- on demand (`clyde mission status --diff`)
- at mission closeout
- and, from Phase 2a, at every snapshot boundary

The diff is computed by the daemon against the live tree using sanitised git invocations (no repository hooks, no repository config for transport) — the same hardening Phase 4 needs for push ([D8](decisions.md#d8-brokered-gitpush-uses-the-developers-existing-credential-inside-the-broker-only)).

## Security properties this phase must demonstrate

- No side-effecting action succeeds without an active lease and a valid session token.
- An agent cannot write outside its lease's edit paths — the attempt fails at the kernel, not at a policy check.
- An agent cannot approve anything: the admin socket is unreachable from its sandbox, and no actor-facing operation grants approval.
- An agent cannot reach the network except the `model-api` allowlist, and every attempt is recorded.
- An agent never possesses the model credential: it authenticates only because the proxy injects auth host-side, and no key material appears in its sandbox.
- A build or fetch sandbox does not receive the Clyde CA certificate, so it cannot be transparently intercepted even by Clyde.
- Repository configuration cannot influence what binary clyded execs as the agent.
- Revoking a mission stops all further actor work immediately, including for sub-agents.
- The workspace environment contains no project build toolchain, so no project code execution is possible in this phase at all.

## Exit criteria

- a human can create, approve, inspect, renew, and revoke a mission end to end from the CLI
- an off-the-shelf MCP-capable agent runs inside the workspace environment and drives `mission_status`, `list_capabilities`, `run_task`, and `request_escalation` with no Clyde-specific modification
- out-of-scope edit attempts fail, and the failure is visible in mission review
- out-of-scope task requests are denied with a structured, actionable reason
- egress from the workspace environment is limited to the `model-api` allowlist, with allowed and denied attempts both recorded
- lease expiry and revocation are enforced against a running agent, with sub-agent fan-out tested
- the audit chain reconstructs a full mission timeline, and truncating it is detectable
- `--json` output for every command is covered by CLI integration tests
- no model credential appears anywhere in the sandbox, verified by a test that scans the mount table and the sandbox environment
- the CA private key is never readable from a sandbox, verified by a test

## Explicitly not in Phase 1

No snapshots, no project code execution, no dependency resolution, no broker operations, no TUI.
