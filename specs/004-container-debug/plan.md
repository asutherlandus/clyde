# Implementation Plan: Container Debugging Options

**Branch**: `004-container-debug` | **Date**: 2026-02-03 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `/specs/004-container-debug/spec.md`

## Summary

Add three new command-line options to the clyde launcher script to enable developer debugging workflows:
1. `--shell` - Launch container with interactive bash shell instead of Claude Code
2. `--x11` - Enable X11 forwarding from container to host display
3. `--exec <cmd>` - Execute a single command in the container and exit

These options allow developers to debug applications in the exact same environment Claude uses.

## Technical Context

**Language/Version**: Bash 5.x (clyde script), Ubuntu 24.04 (container)
**Primary Dependencies**: Docker 24+, X11 (optional, for `--x11`)
**Storage**: N/A (no persistent state changes)
**Testing**: bats-core (existing test framework)
**Target Platform**: Linux hosts with X11 (X11 forwarding); Linux/macOS for shell mode
**Project Type**: Single project (CLI tool + Docker container)
**Performance Goals**: Container startup < 5 seconds (existing requirement preserved)
**Constraints**: Must maintain environment parity with normal Claude mode
**Scale/Scope**: Single-user local development tool

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Notes |
|-----------|--------|-------|
| I. Container Isolation First | PASS | X11 socket mount is explicit opt-in; documented security implications |
| II. Reproducible Environments | PASS | No changes to image build; runtime-only options |
| III. Shell Script Safety | PASS | Will use `set -euo pipefail`, quoted variables, shellcheck |
| IV. Claude Code Integration | PASS | Shell/exec modes provide same environment as Claude mode |
| V. Fail-Safe Defaults | PASS | X11 requires explicit `--x11` flag; validates DISPLAY |

**Shell Scripting Standards Checklist:**
- [x] Shebang: `#!/usr/bin/env bash` (existing)
- [x] Strict mode: `set -euo pipefail` (existing)
- [x] Functions: snake_case with `local` variables (existing pattern)
- [x] Argument parsing: getopts-style handling (existing pattern)
- [x] Shellcheck: All changes must pass with no warnings

## Project Structure

### Documentation (this feature)

```text
specs/004-container-debug/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # N/A (no data entities)
├── quickstart.md        # Phase 1 output
└── tasks.md             # Phase 2 output (via /speckit.tasks)
```

### Source Code (repository root)

```text
bin/
└── clyde                # Main launch script (MODIFY)

docker/
├── Dockerfile           # Container image (NO CHANGES)
├── entrypoint.sh        # User setup (NO CHANGES)
└── nix/
    └── user-init.sh     # Nix environment activation (NO CHANGES)

tests/
├── unit/
│   └── clyde.bats       # Unit tests (MODIFY - add new option tests)
└── integration/
    └── container.bats   # Integration tests (MODIFY - add shell/X11 tests)
```

**Structure Decision**: Existing single-project CLI structure. Modifications to `bin/clyde` for argument parsing and `docker/nix/user-init.sh` for command dispatch.

## Complexity Tracking

No constitution violations requiring justification.

| Item | Rationale |
|------|-----------|
| X11 socket mount | Explicit opt-in via `--x11`; security documented in help text per spec clarification |
