# Clyde Next: Network Egress Model

## Purpose

This document defines how Clyde controls network access for sandboxed work: the egress profile vocabulary, the enforcement mechanism, what the mechanism does and does not guarantee, and how it is realised on each sandbox backend.

It exists because "network: none" and "network: registry-only" appear throughout the design documents as policy values with no stated enforcement. This document makes them mechanical.

Decisions: [D7](decisions.md#d7-registry-only-egress-is-enforced-by-a-clyde-managed-proxy), [D11](decisions.md#d11-workspace-environment-model-api-egress-goes-through-the-clyde-proxy).

## Design constraints

Three constraints shape the whole design:

1. **No privilege.** clyded runs as the developer's own user. Rootless network namespaces cannot create veth pairs into the host, so the usual approach — put the sandbox on its own interface and filter with `nftables` — is unavailable without root or a setuid helper.
2. **One mechanism, all backends.** Whatever the mechanism is, it must work identically under bubblewrap now and Firecracker in Phase 2b ([D9](decisions.md#d9-the-firecracker-backend-lands-as-phase-2b-before-dependency-resolution)), or the plumbing gets rewritten mid-roadmap.
3. **The allowlist decision must be trusted.** If the allowlist is enforced by code inside the sandbox, hostile project code can remove it, and the approval prompt is lying to the user.

## Mechanism

### Sandbox side

Every sandbox, for every profile including `none`, gets an **unshared network namespace with loopback only**. There is no route to the host network, no DNS server reachable, and no interface other than `lo`.

For profiles other than `none`, two additional things are provided:
- a Unix domain socket, bind-mounted into the sandbox, connected to the host-side proxy
- a small trusted forwarder from the runtime root, started as the sandbox's first process, listening on `127.0.0.1:<port>` inside the namespace and bridging accepted connections to that socket

Clients inside the sandbox are configured with `http_proxy` / `https_proxy` / `HTTPS_PROXY` pointing at `127.0.0.1:<port>`, plus the cargo-specific equivalents. The sandbox therefore sees exactly one reachable endpoint, and it is a proxy.

### Host side

The Clyde egress proxy runs in clyded (trusted). It:
- accepts HTTP `CONNECT` only — no plain HTTP forwarding, no TLS interception
- resolves and matches the CONNECT target against the profile's host allowlist
- allows or refuses, and records every attempt either way as an `EgressAttempt` on the task run
- enforces a per-task byte budget and connection count, charged to the lease budget
- emits a `FetchManifest` artifact summarising destinations, bytes, and refusals

Because the proxy is host-side and the sandbox has no other route, the allowlist is enforced in trusted code. Compromising the in-sandbox forwarder gains nothing: it can only reach what the proxy already permits.

### TLS handling: pass-through by default, terminated only for `model-api`

**Default: pass-through.** For every profile except `model-api`, the proxy sees only the `CONNECT` target and forwards opaque bytes. TLS is end-to-end. This is deliberate for `rust-registry` in particular: intercepting dependency traffic would make Clyde a plaintext-handling component in the path of dependency *content*, which would undermine the content-hash pinning that [D18](decisions.md#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift) relies on.

**Carve-out: `model-api` is terminated.** For model hosts only, the proxy terminates TLS, injects the `Authorization` header, and re-originates TLS to the real host. The agent therefore never holds the model credential, which resolves the otherwise-contradictory requirement that the workspace environment hold no credentials while still authenticating ([D11](decisions.md#credential-handling)).

Mechanics:
- Clyde generates a per-installation CA at first run. The private key is host-only: never in a sandbox, never in an artifact, never in an audit payload.
- The CA certificate is mounted read-only into the workspace environment, with `SSL_CERT_FILE`, `NODE_EXTRA_CA_CERTS`, and equivalents pointed at it.
- Build and fetch sandboxes never receive the CA certificate. A sandbox that does not trust the CA cannot be transparently intercepted, which is the property that keeps the carve-out from spreading.
- Upstream certificate verification is performed normally by the proxy. Termination does not mean trusting the upstream less.

**Logging rule.** The proxy sees prompt and completion plaintext for terminated connections and must not log bodies. Recordable metadata is host, request path, status, byte counts, and timing. There is no configuration flag that enables body logging.

## Egress profiles

The set of profiles is closed. A task policy names one; a lease carries one as its ceiling.

| Profile | Reachable | Used by |
|---|---|---|
| `none` | nothing (loopback only, no proxy socket) | `rust.check`, `rust.test.unit`, `workspace.read`, `repo.search` |
| `model-api` | configured model API host(s), TLS terminated, auth injected host-side | workspace environment ([D11](decisions.md#d11-workspace-environment-model-api-egress-goes-through-the-clyde-proxy)) |
| `rust-registry` | `crates.io`, `static.crates.io`, `index.crates.io`, plus configured mirrors | `rust.resolve-deps` |
| `broker` | not a sandbox profile — the broker's own egress, outside any sandbox | `git.push` |
| `custom` | explicit host allowlist from an approval | escalations only, never a default |

**Notes**
- `none` is the default for every task type. A profile other than `none` must be named explicitly in the task policy.
- `rust-registry` host lists come from configuration, and `.clyde/policy.toml` may only narrow them ([D14](decisions.md#d14-machine-readable-policy-in-clyde-agentsmd-advisory-only)). A repository cannot add a registry host.
- `custom` requires a human approval whose `request_digest` covers the exact host list, so approving one destination does not approve another later.

## Profile ordering

Lease derivation and escalation checks need a partial order on profiles ([lease derivation rules](schema-reference.md#derivation-rules-normative)):

```text
none  <  model-api  <  custom(H)         for any H
none  <  rust-registry
rust-registry  <  custom(H)             iff registry hosts ⊆ H
```

`model-api` and `rust-registry` are **incomparable**: neither is a superset of the other. A lease holding one cannot derive a child holding the other; that requires an escalation evaluated against the mission, not a derivation.

This ordering is a pure function and is unit-tested alongside the derivation rules.

## Backend realisation

### Bubblewrap (Phase 2a)

```text
bwrap --unshare-net --unshare-pid --unshare-ipc --unshare-uts --unshare-cgroup \
      --ro-bind <runtime-root-closure> ... \
      --ro-bind <snapshot> /work \
      --bind <mission-cache> /cache \
      --bind <proxy-socket> /run/clyde/egress.sock \   # omitted for profile `none`
      --tmpfs /tmp --proc /proc --dev /dev \
      --clearenv --new-session --die-with-parent ...
```

The forwarder is exec'd first and the task process is its child, so the forwarder dies with the sandbox.

For profile `none`, the socket is simply not bound. There is no configuration flag to disable egress — the absence of the socket *is* the absence of egress, which is the property worth having.

### Firecracker (Phase 2b)

The guest has no network device at all. The forwarder inside the guest bridges `127.0.0.1:<port>` to a **vsock** connection to the host, where clyded's proxy accepts it. The sandbox-visible contract is identical — one loopback proxy endpoint — so task images, cargo configuration, and policy are unchanged across backends.

No tap device, no bridge, no host firewall rules, and therefore no root requirement for networking in either backend.

## What this does not guarantee

Stated plainly here so the approval UX can state it too:

- **Destination scoping, not content control.** An allowlisted host can be sent arbitrary bytes. `model-api` in particular is an exfiltration channel available to a misbehaving agent. What the model provides is that it is the *only* channel, that it is narrow, that every connection is recorded, and that the credential itself cannot be stolen and reused elsewhere.
- **Termination is a trusted-surface increase, not a security feature.** Terminating `model-api` exists to keep a credential out of the sandbox. It does not filter content, and it makes the proxy a component that handles model plaintext. Keeping the carve-out to one profile is what bounds that cost.
- **No protection against a malicious allowlisted host.** If a permitted registry serves hostile content, egress control does not help; that is what the snapshot, isolation, and dependency-policy layers are for.
- **DNS is resolved host-side.** The sandbox cannot make DNS queries at all, which is a benefit, but it also means the proxy's view of a hostname is authoritative for the allowlist decision. Allowlist entries are hostnames, and a compromised resolver on the host is out of scope ([non-goals](problem-statement-threat-model.md#non-goals)).
- **Byte budgets are coarse.** They bound bulk exfiltration and runaway downloads; they do not detect low-volume signalling.

## Audit output

Every task run with a profile other than `none` produces:

- `EgressAttempt` records: timestamp, target host and port, decision, bytes in/out, and the reason for any refusal. For terminated `model-api` connections the request path and status may also be recorded; bodies never are
- a `FetchManifest` artifact: the aggregate view, referenced from the task result and shown in mission review

A refused attempt is a first-class signal, not a log line. `rust.check` should never attempt egress; if it does, that is either a misconfiguration or a hostile dependency probing for a way out, and it must surface in mission review either way.
