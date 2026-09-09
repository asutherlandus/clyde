# AGENTS.md

## Purpose

This file defines repo-local guidance for humans and coding agents working on Clyde.

Clyde is security-critical software. It coordinates isolated environments, handles authority boundaries, and must protect credentials and other sensitive material. Code quality, failure handling, and security hygiene are core requirements, not optional refinements.

## Design documents and decisions

Before implementing anything, read [docs/builder/decisions.md](docs/builder/decisions.md). It records the binding implementation decisions for Phases 0-4 with rationale, consequences, and the open questions that are still undecided.

### The two products
The MVP is split into two products ([D23](docs/builder/decisions.md#d23-the-mvp-splits-into-the-builder-and-the-warden)): **the builder**, the sandboxed build pipeline, and **the warden**, the agent harness. They were called Part 1 and Part 2 in earlier drafts and in commit history. Know which one you are working in before you start — it decides which boundaries are load-bearing for the change you are making.

The document set is partitioned to match: [docs/builder/](docs/builder/) and [docs/warden/](docs/warden/), with the vocabulary and threat model shared at [docs/](docs/README.md).

The warden is named for what it holds custody of — the driver's authority — not for confining a prisoner: the agent it hosts is semi-trusted, careless rather than hostile. It creates sandboxes; it is not one. Keep `Sandbox*` in code for the isolation mechanism and reserve "warden" for the product.

Two rules follow from the split and are worth stating as rules:

- **No crate under `crates/` may have a notion of an agent process.** The split is a daemon-layer boundary. A library crate that needs to know whether an agent exists is a sign it has slipped.
- **Do not describe the builder as though the warden were present.** The builder cannot stop a driver executing project code outside Clyde; it reports that as `advisory` posture ([D26](docs/builder/decisions.md#d26-enforcement-posture-is-explicit-reported-and-recorded)). Code comments, error messages, and docs should not claim a guarantee that depends on a deployment shape they cannot see.

### Reading order for implementation work
1. [docs/builder/decisions.md](docs/builder/decisions.md) — what has been decided and why. Read [D23](docs/builder/decisions.md#d23-the-mvp-splits-into-the-builder-and-the-warden) first: it changes what several earlier decisions are decisions *about*. The four warden decisions (D1, D11, D17, D20) live in [docs/warden/decisions.md](docs/warden/decisions.md)
2. [docs/builder/roadmap.md](docs/builder/roadmap.md#the-two-products) — the map of the split
3. the spec for the phase being worked on: [Phase 0](docs/builder/roadmap.md#phase-0-foundations), [Phase 1](docs/builder/roadmap.md#phase-1-mission-lease-and-approval-core), [Parts 1a/1b](docs/builder/roadmap.md#part-1a-snapshots-and-the-namespace-backend), [Phase 3](docs/builder/roadmap.md#phase-3-separate-dependency-resolution), [Phase 4](docs/builder/roadmap.md#phase-4-credential-broker-and-brokered-git-push), or [the warden](docs/warden/spec.md)
4. [docs/builder/schema.md](docs/builder/schema.md) for entity definitions, state machines, and invariants
5. [the egress model](docs/builder/tasks-and-policy.md#the-egress-model) and [docs/warden/design.md](docs/warden/design.md) — the two subsystems whose design is least obvious from the code
6. [docs/security-model.md](docs/security-model.md) — the whole enforced model in one place, graded by how strongly each control holds. Read it before changing a boundary: it names where each property is enforced and which test asserts it

The remaining documents are the conceptual design set. They are consistent with the decisions but are background rather than instructions.

### Conventions
- **Cite decision identifiers.** When code or a commit implements a decision, reference it as `D7`, `D18`, and so on. When a comment explains why a boundary exists, cite the decision rather than restating the rationale.
- **Do not re-litigate settled decisions.** If a decision looks wrong, say so and explain why; do not quietly implement something else. The decision log exists so that the same arguments are not had twice.
- **Open questions are marked `OQ`.** If work reaches one, stop and ask rather than picking an answer. Each open question records a proposed resolution, which is a starting point for the discussion, not a default.
- **Update the docs with the code.** A change to a boundary, schema, or policy semantics changes a document too. A decision reversed in code but not in `decisions.md` is worse than no decision log at all.

### Current state
The MVP slice — Phases 0 through 4 — is implemented, and `rust.check` has now run once inside a real isolation boundary on the namespace backend; that first run found four bugs the suite could not see. It predates the builder / warden split: the document set has been reorganised around it, the code has not. The operator task surface ([D25](docs/builder/decisions.md#d25-task-execution-has-an-operator-surface-on-the-admin-socket)), posture reporting ([D26](docs/builder/decisions.md#d26-enforcement-posture-is-explicit-reported-and-recorded)), and the materialisation mtime contract ([D27](docs/builder/decisions.md#d27-snapshot-materialisation-preserves-change-ordering-in-mtime)) are built.

The microVM backend now has a guest side ([D24](docs/builder/decisions.md#d24-firecracker-is-the-default-backend-for-build-execution)): a flake-built kernel and erofs root images ([R12](docs/builder/decisions.md#r12-the-guest-root-image-is-uncompressed-erofs)), the `clyde-guest-api` job contract, `clyde-init`, block-image construction, and the vsock guest channel carrying all guest output ([R11](docs/builder/decisions.md#r11-all-guest-output-leaves-over-vsock)). **No test has booted a guest**, and no test spawns a sandbox on either backend. It is opt-in per run — an operator raises a task with `--isolation microvm` — because the vsock egress bridge, guest-side learn mode, and the raised policy floor are still to come. [The bring-up guide](docs/builder/microvm-bring-up.md) is the walkthrough.

The [root README](README.md#status) states what has and has not been exercised.

## Tooling source of truth

The project's **Nix flake is the source of truth for all development tooling dependencies**.

This includes, at minimum:
- the Rust toolchain
- Cargo subcommands
- linters
- formatters
- test tools
- documentation tools
- any auxiliary CLIs required for development, testing, or release work

### Requirements
- Do **not** assume host-global tools are available.
- Do **not** introduce ad hoc install steps as the normal development path.
- Do **not** pin the Rust toolchain separately in a way that conflicts with the flake-based workflow unless there is an explicit documented reason.
- New developer tooling dependencies should be added through the flake.
- CI and local development should converge on the same flake-defined toolchain as closely as practical.

### Agent guidance
- Prefer commands executed through the flake-defined environment.
- If a required tool is missing, update the flake or propose a flake update rather than assuming manual installation.
- Treat deviations from the flake-based toolchain as exceptional and call them out explicitly.

## Rust coding standards

All production Rust code in this repository must aim for:
- correctness
- explicitness
- auditability
- predictable failure behavior
- secure handling of sensitive data

This project should favor a **clear, compositional, functional-leaning style** over cleverness, hidden control flow, or panic-prone shortcuts.

### Preferred style
- Prefer small, focused functions.
- Prefer pure transformations where practical.
- Prefer explicit data flow over hidden mutation.
- Prefer total, well-typed APIs over partial behavior.
- Prefer exhaustive `match` handling when it improves clarity.
- Prefer immutable bindings by default; introduce mutation only when it materially improves clarity or performance.
- Prefer iterator- and transformation-oriented style when it remains readable.
- Keep side effects narrow and visible.
- Separate policy/decision logic from I/O and side effects.
- Separate parsing, validation, execution, and persistence concerns.

### Avoid
- clever shortcuts that obscure control flow
- hidden global state
- implicit fallthrough behavior
- large monolithic functions
- mixing authority decisions with low-level execution details
- panic-driven control flow

## Error handling requirements

### Production code must not panic
Production code must not be written in a way that can panic during normal or malformed input handling.

This means:
- no unchecked `unwrap()` in production code
- no unchecked `expect()` in production code unless there is an extremely strong invariant and the use is explicitly justified in comments
- no indexing assumptions that can panic
- no panic-based input validation
- no panic-based security boundary enforcement

### Required approach
- Return `Result` for fallible operations.
- Use structured error types.
- Preserve context when propagating errors.
- Fail closed for security-sensitive policy decisions.
- Distinguish user/input/configuration errors from internal/system errors where useful.
- Treat parsing, serialization, IPC, filesystem, process, and policy resolution as fallible.
- Surface clear diagnostics without leaking secrets.

### Error design guidance
- Prefer domain-specific error enums for core modules.
- Use `thiserror`-style typed errors for well-defined library/module boundaries.
- Add context when crossing subsystem boundaries.
- Keep error messages actionable and audit-friendly.
- Do not log or display raw secrets, tokens, private key material, or sensitive payloads.

### Testing implications
- Add tests for expected failures, not only success paths.
- Test malformed input, denied policy decisions, missing files, bad configuration, and boundary conditions.
- Test that error paths fail safely.

## `unwrap`, `expect`, and similar APIs

### Production code
The default rule is simple:
- **Do not use `unwrap()` in production code.**
- **Do not use `expect()` in production code unless the invariant is explicit, narrow, and documented.**

Any exception should be rare and easy to defend in review.

### Tests and prototypes
- `unwrap()`/`expect()` may be acceptable in tests where failure should fail the test immediately.
- Even in tests, prefer readable helpers over long chains of `unwrap()` where practical.

## Unsafe Rust
- Avoid `unsafe` entirely unless it is truly necessary.
- Any `unsafe` usage must be minimal, documented, and reviewed with extra scrutiny.
- Every `unsafe` block must explain the safety invariant being relied upon.
- Convenience, speed of implementation, or stylistic preference are not sufficient reasons to introduce `unsafe`.

## Security-critical coding guidance

This codebase is security-sensitive and authority-bearing.

### Credential handling
- Never log credentials or secret material.
- Never persist secrets without an explicit, reviewed design.
- Minimize lifetime and scope of sensitive data in memory where practical.
- Prefer brokered capability use over raw credential handling.
- Avoid copying secret values unnecessarily.
- Be explicit about redaction boundaries in logs, errors, and audit records.

### Authority boundaries
- Treat privilege boundaries as first-class design constraints.
- Fail closed on ambiguous or missing policy.
- Keep authorization checks explicit.
- Do not silently widen scope, authority, or network access.
- Do not blur build/test execution with brokered authority.

### Input handling
- Treat all external input as untrusted.
- Validate configuration, IPC input, file input, task requests, and artifact metadata.
- Prefer typed parsing and validation before acting.
- Reject malformed or ambiguous states early.

### Concurrency and lifecycle
- Be careful with revocation, cancellation, expiry, and partial failure.
- Ensure cleanup paths are explicit and tested.
- Avoid race-prone authority checks split far from use sites.

## API and module design
- Keep module boundaries crisp.
- Encode invariants in types when practical.
- Prefer narrow interfaces with explicit inputs/outputs.
- Avoid APIs that mix trusted and untrusted concerns casually.
- Make state transitions explicit.
- Design for auditability: it should be easy to see who requested what, under which policy, and what happened.

## Review standards

Changes should be reviewed for:
- panic safety
- error propagation quality
- security boundary preservation
- least-privilege behavior
- secret handling
- logging/redaction safety
- clarity and maintainability
- test coverage of failure paths

## Preferred development workflow
- use the flake-defined environment
- keep changes scoped and reviewable
- add or update tests with behavior changes
- update docs when architecture, policy, or workflow semantics change
- call out security-sensitive assumptions explicitly in code and review notes

## Agent-specific instructions
- Prefer minimal, explicit changes.
- Do not add dependencies casually; if tooling is needed, add it through the Nix flake.
- Do not introduce `unwrap()`/`expect()` into production code.
- Do not introduce panic-prone behavior at security boundaries.
- Prefer typed errors and explicit propagation.
- Prefer designs that make policy decisions and authority transitions easy to audit.
- When in doubt, choose the more explicit, less magical implementation.

## Short checklist

Before considering a change complete, verify:
- tooling assumptions are compatible with the Nix flake
- production paths do not panic
- errors are handled and propagated with context
- secrets are not logged or exposed
- authority boundaries remain explicit
- tests cover both success and failure paths
- code remains readable and reviewable
