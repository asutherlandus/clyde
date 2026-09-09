# CLAUDE.md

Project guidance for humans and coding agents lives in [AGENTS.md](AGENTS.md). Read it before making changes — it carries the coding standards, error-handling rules, security requirements, and the rule that the Nix flake is the source of truth for tooling.

## Load these first

1. [AGENTS.md](AGENTS.md) — how to work in this repository
2. [docs/builder/decisions.md](docs/builder/decisions.md) — the binding implementation decisions, the refinements (`R*`), and the open questions (`OQ*`). Read [D23](docs/builder/decisions.md#d23-the-mvp-splits-into-the-builder-and-the-warden) first. The four warden decisions (`D1`, `D11`, `D17`, `D20`) are in [docs/warden/decisions.md](docs/warden/decisions.md)
3. [docs/builder/roadmap.md](docs/builder/roadmap.md#the-two-products) — the builder / warden map
4. The spec for the phase being worked on — start at [Phase 0](docs/builder/roadmap.md#phase-0-foundations)

[AGENTS.md](AGENTS.md#design-documents-and-decisions) has the full reading order.

## Current state

The MVP slice (Phases 0-4) is implemented, and `rust.check` has now run once inside a real isolation boundary — see [What has and has not been exercised](README.md#what-has-and-has-not-been-exercised). It predates the builder / warden split ([D23](docs/builder/decisions.md#d23-the-mvp-splits-into-the-builder-and-the-warden)): the docs have been reorganised around the split, the code has not.

Built since the split: the operator task surface (`D25`), so a human runs tasks on the admin socket and every request records the `Principal` that made it; posture reporting (`D26`), derived at startup and carried on every task run; and the materialisation mtime contract (`D27`).

Built since then: the microVM guest side (`D24`) — a flake-built kernel and erofs root images (`R12`), the `clyde-guest-api` job contract, `clyde-init`, block-image construction with `mke2fs -d`, and the vsock guest channel carrying all guest output (`R11`). It is opt-in per run: an operator raises a task with `clyde task run … --isolation microvm`, and `min_isolation` remains a floor that can only be raised.

Decided and unbuilt: the vsock egress bridge (so `rust.resolve-deps` stays refused), guest-side `fanotify` learn mode, the raised policy floor that would make the microVM the default, and the deferred dependency-cache question (`D28`), which waits on Part 1b's measurements.

One thing the test suite cannot tell you: **no test has booted a guest, and no test spawns a sandbox on either backend**. Every isolation claim is asserted over generated specifications rather than against a kernel — the guest channel is covered end to end against a fake guest, and images are built by real `mke2fs`, but Firecracker and the init under a real kernel are not. The first real bubblewrap run found four bugs the suite could not see (see the [changelog](CHANGELOG.md)); expect the same here. [The bring-up guide](docs/builder/microvm-bring-up.md) says what to read when a guest does not boot.

## Two rules from the split

- No crate under `crates/` may have a notion of an agent process. The split is a daemon-layer boundary.
- Do not describe the builder as though the warden were present. The builder cannot stop a driver executing project code outside Clyde, and says so as `advisory` posture.

## Conventions

- Cite decision identifiers (`D7`, `D18`) rather than restating their rationale.
- Do not re-litigate a settled decision. If one looks wrong, say so; do not quietly implement something else.
- Stop and ask at an open question (`OQ*`). Each records a proposed resolution, which is a starting point for discussion, not a default.
- Update the affected document alongside the code. A decision reversed in code but not in `decisions.md` is worse than no decision log.
