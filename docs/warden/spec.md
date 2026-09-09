# Warden: Deliverables and Exit Criteria

The implementation spec for the agent harness. [design.md](design.md) is the design behind it; this is the deliverable list, the security properties, and the exit criteria.

## Goal

Make bounded delegation real, and make `enforcing` posture reachable: a human approves a mission, Clyde issues a lease, and a coding agent runs inside a Clyde-managed workspace environment under that lease — with no build toolchain, no credentials, no host home, and no egress but the model API.

Decisions in force: [D1](decisions.md#d1-the-coding-agent-runs-inside-a-clyde-managed-workspace-environment), [D11](decisions.md#d11-workspace-environment-model-api-egress-goes-through-the-clyde-proxy), [D17](decisions.md#d17-workspaceedit-helper-execution-is-the-workspace-environment), [D20](decisions.md#d20-the-agent-command-is-host-or-user-configuration-never-repository-configuration), and the builder's [D2](../builder/decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel), [D6](../builder/decisions.md#d6-runtime-roots-are-nix-closures-not-oci-images), [D10](../builder/decisions.md#d10-mcp-is-the-primary-actor-facing-api), [D19](../builder/decisions.md#d19-mcp-over-the-actor-socket-is-line-framed-json-rpc), [D23](../builder/decisions.md#d23-the-mvp-splits-into-the-builder-and-the-warden).

## Prerequisite

[The builder](../builder/roadmap.md), through Phase 4. The warden hosts an actor that drives the builder's surfaces; there is nothing for a hosted agent to do until those surfaces exist and work.

Specifically it needs the actor socket and session tokens (Phase 1), the egress proxy (Phase 1), the task pipeline and access baselines (Part 1a), and the structured denial paths that make escalation legible (Part 1a).

## Deliverables

### 1. The workspace-environment sandbox
Per the [composition table](design.md#sandbox-composition): lease-scoped read-write binds, read-only context, read-only `.git`, the `runtimeRoots.workspace` closure, the actor socket, the token file, the egress socket, tmpfs scratch, no host home, no credentials.

It runs on the **namespace backend**, deliberately: the environment needs a genuinely writable, host-visible working tree, which a block device cannot give it. The runtime root must contain no project build toolchain — asserted over the derivation's closure by `nix flake check`, and asserted again by the daemon before it starts an environment, so a hand-configured root cannot quietly reintroduce one.

### 2. Agent hosting
Launching the configured command inside that sandbox, from host or user configuration only ([the `[agent]` section](design.md#agent-hosting)). The absence of that section is what makes a deployment builder-only, and clyded must treat it as a supported configuration rather than an error.

### 3. The `model-api` egress path
The workspace environment's only egress, through the Clyde proxy under a profile allowlisting the configured model API host(s). The proxy and forwarder are builder components; what the warden adds is the [termination carve-out](design.md#the-model-api-channel) — CA generation and host-only key custody, the certificate mounted into workspace environments and nowhere else, termination for `model-api` hosts only with `Authorization` injected host-side, metadata-only logging, and per-mission request and byte budgets.

### 4. Escalation and sub-agent tools
The two MCP tools that belong to the warden rather than the builder: `request_escalation` and `request_subagent`. The structured denial paths the first consumes already exist; what the warden adds is an actor that can raise one.

### 5. Sub-agent sessions
Each derived lease yields its own workspace-environment sandbox with its own narrower mount table, which is how sub-agent narrowing is *enforced* rather than requested. Revocation fans out to derived leases and sessions in the same transaction as the parent, which the builder's lease manager already implements — the warden tests it against a running agent.

### 6. Diff-based edit auditing against a hosted agent
The builder computes and stores workspace diffs on demand, at snapshot boundaries, and at closeout. The warden is where the fidelity limitation becomes real and must be documented: Clyde cannot enumerate every command the agent runs inside its own environment, so editing work is audited by diff rather than by command ([D1](decisions.md#d1-the-coding-agent-runs-inside-a-clyde-managed-workspace-environment)). An optional exec-logging shim could add command-level records ([OQ3](decisions.md#oq3-exec-logging-shim-in-the-workspace-environment)); it is not a boundary.

### 7. `workspace.edit` becomes live
The three workspace task types are inert in a builder-only deployment. The warden activates them, and their enforcement is what the runtime root contains rather than a sandbox Clyde launches per request ([D17](decisions.md#d17-workspaceedit-helper-execution-is-the-workspace-environment)). An agent writing and running a codemod is simply the agent working.

### 8. Posture becomes `enforcing`
With an agent hosted and the toolchain absent from its runtime root, the deployment reports `enforcing` ([D26](../builder/decisions.md#d26-enforcement-posture-is-explicit-reported-and-recorded)). This is a derived observation, not a configuration flag: clyded reports `enforcing` because it can see that the actor's environment has no reachable toolchain, and it must keep reporting `advisory` if it cannot establish that.

## Security properties this product must demonstrate

- An agent cannot write outside its lease's edit paths — the attempt fails at the kernel, not at a policy check.
- An agent cannot approve or confirm anything: the admin socket is unreachable from its sandbox, and no actor-facing operation grants approval.
- An agent cannot reach the network except the `model-api` allowlist, and every attempt is recorded.
- An agent never possesses the model credential: it authenticates only because the proxy injects auth host-side, and no key material appears in its sandbox.
- A build or fetch sandbox does not receive the Clyde CA certificate, so it cannot be transparently intercepted even by Clyde.
- The workspace environment contains no project build toolchain, verified over the derivation closure and again at environment start.
- No host home, `~/.ssh`, `~/.gnupg`, browser profile, container socket, admin socket, broker socket, mission cache, snapshot store, or artifact store is reachable from a workspace environment, verified by a test over every spec the system can generate.
- Repository configuration cannot influence what binary clyded execs as the agent.
- Revoking a mission stops all further actor work immediately, including for sub-agents.
- A sub-agent's derived lease cannot widen any dimension of its parent's, and its sandbox mount table is correspondingly narrower.
- `clyde doctor` reports `enforcing` only when the toolchain is genuinely unreachable from the actor's environment, and `advisory` otherwise; no configuration value can change which is reported.

## Exit criteria

- an off-the-shelf MCP-capable agent runs inside the workspace environment and drives `mission_status`, `list_capabilities`, `run_task`, `task_status`, and `request_escalation` with no Clyde-specific modification
- the agent completes a full edit → check → test loop autonomously within a lease, with no repeated approval and no baseline prompts for first-party editing
- out-of-scope edit attempts fail at the kernel, and the failure is visible in mission review
- out-of-scope task requests are denied with a structured, actionable reason that names the escalation path
- egress from the workspace environment is limited to the `model-api` allowlist, with allowed and denied attempts both recorded
- no model credential appears anywhere in the sandbox, verified by a test that scans the mount table and the sandbox environment
- the CA private key is never readable from a sandbox, verified by a test
- lease expiry and revocation are enforced against a running agent, with sub-agent fan-out tested
- a sub-agent runs under a derived lease in its own narrower sandbox
- clyded with no `[agent]` section still starts, serves both surfaces, and reports `advisory` — **the builder must not regress**
- posture flips to `enforcing` with an agent hosted, and the flip is visible in `clyde doctor`, on task runs, and in mission review

## Explicitly not in scope

No multi-level sub-agent derivation (Phase 6), no research environment, no browser or synthetic services (Phase 5), no exec-logging shim ([OQ3](decisions.md#oq3-exec-logging-shim-in-the-workspace-environment)), and no move of the workspace environment onto the microVM backend — that needs a write-back path for live edits that nothing in the MVP requires.
