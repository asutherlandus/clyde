# Implementation Plan: Agent Browser Integration

**Branch**: `005-agent-browser` | **Date**: 2026-03-25 | **Spec**: [spec.md](spec.md)
**Input**: Feature specification from `/specs/005-agent-browser/spec.md`

## Summary

Integrate agent-browser (a Rust-based headless browser CLI) into Clyde's Docker container to enable the AI agent to autonomously browse and test web applications. Chrome for Testing is baked into the single Docker image at build time. A `--browser` flag on the `clyde` launch script controls runtime activation, adjusting resource limits (16GB/8CPU) and passing environment configuration. The agent interacts with the browser via CLI commands using a shipped skill definition. Concurrent sessions (up to 4) are supported via agent-browser's `--session` flag for agent team workflows.

## Technical Context

**Language/Version**: Bash 5.x (launch script, entrypoint), Dockerfile
**Primary Dependencies**: agent-browser (npm, ships prebuilt Rust binary), Chrome for Testing (via `agent-browser install --with-deps`)
**Storage**: Named Docker volume `clyde-browser-cache` for `~/.cache/ms-playwright/` persistence
**Testing**: bats (Bash unit/integration tests)
**Target Platform**: Linux (Ubuntu 24.04 container on Docker 24+)
**Project Type**: Single (CLI tool + Docker container)
**Performance Goals**: Warm-cache startup overhead <5s; basic browser workflow <30s
**Constraints**: Max 4 concurrent sessions; 16GB RAM / 8 CPU defaults with browser; no `SYS_ADMIN` capability
**Scale/Scope**: Single container, 1-4 concurrent browser sessions

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Notes |
|-----------|--------|-------|
| I. Container Isolation First | PASS | No additional capabilities granted. Browser sandbox disabled, relying on Docker isolation. No new host filesystem access — only a new named volume. |
| II. Reproducible Environments | PASS | agent-browser installed via npm with pinned version in Dockerfile. Chrome for Testing downloaded at build time via `agent-browser install`. Base image remains pinned Ubuntu 24.04. |
| III. Shell Script Safety | PASS | All new/modified scripts will use `set -euo pipefail`, quote variables, use `local` in functions, pass shellcheck. |
| IV. Claude Code Integration | PASS | Browser tool exposed to agent via skill definition. Resource limits configurable via existing `--memory`/`--cpus` flags. Supports both interactive and headless modes. |
| V. Fail-Safe Defaults | PASS | Browser disabled by default. `--browser` flag is explicit opt-in. Clear error when browser not enabled but agent tries to use it. Startup validates browser availability. |
| Container Architecture: Base Image | PASS | Ubuntu 24.04 (not minimal) already in use — documented in previous feature's Complexity Tracking. Chrome system deps added via apt with pinned versions. |
| Container Architecture: Layer Optimization | PASS | Chrome installation in a dedicated layer after system packages, before entrypoint (changes infrequently). |
| Container Architecture: Secret Management | PASS | No secrets involved in browser feature. |
| Shell Scripting Standards | PASS | All scripts will include shebang, strict mode, snake_case functions, usage() documentation, shellcheck compliance. |

## Project Structure

### Documentation (this feature)

```text
specs/005-agent-browser/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
└── tasks.md             # Phase 2 output (created by /speckit.tasks)
```

### Source Code (repository root)

```text
docker/
├── Dockerfile           # MODIFIED: Add agent-browser + Chrome for Testing layer
├── entrypoint.sh        # MODIFIED: Export CLYDE_BROWSER env var
├── nix/
│   └── user-init.sh     # MODIFIED: Conditional browser setup when CLYDE_BROWSER=1
├── browser/
│   ├── setup-browser.sh # NEW: Browser initialization (install validation, config, session management)
│   ├── agent-browser.json # NEW: Default agent-browser config (ignoreHttpsErrors, no-sandbox)
│   └── agent-browser-disabled.sh # NEW: Stub script shown when --browser not set
└── skills/
    └── agent-browser/
        └── SKILL.md     # NEW: Shipped skill definition for Claude Code agent

bin/
└── clyde                # MODIFIED: Add --browser flag, resource limit override, volume mount

tests/
├── unit/
│   └── clyde.bats       # MODIFIED: Add tests for --browser flag parsing
└── integration/
    └── browser.bats     # NEW: Browser integration tests
```

**Structure Decision**: This feature extends the existing single-project structure. No new top-level directories — browser support is a Docker container capability, so all additions are under `docker/` (container internals) and `bin/` (launch script). A new `docker/browser/` directory groups browser-specific configuration and scripts. A new `docker/skills/` directory holds skill definitions shipped with the container.

## Complexity Tracking

No constitution violations to justify.
