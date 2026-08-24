# Phase 4: Credential Broker and Brokered Git Push

## Goal

Separate code execution from authority: an agent can prepare a commit and request a push, and the push happens through the broker with human approval, without any credential ever being reachable from an agent or a build sandbox.

Phase 4 completes the MVP slice.

Decisions in force: [D2](decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel), [D8](decisions.md#d8-brokered-gitpush-uses-the-developers-existing-credential-inside-the-broker-only), [D13](decisions.md#d13-cli-through-phases-1-3-tui-in-phase-4), [D15](decisions.md#d15-three-binaries-from-phase-0).

## Deliverables

### 1. Broker gateway in clyded

A single adapter through which all privileged external operations pass. It translates a typed authority request into a broker call, attaches the approval reference, and records the operation. No other part of clyded may call the broker.

### 2. `clyde-brokerd`

The broker daemon gains real operations. Its interface is capability-oriented, never secret-oriented ([technology choices](technology-choices.md#broker-api-style)): `git_push(...)` exists; `get_ssh_key()` does not and must not.

Constraints:

- listens on `brokerd.sock`, mode `0600`, never mounted into any sandbox
- accepts requests only from clyded, verified by `SO_PEERCRED`
- validates every request independently — the broker does not trust clyded's word that an approval exists; it verifies the approval record itself
- holds credentials in process only, never writes them anywhere, never logs them, and its types do not implement `Serialize` or a revealing `Debug`
- refuses any operation whose approval is missing, expired, already consumed, or whose digest does not match the request exactly

### 3. `git.commit.prepare`

A trusted clyded operation, not an agent operation:

- computes the scoped diff from the live working tree
- builds a commit proposal: message, author, file list, diff summary, and the tasks that passed against this tree
- on human approval, creates the local commit using sanitised git invocations
- stores the proposal as an artifact linked to the mission

`.git` is read-only inside workspace environments, so an agent cannot create commits directly, and cannot plant hooks or repository configuration that a later trusted git invocation would execute.

### 4. `git.push`

Request carries repository, commit id, remote name, and refspec. Validation, in order:

1. lease authority `may_request_publish`
2. remote is in the configured allowlist
3. branch matches the configured allowlisted patterns, with protected-branch patterns refused outright
4. commit exists, is reachable, and its tree matches what was approved
5. an approval decision exists whose `request_digest` equals this request's normalised digest, unexpired and unconsumed

Then the broker performs the push and records the result. Approval consumption and push execution are transactional: no state where the approval is consumed but the push did not run, or vice versa.

### 5. Hostile repository hardening

A repository's `.git/config` and `.git/hooks` are attacker-controlled content in this threat model — an agent, or any prior compromise, may have written them. `git push` executes local hooks and honours repository configuration, so a naive broker implementation runs untrusted code with credentials in scope. That is the exact privilege pivot the design exists to prevent.

The broker therefore pushes from a **sanitised temporary repository**:

1. create an empty repository in broker-owned scratch
2. fetch the approved commit from the workspace repository by path, with hooks disabled and object-transfer hooks (`uploadpack.packObjectsHook`) neutralised
3. verify the fetched commit id and tree digest match the approval
4. add the allowlisted remote explicitly, ignoring any workspace repository remote configuration
5. push with hooks disabled, and with system and global git configuration neutralised (`GIT_CONFIG_SYSTEM=/dev/null`, `GIT_CONFIG_GLOBAL=/dev/null`) so `url.*.insteadOf`, `core.sshCommand`, and similar rewrites cannot redirect the transport
6. destroy the scratch repository

Every trusted git invocation in Clyde — including `git.commit.prepare` and diff computation in Phase 1 — uses the same sanitised invocation helper. There should be exactly one place in the codebase that constructs a git command, and it should be hard to use unsafely.

### 6. Approval UX for push

The prompt shows remote, resolved URL, branch, commit id and subject, diff statistics, requesting actor, mission, and the passing task evidence for that tree. Approval is per-push by default; `ApproveForMission` is available but scoped to a branch pattern, never to "any push".

### 7. TUI

The ratatui client, now that mission, task, and approval surfaces are stable ([D13](decisions.md#d13-cli-through-phases-1-3-tui-in-phase-4)): mission and lease status with budget, live task list and logs, pending approvals with full policy detail, and a mission timeline. It is a client of the same admin and actor APIs — no privileged path exists only in the TUI.

### 8. Mission review

The end-to-end review surface: objective, files changed, tasks run with pass/fail and policy digests, escalations requested and their outcomes, approvals granted, egress attempts, brokered operations, budget consumed, and the closing diff. This is what makes autonomous work trustworthy after the fact, and it is the last MVP deliverable for a reason — everything before it feeds it.

## Security properties this phase must demonstrate

- no SSH agent socket, key file, or token is present in any sandbox mount table, verified by a test that inspects every spec the system can generate
- an agent cannot invoke the broker: the socket is absent from its sandbox and no actor-facing operation reaches the broker except `request_publish`, which only creates an approval request
- a push cannot occur without a matching, unexpired, unconsumed approval, verified including the tamper cases: altered commit, altered refspec, altered remote
- a repository containing a hostile `pre-push` hook and a hostile `.git/config` cannot execute anything during a brokered push, verified by a fixture that would create a marker file if either ran
- protected-branch and non-allowlisted-remote pushes are refused
- mission revocation freezes in-flight brokered operations rather than cancelling them silently

## Exit criteria

- an agent prepares a commit, requests a push, and the push succeeds only after human approval on the admin socket
- no credential is reachable from any agent or build sandbox, demonstrated by the mount-table test
- the hostile-hook and hostile-config fixtures execute nothing
- push prompts show remote, branch, commit, actor, and task evidence
- the broker independently verifies approvals rather than trusting the caller
- the TUI presents mission, task, approval, and audit surfaces over the same APIs as the CLI
- mission review shows the complete chain from objective to pushed commit

## Explicitly not in Phase 4

No signing, no publishing, no private registry credentials, no scoped token minting, no multi-user or shared-runner support. Those follow the MVP.
