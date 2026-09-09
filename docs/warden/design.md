# The Warden: Design

The warden is the agent harness: how a coding agent is hosted inside Clyde. Where the agent process runs, what it can see and reach, how it authenticates, what API it uses, and how editing authority is enforced.

It is a product **on top of** [the builder](../builder/design.md), not a prerequisite for it. The builder — snapshots, baselines, the dependency inventory, isolated build execution, brokered push, audit — works with no hosted agent at all, driven by a human operator or by an agent running outside Clyde ([D23](../builder/decisions.md#d23-the-mvp-splits-into-the-builder-and-the-warden)).

Decisions: [D1](decisions.md#d1-the-coding-agent-runs-inside-a-clyde-managed-workspace-environment), [D11](decisions.md#d11-workspace-environment-model-api-egress-goes-through-the-clyde-proxy), [D17](decisions.md#d17-workspaceedit-helper-execution-is-the-workspace-environment), [D20](decisions.md#d20-the-agent-command-is-host-or-user-configuration-never-repository-configuration). It also relies on the builder's [D2](../builder/decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel), [D6](../builder/decisions.md#d6-runtime-roots-are-nix-closures-not-oci-images), [D10](../builder/decisions.md#d10-mcp-is-the-primary-actor-facing-api), and [D19](../builder/decisions.md#d19-mcp-over-the-actor-socket-is-line-framed-json-rpc).

> The warden **creates** sandboxes; it is not one. Throughout this document, *a sandbox* is the isolation mechanism — here a bubblewrap namespace — and *the warden* is the product that specifies, launches, and tears one down per actor session.

## What it is worth, stated precisely

The warden buys three things, and it is worth being exact because the builder already buys the largest one.

1. **Toolchain absence becomes structural.** The agent cannot invoke `cargo` because there is no `cargo`. `run_task` is the only path to project execution, and posture becomes `enforcing`.
2. **Credential adjacency disappears.** An agent on the host holds the developer's SSH key, GitHub token, and cloud credentials, which makes the credential broker's third function — withholding a credential from an untrusted driver — vacuous ([D8 amendment](../builder/decisions.md#amendment-the-brokers-value-is-posture-dependent)). Inside the warden, it is real.
3. **Edit scope becomes prevention rather than detection.** Under the builder alone, an out-of-scope edit shows up in the closing diff after the fact. Under the warden it fails at the kernel.

What it does **not** buy: protection against a hostile dependency. That is the builder's job and it is already done, because a hostile `build.rs` does not care who requested the build.

Without the warden, Clyde reports `advisory` posture and says which of the three is missing ([D26](../builder/decisions.md#d26-enforcement-posture-is-explicit-reported-and-recorded)).

## The core property

> The agent must not be able to execute project build or test code except by asking Clyde.

This is a property of the agent's *environment*, not of the agent's behaviour or of policy text — and, importantly, not necessarily of an environment Clyde provides. A host with no Rust toolchain has the same property. The warden's contribution is to provide it *reliably* rather than to be the only possible source of it ([D1](decisions.md#narrowing-the-property-is-toolchain-absence-not-clyde-hosting)).

Where Clyde does provide it, two structural things achieve it:

1. The agent runs in a sandbox whose runtime root contains **no project build toolchain** — no `cargo`, no `rustc`, no `node`, no browser, no signing tools, no container client. Not "forbidden": absent.
2. That sandbox has no egress except the model API allowlist, no credentials, no host home directory, and writable access only to the paths its lease permits.

An agent in that environment cannot run `cargo check` even if it decides to. `run_task` is the only path, and `run_task` goes through policy.

## Sandbox composition

Each actor session gets its own workspace-environment sandbox.

| Mount | Mode | Notes |
|---|---|---|
| lease `edit_paths` | read-write | the writable surface; this *is* the edit-scope enforcement |
| lease `read_paths` | read-only | additional context the lease permits |
| `.git` | read-only | prevents hook and config planting ([D8](../builder/decisions.md#d8-brokered-gitpush-uses-the-developers-existing-credential-inside-the-broker-only)) |
| `runtimeRoots.workspace` closure | read-only | nix closure ([D6](../builder/decisions.md#d6-runtime-roots-are-nix-closures-not-oci-images)) |
| `AGENTS.md`, `.clyde/policy.toml` | read-only | guidance and visible policy |
| `/run/clyde/clyded.sock` | read-write socket | actor API only, never the admin socket |
| `/run/clyde/session-token` | read-only, `0400` | capability token ([D2](../builder/decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel)) |
| `/run/clyde/egress.sock` | read-write socket | proxy bridge, `model-api` profile |
| Clyde CA certificate | read-only | so the proxy can terminate `model-api` and inject auth; **not** given to build sandboxes |
| `/tmp`, `/scratch` | read-write tmpfs | scratch |

**Not present:** host home, `~/.ssh`, `~/.gnupg`, cloud config, browser profiles, other projects, the admin socket, the broker socket, any container runtime socket, the mission cache, the snapshot store, and the artifact blob store.

Artifacts and logs are read through the API, not through a mounted store, so the agent cannot read another mission's outputs by walking a filesystem.

This runs on the **namespace backend**, and that is a deliberate choice rather than a gap. The workspace environment needs a genuinely writable, host-visible working tree, and Firecracker has no filesystem passthrough — putting it behind a block device would mean inventing a write-back path for ordinary file edits ([D24](../builder/decisions.md#d24-firecracker-is-the-default-backend-for-build-execution)). The boundary here holds a semi-trusted agent, not hostile dependency code, and the residual risk is [stated](../builder/roadmap.md#risk-7-the-workspace-environments-model-api-egress-is-an-exfiltration-path) rather than papered over.

### Runtime root contents

`runtimeRoots.workspace` provides text and code manipulation: a shell, coreutils, `grep`/`ripgrep`, `sed`/`sd`, `find`/`fd`, `jq`, `diff`/`patch`, a structural-rewrite tool, and a general-purpose scripting interpreter for one-off codemods.

It must **not** provide `cargo`, `rustc`, `rustup`, `node`, `npm`/`pnpm`/`yarn`, a browser, `gpg`, `ssh`, `docker`/`podman`, or a package manager that could install any of them. This is asserted by a test over the derivation's closure, not by review, and asserted again by the daemon before it starts an environment, so a hand-configured root cannot quietly reintroduce one.

## Agent hosting

The command clyded execs comes from host or user configuration only, never from repository configuration ([D20](decisions.md#d20-the-agent-command-is-host-or-user-configuration-never-repository-configuration)):

```toml
[agent]
command = "claude"
args = ["--mcp-socket", "/run/clyde/clyded.sock"]
env = ["TERM", "LANG"]        # allowlist; the sandbox environment is otherwise cleared
```

The agent binary must be reachable inside the sandbox, which means it comes from the workspace runtime root or an explicitly mounted read-only path recorded in the sandbox spec. It is not copied out of the host's `PATH` implicitly.

**The absence of an `[agent]` section is what makes a deployment builder-only.** clyded must treat that as a supported configuration and not an error: it starts, serves the operator and actor surfaces, reports `advisory` posture, and never attempts to launch an environment. If an `[agent]` section is present but the configured command is absent from the sandbox, mission activation fails with a clear diagnostic rather than starting a sandbox with nothing in it.

## Editing authority is mount topology

Earlier drafts had every file write pass through an `edit_files` API call so clyded could enforce path scope. With the agent inside a sandbox, the bind-mount table already expresses exactly that scope, enforced by the kernel rather than by a check in a request handler.

Consequences:

- **No `read_code` or `edit_files` API.** The agent uses its ordinary filesystem tools. This is also why an off-the-shelf agent works without modification.
- **Sub-agent narrowing is real.** A derived lease with a narrower `edit_paths` yields a sandbox with a narrower writable set. The sub-agent cannot write outside it, whatever it attempts.
- **Edit auditing is diff-based.** Clyde records diffs at snapshot and task boundaries and at mission closeout, rather than a stream of per-write records. This is a genuine reduction in audit fidelity relative to the earlier design, accepted knowingly: the boundary that matters (what can be written) is stronger, and the record (what was written) is complete even if the sequence is coarser. An optional exec-logging shim could add command-level records ([OQ3](decisions.md#oq3-exec-logging-shim-in-the-workspace-environment)); it is not a boundary.

Under the builder alone, the diff is all there is: nothing prevents an out-of-scope write, and the closing diff is where it becomes visible. Prevention is one of the three things hosting the agent buys.

## `workspace.edit` after this change

The original design required a separate low-authority runtime for helper-driven edits — codemods, batch rewrites, ad hoc scripts — with a hard rule that it be distinct from build/test/fetch runtimes and carry no project build toolchain.

The agent's own environment satisfies every one of those requirements ([D17](decisions.md#d17-workspaceedit-helper-execution-is-the-workspace-environment)). So an agent writing and running a codemod is simply the agent working, with no task request and no sandbox launch. `workspace.edit` remains in the task catalog as the name for that class of activity — it is what appears in the audit record and what a lease's `task_scope` authorises — but in the MVP it is not a Clyde-launched sandbox.

The rule that mattered survives intact, and in a stronger form: work needing the project build toolchain *cannot* happen in the workspace environment, because the toolchain is not there. It has to become `rust.check`, `rust.test.unit`, or another typed task, which is exactly the intended funnel.

The three workspace task types are inert in a builder-only deployment. The warden activates them.

## Actor API

MCP over `clyded.sock` as newline-delimited JSON-RPC ([D19](../builder/decisions.md#d19-mcp-over-the-actor-socket-is-line-framed-json-rpc)), authenticated with the session token.

| Tool | Product | Purpose |
|---|---|---|
| `mission_status` | builder | objective, scope, allowed tasks, budget remaining, expiry |
| `list_capabilities` | builder | what this lease may do now, and what would need escalation |
| `run_task` | builder | request a typed task; returns a task run id |
| `task_status` | builder | state and outcome, including structured failure classification |
| `task_logs` | builder | streamed or ranged log access for a task run |
| `list_artifacts` | builder | artifacts produced by this mission's task runs |
| `request_publish` | builder | ask for a brokered operation |
| `commit_prepare` | builder | ask clyded to build a commit proposal from the working tree |
| **`request_escalation`** | **warden** | ask for capability beyond the lease, with reason and alternatives |
| **`request_subagent`** | **warden** | ask for a derived lease and a sub-agent session |

The builder tools serve any machine driver — CI, or an agent running outside Clyde — and are what make the pipeline usable without changing how anyone works. The two warden tools are the ones that only make sense when there is an agent to ask on behalf of, and a human to ask. A human driving the builder needs neither: the escalation is just running the fetch task, and there is no sub-agent.

The structured denial paths `request_escalation` consumes already exist in the builder; what the warden adds is an actor that can raise one.

**Tool descriptions carry policy.** Each description states the policy consequences of calling it — whether it can trigger network access, whether it requires approval, what it will and will not do. The description is part of the security UX: the agent's understanding of its constraints comes from here, and a vague description produces an agent that asks for the wrong things.

**Denials are actionable.** A denied request returns the structured `PolicyReason` set ([schema](../builder/schema.md#policy-decision)), rendered as: what was denied, which constraint denied it, and what the narrower or escalated alternative is. An agent that receives "denied" with no path forward will either loop or try to work around the boundary; both are worse than a clear next step.

This matters particularly for access drift ([D18](../builder/decisions.md#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift)). "Build failed" would send the agent hunting through its own code for a bug that isn't there. "This build tried to read `crates/proto/schema.sql`, which is outside the confirmed access baseline for this target; a human must confirm the change" tells it to raise an escalation instead — and it should never be able to confirm one itself.

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

## Sub-agents

Sub-agent derivation only arises where Clyde hosts the agent, because a derived lease is *enforced* by giving the child a narrower mount table. A builder-only deployment has one lease and whatever driver holds it.

Each derived lease yields its own workspace-environment sandbox, following the same sequence from step 3 above. The derivation rules themselves are the builder's ([the eight-rule check](../builder/schema.md#derivation-rules-normative)) and the lease manager already implements them; the warden tests them against a running agent. Revocation fans out to derived leases and sessions in the same transaction as the parent.

The MVP permits **one level** of derivation: a child cannot spawn further children. Note that some egress profiles are [incomparable rather than ordered](../builder/tasks-and-policy.md#profile-ordering), and an incomparable request is an escalation, not a derivation.

## The model-API channel

The workspace environment reaches the model API through the Clyde proxy under the `model-api` profile ([D11](decisions.md#d11-workspace-environment-model-api-egress-goes-through-the-clyde-proxy)). It is the environment's **only** egress.

The proxy and forwarder are builder components. What the warden adds is the termination carve-out ([D7 amendment](../builder/decisions.md#amendment-selective-tls-termination-for-model-api-only)):

- CA generation at first run; the private key is host-only, mode `0600`, never in a sandbox, an artifact, or an audit payload
- the CA certificate mounted read-only into workspace environments, with `SSL_CERT_FILE`, `NODE_EXTRA_CA_CERTS`, and equivalents set; **not** mounted into build or fetch sandboxes
- TLS termination for `model-api` hosts only, with `Authorization` injected host-side and normal upstream certificate verification
- pass-through with no interception for every other profile
- metadata-only logging for terminated connections: host, request path, status, byte counts, timing. No body logging, and no flag that enables it
- per-mission request and byte budgets, since the agent can spend the credential without holding it

**The credential is not in the sandbox.** The agent authenticates without ever possessing a key, which resolves what would otherwise be a contradiction: the workspace environment is specified as holding no credentials, yet something must authenticate. Injecting the key into the sandbox was the simpler option and was rejected, so that a compromised or prompt-injected agent cannot exfiltrate a reusable credential — it can only spend the channel while it holds a lease, bounded by the mission's budgets.

**The channel is still an exfiltration path.** The endpoint accepts arbitrary request bodies, so an agent that wanted to send repository content out could. That is not solved, and should not be described as if it were. What the design provides:

- it is the **only** egress from the workspace environment, and it is loopback-proxied, allowlisted, and recorded
- untrusted *dependency* code never runs in this environment, so exfiltration requires the agent itself to misbehave, not a hostile crate
- build and test environments have no egress at all, so hostile dependency code has no channel even if it can read the snapshot
- the credential cannot be lifted and used elsewhere

**What termination costs.** The proxy now handles model plaintext, which is a real trusted-surface increase. It must not log bodies, and the carve-out is scoped to model hosts alone — build and fetch sandboxes never receive the CA certificate, so they cannot be transparently intercepted even by Clyde. Keeping the carve-out to one profile is what bounds that cost.

The residual risk is a misbehaving or prompt-injected agent, a different threat class from the supply-chain focus of the [threat model](../threat-model.md), and not claimed to be addressed in the MVP.

## Human interaction

The human works on the host: the `clyde` CLI on the admin socket for mission creation, approvals, status, revocation, and — when they want to drive a task themselves — task execution ([D25](../builder/decisions.md#d25-task-execution-has-an-operator-surface-on-the-admin-socket)). The human may also edit files directly in the workspace with their own editor; the agent's sandbox binds the same live tree, so both see each other's changes.

The operator task surface does not weaken anything here. A human running `rust.check` themselves is admitted against the same lease, policy, budget, and baseline as the agent would be, and the run is recorded with the human as the acting principal. What stays impossible is a *sandboxed actor* reaching the admin socket at all.

`clyde approve` is an operator command and refuses to run inside a sandbox, because the admin socket is never mounted into one. That refusal is the mechanism that makes agent self-approval impossible.
