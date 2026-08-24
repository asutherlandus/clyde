# Clean-Sheet Clyde: Task Policy Matrix

## Purpose

This document defines a high-level task and policy matrix for Clyde Next.

It translates the threat model, requirements, high-level architecture, and mission/lease model into a concrete catalog of typed operations. The goal is to make it clear:
- what kinds of tasks Clyde supports
- which trust level each task belongs to
- what runtime and isolation profile should be used
- what network, filesystem, and credential access is allowed
- which tasks are safe for autonomous agent loops
- which tasks require escalation or human approval

This document builds on:
- [problem-statement-threat-model.md](problem-statement-threat-model.md)
- [requirements.md](requirements.md)
- [high-level-design.md](high-level-design.md)
- [mission-lease-model.md](mission-lease-model.md)

## Design Intent

The matrix exists to prevent Clyde from collapsing back into "run arbitrary shell in a container."

Instead of a flat command model, Clyde should expose a set of typed tasks with explicit policy. A mission or lease may authorize a task family such as `rust.check`, but the task policy still determines exactly how that task runs.

## Policy Dimensions

Each task profile should define at least the following dimensions:
- **trust class**
- **runtime class**
- **input scope**
- **writable outputs**
- **network policy**
- **credential policy**
- **human approval policy**
- **agent autonomy policy**
- **audit/provenance level**

## Trust Classes

Clyde Next should classify tasks into five broad trust classes.

### T0: Read/Edit only
- no project code execution
- no credentials
- no network required
- safe for broad agent autonomy within lease

### T1: Trusted tool execution
- trusted built-in tools only
- project code not executed as code
- low risk

### T2: Untrusted offline execution
- project code may execute
- no external network
- no credentials
- main inner-loop class for compile, unit test, and build

### T3: Constrained external execution
- project code may execute
- tightly scoped network or synthetic services allowed
- no raw credentials
- escalation usually required unless pre-approved by mission/policy

### T4: Authority operations
- no untrusted project code execution in same context
- brokered credentials and privileged effects
- push, sign, publish, real identity access
- strong approval and audit requirements

## Runtime Classes

Clyde should support a small set of runtime classes.

### R0: In-process trusted operation
For simple control-plane or workspace actions.

### R1: Trusted tool runner
For trusted binaries that inspect or transform files without executing repo code.

### R2: Hardened container
For medium-risk tasks or compatibility paths.

### R3: MicroVM or equivalent strong sandbox
Preferred for untrusted build, dependency install, browser, and arbitrary repo execution.

### R4: Credential broker
For git push, signing, publish, and credential-mediated actions.

## Global Policy Defaults

Unless a task explicitly says otherwise:
- source inputs come from immutable snapshots
- repo access is subtree-scoped where possible
- outputs are written to explicit output channels
- host home directory is never mounted
- `~/.ssh`, `~/.gnupg`, browser profiles, cloud config, and Docker sockets are never mounted into untrusted tasks
- network is denied by default
- credentials are unavailable by default
- tasks are attributable to mission + lease + actor

## Task Policy Summary Matrix

| Task | Trust | Runtime | Source input | Writable output | Network | Credentials | Agent autonomy | Approval |
|---|---|---|---|---|---|---|---|---|
| `workspace.read` | T0 | R0 | live workspace scoped | none | none | none | yes | none |
| `workspace.edit` | T0 | R0 | live workspace scoped | repo paths in lease | none | none | yes | none |
| `repo.search` | T0 | R0/R1 | live workspace scoped | none | none | none | yes | none |
| `format.trusted` | T1 | R1 | snapshot or live scoped | allowed repo paths | none | none | yes | none |
| `lint.static` | T1 | R1 | snapshot scoped | logs only | none | none | yes | none |
| `rust.resolve-deps` | T3 | R3 | snapshot + lockfiles | dep bundle/cache only | registry-only | brokered private-dep access if needed | conditional | usually yes |
| `rust.check` | T2 | R3 | snapshot scoped | build outputs/logs | none | none | yes | none |
| `rust.build` | T2 | R3 | snapshot scoped | build/package outputs | none | none | yes | none |
| `rust.test.unit` | T2 | R3 | snapshot scoped | test logs/coverage | none | none | yes | none |
| `rust.test.integration.synthetic` | T3 | R3 | snapshot scoped | test logs/artifacts | synthetic only | none | yes if mission allows | maybe |
| `rust.test.integration.external` | T3 | R3 | snapshot scoped | test logs/artifacts | scoped external | scoped tokens only | no/limited | yes |
| `node.resolve-deps` | T3 | R3 | snapshot + lockfiles | dep bundle/cache only | registry-only | brokered private-registry access if needed | conditional | usually yes |
| `web.build` | T2 | R3 | snapshot scoped | build assets/logs | none | none | yes | none |
| `browser.test.synthetic` | T3 | R3 | snapshot scoped | screenshots/traces/logs | synthetic only | none | yes if mission allows | maybe |
| `browser.test.external` | T3 | R3 | snapshot scoped | screenshots/traces/logs | scoped external | synthetic or scoped test identity only | no/limited | yes |
| `service.run.synthetic` | T3 | R3 | snapshot + config | ephemeral service state | synthetic only | none | yes if mission allows | maybe |
| `artifact.package` | T2 | R3 | build outputs only | package artifacts | none | none | yes | none |
| `git.fetch` | T4 | R4 | repo ref request only | fetched refs/metadata | brokered git only | brokered | no direct autonomy unless policy allows | yes/policy |
| `git.commit.prepare` | T1/T4 | R0/R4 | workspace diff | commit object/proposal | none | none or brokered sign-off later | yes for prepare | none |
| `git.push` | T4 | R4 | commit/ref selection | remote branch update | brokered git only | brokered | no | yes |
| `artifact.sign` | T4 | R4 | digest or manifest only | signature | none or brokered signer | brokered signing key | no | yes |
| `artifact.publish` | T4 | R4 | selected artifact/digest only | registry release state | brokered external only | brokered publish creds | no | yes |
| `shell.untrusted` | T2/T3 | R3 | snapshot scoped | explicit outputs only | none by default | none | conditional | yes or policy |

## Detailed Task Families

## 1. Workspace and planning tasks

### `workspace.read`
Purpose:
- read files
- inspect config
- analyze code structure

Policy:
- trust: T0
- runtime: R0
- source: live workspace paths within lease
- writes: none
- network: none
- credentials: none

Usage:
- always safe within mission scope
- core primitive for human and agent reasoning

### `workspace.edit`
Purpose:
- modify files
- create patches
- refactor scoped code

Policy:
- trust: T0
- runtime: R0
- writes limited to lease repo scope
- no network
- no credentials

Usage:
- primary inner-loop operation for agents
- should be budgeted and auditable

### `repo.search`
Purpose:
- find symbols, references, config, ownership boundaries

Policy:
- trust: T0
- runtime: R0 or R1
- no writes
- no network
- no credentials

## 2. Trusted tool tasks

### `format.trusted`
Purpose:
- run trusted formatter wrappers such as `rustfmt`, `prettier`, or similar tools when invoked in a trusted mode

Policy:
- trust: T1
- runtime: R1
- no external network
- no credentials
- only writes to scoped repo files

Notes:
- if a formatter executes project plugins or untrusted config code, it must be reclassified into T2/T3

### `lint.static`
Purpose:
- run trusted static analyzers that do not execute project hooks or plugin code

Policy:
- trust: T1
- runtime: R1
- snapshot inputs preferred
- write logs only
- no network
- no credentials

## 3. Dependency resolution tasks

### `rust.resolve-deps`
Purpose:
- retrieve crates and toolchain inputs needed for later networkless Rust compilation

Policy:
- trust: T3
- runtime: R3 preferred
- inputs: source snapshot, `Cargo.lock`, cargo config, dependency policy
- writes: dependency bundle, registry cache, fetch manifest, SBOM fragments
- network: registry-only or mirror-only
- credentials: none by default; brokered access only for approved private sources

Approval guidance:
- may be pre-approved by repo policy for public mirrored dependencies
- should require approval for private git dependencies, lockfile drift, or broader network

### `node.resolve-deps`
Purpose:
- retrieve npm/pnpm/yarn packages and metadata needed for later builds/tests

Policy:
- trust: T3
- runtime: R3 preferred
- network: npm proxy only or approved mirrors only
- credentials: brokered private-registry access only when explicitly allowed
- writes: dependency bundle/cache only

Approval guidance:
- same pattern as Rust: safe public mirrored installs may be pre-approved, private or drift-sensitive fetches should escalate

## 4. Offline build and compile tasks

### `rust.check`
Purpose:
- compile and type-check Rust code, including proc macros and `build.rs`

Policy:
- trust: T2
- runtime: R3 preferred
- source: immutable snapshot
- writes: build outputs and logs only
- network: none
- credentials: none
- caches: read-only dependency inputs; isolated writable build dirs

Agent guidance:
- should be a standard pre-authorized inner-loop task

### `rust.build`
Purpose:
- produce binaries or libraries without publish authority

Policy:
- same as `rust.check`
- outputs include explicit artifact directory
- package signing remains separate

### `web.build`
Purpose:
- compile frontend assets and server-side bundles in an offline sandbox

Policy:
- trust: T2
- runtime: R3 preferred
- network: none
- credentials: none
- outputs: static assets, bundles, logs

Notes:
- any plugin or framework hook should still be treated as untrusted execution

## 5. Test tasks

### `rust.test.unit`
Purpose:
- run unit tests and doctests in isolated no-network sandboxes

Policy:
- trust: T2
- runtime: R3
- network: none
- credentials: none
- writes: logs, coverage, failure artifacts

### `rust.test.integration.synthetic`
Purpose:
- run integration tests against fake or internal-only services

Policy:
- trust: T3
- runtime: R3
- network: synthetic only
- credentials: none
- outputs: logs, coverage, traces

Notes:
- preferred over real-service integration during autonomous agent loops

### `rust.test.integration.external`
Purpose:
- run integration tests that must talk to a real external staging or partner system

Policy:
- trust: T3
- runtime: R3
- network: destination-scoped external only
- credentials: short-lived scoped tokens only, if absolutely required
- approval: yes

### `browser.test.synthetic`
Purpose:
- run browser or end-to-end tests with fake identity and isolated browser state

Policy:
- trust: T3
- runtime: R3
- network: synthetic internal environment only
- credentials: none
- outputs: screenshots, videos, traces, logs

### `browser.test.external`
Purpose:
- run browser tests against a real preview/staging stack

Policy:
- trust: T3
- runtime: R3
- network: destination-scoped preview/staging only
- credentials: scoped test identity only; never developer browser profile
- approval: yes

## 6. Service and local environment tasks

### `service.run.synthetic`
Purpose:
- start ephemeral local-like dependencies for testing, such as fake SMTP, fake OAuth, ephemeral Postgres, Redis, or S3-compatible services

Policy:
- trust: T3
- runtime: R3 or tightly controlled R2
- network: internal synthetic only
- credentials: synthetic only
- persistence: ephemeral by default

Usage:
- critical for making full-stack testing compatible with least privilege

## 7. Packaging and artifact tasks

### `artifact.package`
Purpose:
- take build outputs and create distributable packages or images without publishing them

Policy:
- trust: T2
- runtime: R3
- inputs: build outputs only where practical
- network: none
- credentials: none
- writes: package artifacts, manifests

Notes:
- packaging should remain separate from signing and publish

## 8. Brokered authority tasks

### `git.fetch`
Purpose:
- retrieve refs or repository contents that require credentials

Policy:
- trust: T4
- runtime: R4
- network: broker-controlled git transport only
- credentials: brokered
- source code execution: none in broker context

Notes:
- when possible, fetch outputs should be handed off as artifacts or snapshots rather than raw credentialed repo access

### `git.commit.prepare`
Purpose:
- prepare a commit proposal from workspace changes

Policy:
- trust: mostly T1, with possible T4 follow-on if signing is involved
- runtime: R0 for diff/metadata, R4 only if brokered signing step is requested later
- network: none
- credentials: none by default

Usage:
- safe for agent autonomy to prepare, but not necessarily finalize with authority

### `git.push`
Purpose:
- push selected refs to a remote

Policy:
- trust: T4
- runtime: R4
- network: brokered git destination only
- credentials: brokered only
- approval: yes unless explicitly policy-automated for narrow cases

### `artifact.sign`
Purpose:
- sign digests, manifests, commits, or release metadata

Policy:
- trust: T4
- runtime: R4
- input: explicit digest or manifest only
- raw artifact browsing by signer should be minimized
- credentials: brokered signing key only
- approval: yes

### `artifact.publish`
Purpose:
- publish crates, packages, images, or release artifacts

Policy:
- trust: T4
- runtime: R4
- input: explicitly selected artifact set
- network: brokered destination only
- credentials: brokered registry credentials only
- approval: yes

## 9. Escape-hatch tasks

### `shell.untrusted`
Purpose:
- permit a narrow compatibility path for arbitrary repo commands that do not yet have a dedicated task type

Policy:
- trust: T2 or T3 depending on network
- runtime: R3
- source: immutable snapshot
- network: none by default
- credentials: none
- writes: explicit outputs only
- approval: yes by default, or policy-controlled for trusted teams

Notes:
- this exists as a migration path, not as the preferred steady-state model

## Inner-Loop vs Boundary-Crossing Tasks

## Safe inner-loop tasks
These should generally be allowed inside a mission lease without repeated human approval, if they remain within scope and budget:
- `workspace.read`
- `workspace.edit`
- `repo.search`
- `format.trusted`
- `lint.static`
- `rust.check`
- `rust.build`
- `rust.test.unit`
- `web.build`
- `artifact.package`
- `browser.test.synthetic` when already mission-approved
- `rust.test.integration.synthetic` when already mission-approved

## Boundary-crossing tasks
These should normally require escalation, special mission policy, or direct human approval:
- `rust.resolve-deps`
- `node.resolve-deps`
- `rust.test.integration.external`
- `browser.test.external`
- any task requesting broader repo scope
- any task requesting real external identities or credentials
- `git.fetch` when credentialed private sources are involved
- `git.push`
- `artifact.sign`
- `artifact.publish`
- `shell.untrusted` with any network access

## Language-Specific Guidance

## Rust-specific notes
- `cargo check`, `cargo build`, `cargo test`, proc macros, and `build.rs` all belong in untrusted execution classes
- dependency fetch must be separate from compile/test
- networkless compilation is the default
- cargo caches should be treated as artifacts or controlled caches, not open host mounts

## Full-stack notes
- package-manager install and frontend build must be separated when practical
- browser tests should use isolated browser state
- staging access should use scoped test identities, not developer credentials
- synthetic services should be first-class and preferred

## Relationship to Missions and Leases

The matrix defines what a task means. Missions and leases define who may use which tasks.

### Example
A lease might allow:
- repo scope: `backend/auth`
- task scope: `rust.check`, `rust.test.unit`

That means the agent may request those tasks, but the matrix still controls:
- sandbox class
- network denial
- output locations
- credential unavailability
- audit data captured

This separation prevents agents from redefining task semantics.

## Recommended MVP Matrix

For an initial implementation, Clyde Next should prioritize these task types:
- `workspace.read`
- `workspace.edit`
- `repo.search`
- `rust.resolve-deps`
- `rust.check`
- `rust.test.unit`
- `node.resolve-deps`
- `web.build`
- `browser.test.synthetic`
- `git.push`
- `artifact.sign` later

This subset is enough to validate the core least-privilege and agentic workflow model without requiring full ecosystem coverage on day one.

## Summary

The task policy matrix is the operational bridge between Clyde's security model and its user experience.

It lets Clyde support agentic coding without giving agents ambient power by ensuring that:
- each action belongs to a typed task family
- each task family has explicit isolation semantics
- safe inner-loop tasks can be automated
- boundary-crossing tasks remain visible and reviewable
- credentials stay in brokered authority paths rather than leaking into execution sandboxes
