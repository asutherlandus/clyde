# Implementation Plan: Nix-Based Dependency Management

**Branch**: `003-nix-dependencies` | **Date**: 2026-01-30 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `/specs/003-nix-dependencies/spec.md`

## Summary

Replace Clyde's monolithic Dockerfile package installation with a hybrid Nix + npm approach:
- **Project dependencies** (git, gh, Node.js, user packages): Managed by Nix with flake.lock pinning for reproducibility
- **Claude Code**: Installed via npm at runtime, always getting the latest version

The Docker image becomes minimal (Ubuntu + Nix + tini/gosu), while base tooling (Node.js, git, gh) comes from Nix and Claude Code comes from npm. Users can customize their project environment via flake.nix without rebuilding the image, while always having the latest Claude Code.

## Technical Context

**Language/Version**: Bash 5.x (launch script, entrypoint), Nix (package management), npm (Claude Code)
**Primary Dependencies**: Nix 2.18+ (single-user mode), Docker 24+, npm (bundled with Node.js)
**Storage**: Named Docker volumes: `clyde-nix-store` for /nix persistence, `clyde-npm-cache` for Claude Code
**Testing**: bats-core (existing), manual integration tests
**Target Platform**: Linux containers (x86_64, arm64)
**Project Type**: Single project (shell scripts + Dockerfile)
**Performance Goals**: <10s cached startup, <3min first-time setup
**Constraints**: No host Nix store sharing, single-user Nix only
**Scale/Scope**: Single-user local development tool

## Constitution Check

*GATE: ✓ Passed pre-research. ✓ Re-checked post-design.*

| Principle | Status | Notes |
|-----------|--------|-------|
| I. Container Isolation First | PASS | Non-root user preserved, /nix isolated in Docker volume |
| II. Reproducible Environments | PASS | Nix flake.lock provides reproducibility; base image still pinned |
| III. Shell Script Safety | PASS | All scripts use strict mode, shellcheck compliance required |
| IV. Claude Code Integration | PASS | Claude Code installed via npm at runtime (always latest), same functionality preserved |
| V. Fail-Safe Defaults | PASS | Default config works without user Nix knowledge |
| Base Image | DEVIATION | Ubuntu 24.04 retained for glibc compatibility (see Complexity Tracking) |
| Layer Optimization | N/A | Nix replaces layer-based caching with store-based caching |
| Secret Management | PASS | Token handling unchanged (mounted secret files) |
| Signal Handling | PASS | tini + gosu preserved |

## Project Structure

### Documentation (this feature)

```text
specs/003-nix-dependencies/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output (N/A - no API)
└── tasks.md             # Phase 2 output (/speckit.tasks)
```

### Source Code (repository root)

```text
docker/
├── Dockerfile           # Minimal: Ubuntu 24.04 minimal + Nix + tini/gosu
├── Dockerfile.old       # Backup of current monolithic Dockerfile
├── entrypoint.sh        # Extended: Nix environment activation
├── nix/
│   ├── flake.nix        # Default packages (git, gh, node - NOT claude-code)
│   └── flake.lock       # Pinned package versions
└── open-url.sh          # OAuth URL handler (unchanged)

bin/
└── clyde                # Extended: Nix config detection, store volume, new flags

tests/
├── unit/
│   └── clyde.bats       # Extended: Nix-related flag tests
└── integration/
    ├── container.bats   # Extended: Nix environment tests
    └── nix-configs/     # Test fixtures (sample flake.nix/shell.nix)
```

**Structure Decision**: Extends existing single-project structure. New `docker/nix/` directory contains Nix configurations. No new top-level directories needed.

## Complexity Tracking

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| Ubuntu 24.04 minimal (not Alpine) | glibc required for Nix binary cache and npm native modules | Alpine uses musl libc which causes runtime errors; minimal variant reduces size while keeping glibc |
| Named Docker volume for /nix | Nix store is large (2-10GB), must persist across container restarts | Rebuilding store on each start would take 3+ minutes, violating SC-001 |

## Implementation Phases

### Phase 0: Research ✓ COMPLETE

All research questions resolved. See [research.md](./research.md) for details.

| Question | Resolution |
|----------|------------|
| Nix in Docker | Single-user mode (`--no-daemon`), chown /nix to runtime user |
| Flake merging | Use `inputsFrom` in `mkShellNoCC` |
| claude-code | **Hybrid approach**: Install via npm at runtime for always-latest; Nix provides Node.js |
| Output filtering | `--quiet --log-format=bar` + sed/grep |

### Phase 1: Design ✓ COMPLETE

Generated artifacts:
- [research.md](./research.md) - Resolved research questions with code examples
- [data-model.md](./data-model.md) - Configuration files, environment variables, mount points
- [quickstart.md](./quickstart.md) - User guide for creating flake.nix

### Phase 2: Tasks (via /speckit.tasks)

Task breakdown for implementation - ready for generation.
