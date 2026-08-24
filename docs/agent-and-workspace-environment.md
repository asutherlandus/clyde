# Clyde Next: Agent Integration and the Workspace Environment

## Purpose

This document defines how a coding agent is hosted inside Clyde: where the agent process runs, what it can see and reach, how it authenticates, what API it uses, and how editing authority is enforced.

It covers ground the original design set left implicit. The earlier documents describe the agent as an actor that "requests tasks through Clyde" without saying where the agent process lives — and if it lives on the host with ordinary authority, most of the rest of the design is unenforceable.

Decisions: [D1](decisions.md#d1-the-coding-agent-runs-inside-a-clyde-managed-workspace-environment), [D2](decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel), [D10](decisions.md#d10-mcp-is-the-primary-actor-facing-api), [D11](decisions.md#d11-workspace-environment-model-api-egress-goes-through-the-clyde-proxy), [D17](decisions.md#d17-workspaceedit-helper-execution-is-the-workspace-environment).

## The core property

> The agent must not be able to execute project build or test code except by asking Clyde.

This is a property of the agent's *environment*, not of the agent's behaviour or of policy text. It is achieved by two things, both structural:

1. The agent runs in a sandbox whose runtime root contains **no project build toolchain** — no `cargo`, no `rustc`, no `node`, no browser, no signing tools, no container client. Not "forbidden": absent.
2. That sandbox has no egress except the model API allowlist, no credentials, no host home directory, and writable access only to the paths its lease permits.

An agent in that environment cannot run `cargo check` even if it decides to. `run_task` is the only path, and `run_task` goes through policy.

## Sandbox composition

Each actor session gets its own workspace-environment sandbox:

| Mount | Mode | Notes |
|---|---|---|
| lease `edit_paths` | read-write | the writable surface; this *is* the edit-scope enforcement |
| lease `read_paths` | read-only | additional context the lease permits |
| `.git` | read-only | prevents hook and config planting ([D8](decisions.md#d8-brokered-gitpush-uses-the-developers-existing-credential-inside-the-broker-only)) |
| `runtimeRoots.workspace` closure | read-only | nix closure ([D6](decisions.md#d6-runtime-roots-are-nix-closures-not-oci-images)) |
| `AGENTS.md`, `.clyde/policy.toml` | read-only | guidance and visible policy |
| `/run/clyde/clyded.sock` | read-write socket | actor API only, never the admin socket |
| `/run/clyde/session-token` | read-only, `0400` | capability token ([D2](decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel)) |
| `/run/clyde/egress.sock` | read-write socket | proxy bridge, `model-api` profile |
| Clyde CA certificate | read-only | so the proxy can terminate `model-api` and inject auth; not given to build sandboxes |
| `/tmp`, `/scratch` | read-write tmpfs | scratch |

Not present: host home, `~/.ssh`, `~/.gnupg`, cloud config, browser profiles, other projects, the admin socket, the broker socket, any container runtime socket, the mission cache, the snapshot store, and the artifact blob store.

Artifacts and logs are read through the API, not through a mounted store, so the agent cannot read another mission's outputs by walking a filesystem.

### Runtime root contents

`runtimeRoots.workspace` provides text and code manipulation: a shell, coreutils, `grep`/`ripgrep`, `sed`/`sd`, `find`/`fd`, `jq`, `diff`/`patch`, a structural-rewrite tool, and a general-purpose scripting interpreter for one-off codemods.

It must not provide: `cargo`, `rustc`, `rustup`, `node`, `npm`/`pnpm`/`yarn`, a browser, `gpg`, `ssh`, `docker`/`podman`, or a package manager that could install any of the above. This is asserted by a test over the derivation's closure, not by review.

## Editing authority is mount topology

Earlier drafts had every file write pass through an `edit_files` API call so clyded could enforce path scope. With the agent inside a sandbox, the bind-mount table already expresses exactly that scope, enforced by the kernel rather than by a check in a request handler.

Consequences:

- **No `read_code` or `edit_files` API.** The agent uses its ordinary filesystem tools. This is also why an off-the-shelf agent works without modification.
- **Sub-agent narrowing is real.** A derived lease with a narrower `edit_paths` yields a sandbox with a narrower writable set. The sub-agent cannot write outside it, whatever it attempts.
- **Edit auditing is diff-based.** Clyde records diffs at snapshot and task boundaries and at mission closeout, rather than a stream of per-write records. This is a genuine reduction in audit fidelity relative to the earlier design, accepted knowingly: the boundary that matters (what can be written) is stronger, and the record (what was written) is complete even if the sequence is coarser. An optional exec-logging shim could add command-level records ([OQ3](decisions.md#oq3-exec-logging-shim-in-the-workspace-environment)).

## `workspace.edit` after this change

The original design required a separate low-authority runtime for helper-driven edits — codemods, batch rewrites, ad hoc scripts — with a hard rule that it be distinct from build/test/fetch runtimes and carry no project build toolchain.

The agent's own environment now satisfies every one of those requirements ([D17](decisions.md#d17-workspaceedit-helper-execution-is-the-workspace-environment)). So an agent writing and running a codemod is simply the agent working, with no task request and no sandbox launch. `workspace.edit` remains in the task catalog as the name for that class of activity — it is what appears in the audit record and what a lease's `task_scope` authorises — but in the MVP it is not a Clyde-launched sandbox.

The rule that mattered survives intact, and in a stronger form: work needing the project build toolchain *cannot* happen in the workspace environment, because the toolchain is not there. It has to become `rust.check`, `rust.test.unit`, or another typed task, which is exactly the intended funnel.

## Actor API

MCP over `clyded.sock` as newline-delimited JSON-RPC ([D19](decisions.md#d19-mcp-over-the-actor-socket-is-line-framed-json-rpc)), authenticated with the session token ([D10](decisions.md#d10-mcp-is-the-primary-actor-facing-api)).

| Tool | Purpose |
|---|---|
| `mission_status` | objective, scope, allowed tasks, budget remaining, expiry |
| `list_capabilities` | what this lease may do now, and what would need escalation |
| `run_task` | request a typed task; returns a task run id |
| `task_status` | state and outcome, including structured failure classification |
| `task_logs` | streamed or ranged log access for a task run |
| `list_artifacts` | artifacts produced by this mission's task runs |
| `request_escalation` | ask for capability beyond the lease, with reason and alternatives |
| `request_subagent` | ask for a derived lease and a sub-agent session |
| `request_publish` | ask for a brokered operation (Phase 4) |
| `commit_prepare` | ask clyded to build a commit proposal from the working tree |

### Tool descriptions carry policy

Each tool description states the policy consequences of calling it — whether it can trigger network access, whether it requires approval, what it will and will not do. The description is part of the security UX: the agent's understanding of its constraints comes from here, and a vague description produces an agent that asks for the wrong things.

### Denials are actionable

A denied request returns the structured `PolicyReason` set ([schema reference](schema-reference.md#policy-decision)), rendered as: what was denied, which constraint denied it, and what the narrower or escalated alternative is. An agent that receives "denied" with no path forward will either loop or try to work around the boundary; both are worse than a clear next step.

This matters particularly for access drift ([D18](decisions.md#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift)). "Build failed" would send the agent hunting through its own code for a bug that isn't there. "This build tried to read `crates/proto/schema.sql`, which is outside the confirmed access baseline for this target; a human must confirm the change" tells it to raise an escalation instead — and it should never be able to confirm one itself.

## Session lifecycle

```text
1. Human creates a mission (admin socket) and approves the envelope.
2. clyded issues the primary lease.
3. clyded creates the workspace-environment sandbox for the primary actor:
   mounts per lease scope, token file written 0400, sockets bound.
4. clyded starts the agent process inside the sandbox.
5. Agent reads /run/clyde/session-token, connects to clyded.sock, calls mission_status.
6. Agent works: edits directly, requests typed tasks, reads results.
7. On lease expiry, revocation, or mission closeout: token revoked, sandbox torn down,
   mission cache deleted, closeout diff and summary recorded.
```

Sub-agent sessions follow the same sequence from step 3, with a derived lease and a narrower mount table.

## The model API channel, stated plainly

The workspace environment reaches the model API through the Clyde proxy under the `model-api` profile ([D11](decisions.md#d11-workspace-environment-model-api-egress-goes-through-the-clyde-proxy)).

**The credential is not in the sandbox.** The proxy terminates TLS for model hosts and injects the `Authorization` header host-side, so the agent authenticates without ever possessing a key. A compromised or prompt-injected agent cannot steal a reusable credential — it can only spend the channel while it holds a lease, bounded by the mission's request and byte budgets.

**The channel is still an exfiltration path.** The endpoint accepts arbitrary request bodies, so an agent that wanted to send repository content out could. That is not solved, and should not be described as if it were. What the design provides:

- it is the **only** egress from the workspace environment, and it is loopback-proxied, allowlisted, and recorded
- untrusted *dependency* code never runs in this environment, so exfiltration requires the agent itself to misbehave, not a hostile crate
- build and test environments have no egress at all, so hostile dependency code has no channel even if it can read the snapshot
- the credential cannot be lifted and used elsewhere

**What termination costs.** The proxy now handles model plaintext, which is a real trusted-surface increase. It must not log bodies, and the carve-out is scoped to model hosts alone — build and fetch sandboxes never receive the CA certificate, so they cannot be transparently intercepted even by Clyde.

The residual risk is a misbehaving or prompt-injected agent, a different threat class from the supply-chain focus of the [threat model](problem-statement-threat-model.md), and not claimed to be addressed in the MVP.

## Human interaction

The human works on the host: `clyde` CLI on the admin socket for mission creation, approvals, status, and revocation ([D13](decisions.md#d13-cli-through-phases-1-3-tui-in-phase-4)). The human may also edit files directly in the workspace with their own editor; the agent's sandbox binds the same live tree, so both see each other's changes.

`clyde approve` is an operator command and refuses to run inside a sandbox, because the admin socket is never mounted into one ([D2](decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel)). That refusal is the mechanism that makes agent self-approval impossible.
