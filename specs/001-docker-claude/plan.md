# Implementation Plan: Docker Container for Claude Code

**Branch**: `001-docker-claude` | **Date**: 2026-01-24 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `/specs/001-docker-claude/spec.md`

## Summary

Build a Docker-based isolated environment for running Claude Code with full TUI support. The system consists of a `clyde` shell script that launches a pre-configured Ubuntu 24.04 container with Claude Code installed, mounting the user's current directory and credentials for seamless operation. The container provides security isolation while maintaining productivity through skip-permissions mode, dynamic UID/GID matching, and OAuth credential sharing.

## Technical Context

**Language/Version**: Bash 5.x (launch script), Dockerfile (container definition)
**Primary Dependencies**: Docker Engine 24+, Node.js 20 LTS, Claude Code CLI (@anthropic-ai/claude-code)
**Storage**: N/A (stateless container, host mounts for persistence)
**Testing**: shellcheck (static analysis), bats-core (bash testing), manual integration testing
**Target Platform**: Linux (primary), macOS (secondary via Docker Desktop)
**Project Type**: Single project (CLI tool + Docker image)
**Performance Goals**: Container launch <10 seconds (after initial build), image build <5 minutes
**Constraints**: 8GB RAM default limit, 4 CPU default limit, host network mode required
**Scale/Scope**: Single-user local development tool

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

### Principle I: Container Isolation First

| Requirement | Status | Evidence |
|-------------|--------|----------|
| Non-root container user | ✅ PASS | FR-009: Dynamic UID/GID matching |
| Explicit volume mounts only | ✅ PASS | FR-002, FR-003, FR-003a: Only PWD, ~/.claude, ~/.gitconfig, ~/.ssh mounted |
| Network restricted to declared ports | ⚠️ JUSTIFIED | FR-009a: Full host network required (see Complexity Tracking) |

### Principle II: Reproducible Environments

| Requirement | Status | Evidence |
|-------------|--------|----------|
| Pinned base image | ✅ PASS | FR-006: ubuntu:24.04 (specific tag, not latest) |
| Exact package versions | ✅ PASS | Will pin Node.js 20.x LTS, npm packages in Dockerfile |
| Deterministic builds | ✅ PASS | Single Dockerfile, no external state dependencies |

### Principle III: Shell Script Safety

| Requirement | Status | Evidence |
|-------------|--------|----------|
| set -euo pipefail | ✅ PLANNED | clyde script header |
| Quoted variables | ✅ PLANNED | All variable expansions quoted |
| No eval | ✅ PLANNED | No dynamic execution |
| stderr for errors | ✅ PLANNED | Error handling to stderr |
| shellcheck clean | ✅ PLANNED | CI gate requirement |

### Principle IV: Claude Code Integration

| Requirement | Status | Evidence |
|-------------|--------|----------|
| Project volume mount | ✅ PASS | FR-002: PWD mounted at identical path |
| Secure credential passing | ✅ PASS | FR-003: ~/.claude mounted (not baked in) |
| Interactive mode support | ✅ PASS | FR-005: TTY allocation |
| Resource limits configurable | ✅ PASS | FR-013: 8GB/4CPU defaults with override flags |

### Principle V: Fail-Safe Defaults

| Requirement | Status | Evidence |
|-------------|--------|----------|
| Secure defaults without config | ✅ PASS | FR-009: Non-root, FR-013: Resource limits |
| Explicit opt-in for dangerous ops | ✅ PASS | Skip-permissions is container-internal only |
| Startup validation | ✅ PASS | FR-012: Docker availability check |
| Clear error messages | ✅ PASS | Edge cases specify error messaging |

**Gate Result**: ✅ PASS (1 justified violation documented below)

## Project Structure

### Documentation (this feature)

```text
specs/001-docker-claude/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output (minimal - CLI tool)
├── quickstart.md        # Phase 1 output
└── contracts/           # Phase 1 output (CLI interface spec)
```

### Source Code (repository root)

```text
docker/
├── Dockerfile           # Container image definition
├── entrypoint.sh        # Container entrypoint with UID/GID handling
└── .dockerignore        # Build context exclusions

bin/
└── clyde                # Main launch script (installed to PATH)

tests/
├── unit/
│   └── clyde.bats       # Bash unit tests for clyde script
└── integration/
    └── container.bats   # Container integration tests
```

**Structure Decision**: Minimal CLI tool structure. The `docker/` directory contains all container-related files. The `bin/` directory contains the user-facing script. Tests use bats-core for bash testing.

## Complexity Tracking

| Violation | Why Needed | Simpler Alternative Rejected Because | Compensating Controls |
|-----------|------------|-------------------------------------|----------------------|
| Full host network (Principle I) | Claude Code must access arbitrary APIs, git remotes, package registries dynamically | Bridge network with explicit port mapping impossible - endpoints not known in advance; allowlist approach would require constant maintenance and break on new services | Container isolation, non-root user, resource limits |
| Full ubuntu:24.04 base image (Container Architecture) | Claude Code npm packages require glibc; native modules may fail on musl | Alpine Linux: musl libc compatibility issues with npm native modules; Ubuntu minimal: missing utilities Claude Code may invoke; Distroless: no shell, incompatible with entrypoint pattern | Non-root user (FR-009), resource limits (FR-013), explicit volume mounts only, security scanning required before production |

## Deferred Items

| Item | Constitution Reference | Why Deferred | Trigger for Implementation |
|------|----------------------|--------------|---------------------------|
| Container health checks | Principle V: "Health checks MUST be implemented" | Interactive short-lived container - user is attached to terminal and immediately aware of failures. Health checks designed for long-running services and orchestration systems. | If Clyde adds daemon/background mode or orchestration support |
