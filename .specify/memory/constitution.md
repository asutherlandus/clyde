<!--
Sync Impact Report
==================
Version change: 1.0.0 → 1.1.0
Modified principles: Container Architecture - Base Image
Changes:
  - Base Image requirement changed from "Use" to "Prefer" minimal images
  - Added formal exception mechanism for larger base images when compatibility requires
  - Exceptions must document: compatibility reason, rejected alternatives, compensating controls
Rationale: Claude Code npm packages may require glibc; Alpine musl can cause runtime issues
Templates requiring updates: None
Follow-up TODOs: None
-->

# Clyde Constitution

## Core Principles

### I. Container Isolation First

All functionality MUST run within Docker containers with explicit, minimal privilege grants.
Containers MUST NOT run as root unless absolutely required, and such exceptions MUST be
documented with security rationale. Host filesystem access MUST be limited to explicitly
mounted volumes. Network access MUST be restricted to declared ports and services.

**Rationale**: Claude Code operates on user codebases and executes arbitrary commands.
Defense-in-depth via container isolation prevents accidental or malicious damage to
host systems.

### II. Reproducible Environments

Docker images MUST be built from pinned base images (digest or specific tag, never `latest`).
All installed packages MUST specify exact versions. Build processes MUST be deterministic—
the same Dockerfile and context MUST produce functionally identical images. Multi-stage
builds SHOULD be used to minimize final image size and attack surface.

**Rationale**: Reproducibility ensures consistent behavior across development, testing,
and production. Version pinning prevents silent breakage from upstream changes and
enables reliable rollbacks.

### III. Shell Script Safety

All shell scripts MUST begin with `set -euo pipefail` (or equivalent for non-bash shells).
Scripts MUST quote all variable expansions (`"$var"` not `$var`). Scripts MUST NOT use
`eval` or similar dynamic execution unless security-reviewed and documented. Error messages
MUST go to stderr; output data MUST go to stdout. Exit codes MUST be meaningful and
documented.

**Rationale**: Shell scripts are error-prone by default. Strict mode and quoting prevent
common bugs like word splitting, unset variable errors, and silent failures that could
compromise container integrity or Claude Code operation.

### IV. Claude Code Integration

The container MUST provide Claude Code with appropriate filesystem access to the user's
project via volume mounts. Environment variables for API keys and configuration MUST be
passed securely (not baked into images). The container MUST support both interactive
terminal sessions and non-interactive (headless) execution modes. Resource limits
(memory, CPU) SHOULD be configurable to prevent runaway processes.

**Rationale**: Claude Code requires controlled access to user projects while maintaining
security boundaries. Supporting multiple execution modes enables CI/CD integration and
local development workflows.

### V. Fail-Safe Defaults

Default configurations MUST be secure and functional without requiring user modification.
Dangerous operations (host network mode, privileged containers, sensitive mounts) MUST
require explicit opt-in flags. Startup scripts MUST validate required environment
variables and fail with clear error messages before proceeding. Health checks MUST
be implemented to detect and report container failures.

**Rationale**: Users should not need to understand container internals to use Clyde
safely. Secure defaults with explicit opt-in for advanced features prevent
misconfiguration vulnerabilities.

## Container Architecture

Container design MUST follow these structural requirements:

- **Base Image**: Prefer official, minimal base images (Alpine, Distroless, or slim variants)
  to reduce attack surface. When compatibility requirements necessitate a larger base image
  (e.g., glibc dependencies, native module support), the deviation MUST be documented in
  plan.md Complexity Tracking with: (1) specific compatibility reason, (2) rejected
  alternatives, and (3) compensating controls. Document security scanning results for
  chosen base regardless of size.
- **Layer Optimization**: Order Dockerfile instructions from least to most frequently
  changing to maximize build cache efficiency.
- **Secret Management**: Never store secrets in images or environment variables visible
  in `docker inspect`. Use Docker secrets, mounted files, or runtime injection.
- **Logging**: All container logs MUST go to stdout/stderr for Docker log driver
  compatibility. Structured JSON logging SHOULD be used for production deployments.
- **Signal Handling**: Entrypoint scripts MUST properly handle SIGTERM for graceful
  shutdown. Use `exec` to replace shell with final process where appropriate.

## Shell Scripting Standards

All shell scripts in this project MUST adhere to these standards:

- **Shebang**: Use `#!/usr/bin/env bash` for portability (or explicit path if
  specific shell required).
- **Strict Mode**: Every script MUST include `set -euo pipefail` immediately after
  shebang.
- **Functions**: Complex scripts MUST use functions with `local` variables. Function
  names MUST use snake_case.
- **Argument Parsing**: Use `getopts` or explicit argument handling. Document all
  flags in a `usage()` function.
- **Temporary Files**: Use `mktemp` for temporary files. Set up cleanup traps with
  `trap cleanup EXIT`.
- **Shellcheck**: All scripts MUST pass `shellcheck` with no warnings. Disable
  specific checks only with inline comments explaining why.

## Governance

This constitution supersedes all other development practices for the Clyde project.
All pull requests and code reviews MUST verify compliance with these principles.

### Amendment Process

1. Proposed amendments MUST be documented with rationale and impact analysis.
2. Changes to Core Principles require explicit team consensus.
3. All amendments MUST update the version number per semantic versioning:
   - MAJOR: Backward-incompatible principle changes or removals
   - MINOR: New principles or significant expansions
   - PATCH: Clarifications and non-semantic updates
4. The Sync Impact Report at the top of this file MUST be updated with each amendment.

### Compliance Review

- All new Dockerfiles MUST be reviewed against Container Architecture requirements.
- All new shell scripts MUST pass `shellcheck` before merge.
- Security-sensitive changes MUST document their compliance with Principle I (Isolation)
  and Principle V (Fail-Safe Defaults).

**Version**: 1.1.0 | **Ratified**: 2026-01-24 | **Last Amended**: 2026-01-24
