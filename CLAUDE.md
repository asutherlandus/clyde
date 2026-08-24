# CLAUDE.md

Project guidance for humans and coding agents lives in [AGENTS.md](AGENTS.md). Read it before making changes — it carries the coding standards, error-handling rules, security requirements, and the rule that the Nix flake is the source of truth for tooling.

## Load these first

1. [AGENTS.md](AGENTS.md) — how to work in this repository
2. [docs/decisions.md](docs/decisions.md) — the binding implementation decisions (`D1`–`D20`) and the open questions (`OQ*`)
3. The spec for the phase being worked on — start at [docs/phase-0-foundations.md](docs/phase-0-foundations.md)

[AGENTS.md](AGENTS.md#design-documents-and-decisions) has the full reading order.

## Current state

Design and implementation-planning documents only. No implementation code exists, and there is no flake yet — creating it is the first Phase 0 deliverable.

## Conventions

- Cite decision identifiers (`D7`, `D18`) rather than restating their rationale.
- Do not re-litigate a settled decision. If one looks wrong, say so; do not quietly implement something else.
- Stop and ask at an open question (`OQ*`). Each records a proposed resolution, which is a starting point for discussion, not a default.
- Update the affected document alongside the code. A decision reversed in code but not in `decisions.md` is worse than no decision log.
