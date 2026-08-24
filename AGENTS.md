# AGENTS.md

## Purpose

This file defines repo-local guidance for humans and coding agents working on Clyde.

Clyde is security-critical software. It coordinates isolated environments, handles authority boundaries, and must protect credentials and other sensitive material. Code quality, failure handling, and security hygiene are core requirements, not optional refinements.

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
