# Changelog

## Unreleased — the Clyde Next MVP

The MVP slice, Phases 0 through 4, implemented against the design document set in
`docs/`.

### Phase 0 — foundations

- Nix flake as the source of truth for tooling, with the three runtime roots as
  nix closures and a `checks` output that asserts over the closure of
  `runtimeRoots.workspace` that no project build toolchain is reachable from it.
  Adding `cargo` to that root fails `nix flake check`.
- Every entity in the schema reference as a validated Rust type: newtype
  identifiers, state machines as transition functions returning `Result`, and
  redaction that is structural — credential-bearing types do not implement
  `Serialize`, so they cannot reach a log line or an audit payload by accident.
- The four functions the security model rests on, as pure functions with denial
  cases tested first: task policy resolution, lease derivation, egress profile
  ordering, and budget charging.
- Configuration layering in which the repository layer may only narrow, and a
  widening value is a rejection with a diagnostic rather than a silent clamp.
- Storage with an append-only, hash-chained audit log whose head is recorded
  separately, because truncating the tail of a bare chain is otherwise
  undetectable.
- `clyde doctor`, which distinguishes "user namespaces unavailable" from "blocked
  by AppArmor" and works without a running daemon.
- Fifteen test fixtures, each stating the property it asserts in its own README.

### Phase 1 — mission, lease, and approval

- Two sockets, and the separation is the mechanism rather than a policy: the
  admin socket is never mounted into a sandbox, so an agent cannot approve its
  own request. `clyde approve` refuses inside a sandbox and says why.
- Mission proposal produces the exact envelope that will be issued, including the
  caveats the approval UX must state rather than imply.
- Capability tokens delivered by file, mode 0400, never in an argv or an
  environment. Unknown, expired, and revoked tokens are rejected identically.
- The egress proxy, the Clyde CA, and the in-sandbox forwarder. Profile `none` is
  the absence of a socket rather than a flag.

### Phase 2 — snapshots and isolation

- One `SandboxSpec`, two backends. Bubblewrap argv construction, seccomp filters,
  limit wrapping, and Firecracker VM configuration are all pure functions over a
  spec, so what Clyde runs is a value a test can assert on.
- Access baselines with the two-tier model: subtree grants for first-party code
  and individually confirmed pins for everything outside them. Editing inside a
  grant produces zero drift and zero prompts, which is the property the whole
  control depends on.
- Structured failure classification that recognises host and policy conditions
  before anything is attributed to the user's code.

### Phase 3 — dependency resolution

- `rust.resolve-deps` with a manifests-only input snapshot and the
  `rust-registry` egress profile, refused on a host that cannot provide microVM
  isolation rather than downgraded.
- The code-execution inventory, checked before the sandbox starts, with a
  same-version content change reported distinctly from an upgrade.

### Phase 4 — the credential broker

- A broker that holds the credential in one field of one struct and validates
  every request on its own account, including reading the approval record rather
  than trusting the caller.
- Brokered push from a sanitised temporary repository. A fixture with eight
  hostile hooks, a transport rewrite pointing at a decoy remote, an
  object-transfer hook, and content filters executes nothing.
- The terminal client, and mission review, which verifies the audit chain as part
  of the review and sorts refused egress attempts to the top.
