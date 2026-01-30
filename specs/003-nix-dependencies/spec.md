# Feature Specification: Nix-Based Dependency Management

**Feature Branch**: `003-nix-dependencies`
**Created**: 2026-01-30
**Status**: Draft
**Input**: User description: "Nix-based dependency management for Clyde Docker container. Replace monolithic Dockerfile package installation with Nix for declarative, reproducible, user-configurable package management."

## Clarifications

### Session 2026-01-30

- Q: When user's Nix config has errors, should the system block or proceed with defaults? → A: Prompt user interactively: "Config invalid. Proceed with defaults? [Y/n]"
- Q: Should Nix operations be persistently logged for troubleshooting? → A: No persistent logs; `--verbose` flag is sufficient for debugging
- Q: Should there be a progress indicator while Nix is fetching/building packages? → A: Show package names as they download: "Fetching git... Fetching nodejs..."
- Q: Which Ubuntu variant for the base image? → A: Ubuntu 24.04 minimal (~40MB vs ~78MB full) - keeps glibc, reduces size
- Q: How should Claude Code versioning work vs project dependency pinning? → A: **Hybrid approach**: Claude Code installed via npm at runtime (always latest), project dependencies managed via Nix flake.lock (pinned). This separates "tool freshness" from "project reproducibility".

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Zero-Config Default Experience (Priority: P1)

A developer runs `clyde` for the first time without any Nix configuration files. The container starts with a functional default environment including Claude Code, git, GitHub CLI, and Node.js - exactly like today's experience but powered by Nix under the hood.

**Why this priority**: This is the foundation. If the default experience breaks, no other features matter. Users who don't know or care about Nix should have an identical experience to before.

**Independent Test**: Can be fully tested by running `clyde` in a directory with no flake.nix/shell.nix and verifying claude, git, gh, and node commands are available.

**Acceptance Scenarios**:

1. **Given** a user with no Nix configuration files, **When** they run `clyde`, **Then** the container starts with claude-code, git, gh CLI, and Node.js available in PATH
2. **Given** a user running clyde for the first time, **When** the container starts, **Then** startup time is comparable to the current implementation (within 5 seconds for cached packages)
3. **Given** a user with no Nix knowledge, **When** they use clyde, **Then** they see friendly progress output (package names being fetched) but no verbose Nix internals unless something fails or `--verbose` is used

---

### User Story 2 - Project-Specific Dependencies (Priority: P2)

A developer working on a Rust project adds a `flake.nix` to their project root specifying rustc, cargo, and rust-analyzer. When they run `clyde` from that project directory, those tools are automatically available in addition to the defaults.

**Why this priority**: This is the core value proposition - letting projects declare their own dependencies without modifying the Docker image.

**Independent Test**: Can be tested by creating a minimal flake.nix with one package (e.g., ripgrep), running clyde, and verifying the package is available.

**Acceptance Scenarios**:

1. **Given** a project with a valid flake.nix, **When** the user runs `clyde` from that directory, **Then** all packages specified in the flake are available in the container
2. **Given** a project with a shell.nix (legacy format), **When** the user runs `clyde`, **Then** packages from shell.nix are available (flakes are preferred but shell.nix is supported)
3. **Given** a project flake.nix that specifies python312, **When** the user runs `clyde`, **Then** python3 --version shows Python 3.12.x
4. **Given** a project with an invalid flake.nix, **When** the user runs `clyde`, **Then** a clear error message explains the problem and prompts "Proceed with defaults? [Y/n]"

---

### User Story 3 - User-Global Default Packages (Priority: P3)

A developer who always wants Python and jq available creates `~/.config/clyde/flake.nix` on their host machine. Now every clyde session includes these tools, regardless of project.

**Why this priority**: Power users want to customize their baseline environment without modifying every project.

**Independent Test**: Can be tested by creating ~/.config/clyde/flake.nix with a unique package, running clyde in a project without its own config, and verifying the package is present.

**Acceptance Scenarios**:

1. **Given** a user with ~/.config/clyde/flake.nix, **When** they run `clyde` in a project without its own Nix config, **Then** packages from the user config are available
2. **Given** a user with ~/.config/clyde/flake.nix AND a project with flake.nix, **When** they run `clyde`, **Then** both user and project packages are available (project takes precedence on conflicts)
3. **Given** a user with ~/.config/clyde/shell.nix (legacy), **When** they run `clyde`, **Then** the shell.nix is used if no flake.nix exists

---

### User Story 4 - Persistent Nix Store (Priority: P2)

A developer runs clyde multiple times across different projects. Packages downloaded once are cached and reused, so subsequent sessions start quickly without re-downloading.

**Why this priority**: Without caching, every container start would download packages - making the feature unusable in practice.

**Independent Test**: Can be tested by running clyde twice with the same flake.nix and measuring that the second run starts significantly faster.

**Acceptance Scenarios**:

1. **Given** a user has previously run clyde with a flake requiring package X, **When** they run clyde again (same or different project) needing package X, **Then** package X is loaded from cache without network download
2. **Given** the Nix store volume does not exist, **When** user runs `clyde` for the first time, **Then** the volume is automatically created
3. **Given** a user wants to clear the cache, **When** they run `clyde --nix-gc`, **Then** unused packages are garbage collected from the store

---

### User Story 5 - Inspect Available Packages (Priority: P4)

A developer wants to see what packages are available in their current clyde environment. They can run a command to list all Nix-provided packages.

**Why this priority**: Nice to have for debugging and understanding the environment, but not essential for core functionality.

**Independent Test**: Can be tested by running the inspection command and verifying output lists expected packages.

**Acceptance Scenarios**:

1. **Given** a user in a clyde session, **When** they run `clyde-packages` (or similar), **Then** they see a list of all Nix-provided packages and their versions
2. **Given** a user outside the container, **When** they run `clyde --list-packages`, **Then** they see what packages would be available without starting a full session

---

### Edge Cases

- What happens when the user's flake.nix has syntax errors?
  - System shows Nix's error message and prompts: "Proceed with defaults? [Y/n]"
- What happens when a package specified in flake.nix doesn't exist?
  - System shows Nix's "package not found" error and prompts: "Proceed with defaults? [Y/n]"
- What happens when the Nix store volume runs out of space?
  - System shows a clear error message suggesting `clyde --nix-gc` to free space
- What happens when there's no network and packages aren't cached?
  - System shows a clear error explaining which packages couldn't be fetched
- What happens when project flake.nix conflicts with user flake.nix (same package, different versions)?
  - Project version takes precedence; no error unless Nix itself cannot resolve
- What happens when the user has Nix installed on the host and wants to share the store?
  - This is explicitly NOT supported in v1 to avoid corruption; store is isolated in Docker volume

## Requirements *(mandatory)*

### Functional Requirements

#### Docker Image

- **FR-001**: System MUST provide a minimal base Docker image containing only: Ubuntu 24.04 minimal variant, Nix package manager, tini (init), gosu (privilege dropping), and the default Nix configuration
- **FR-002**: System MUST NOT include application packages (Node.js, git, gh, etc.) directly in the Docker image; these MUST come from Nix
- **FR-003**: System MUST install Nix in single-user mode to avoid daemon complexity in containers
- **FR-004**: System MUST include a default Nix configuration that provides: git, gh (GitHub CLI), Node.js 20 LTS, curl, and openssh-client (NOTE: Claude Code is NOT included in Nix - see FR-004a)
- **FR-004a**: System MUST install Claude Code via npm at container startup (`npm install -g @anthropic-ai/claude-code`), ensuring users always have the latest version. The npm global directory MUST be cached in a persistent Docker volume (`clyde-npm-cache`) for fast subsequent launches.

#### Configuration Discovery

- **FR-005**: System MUST check for Nix configuration in this priority order: (1) project flake.nix, (2) project shell.nix, (3) user ~/.config/clyde/flake.nix, (4) user ~/.config/clyde/shell.nix, (5) container default
- **FR-006**: System MUST support Nix flakes as the primary configuration format
- **FR-007**: System MUST support traditional shell.nix for backwards compatibility with existing Nix users
- **FR-008**: System MUST merge packages from multiple configuration layers (user + project) when both exist

#### Nix Store Management

- **FR-009**: System MUST use a named Docker volume (`clyde-nix-store`) to persist the Nix store across container restarts
- **FR-009a**: System MUST use a named Docker volume (`clyde-npm-cache`) to persist the npm global directory for Claude Code installation
- **FR-010**: System MUST automatically create the Nix store volume on first run if it doesn't exist
- **FR-010a**: System MUST automatically create the npm cache volume on first run if it doesn't exist
- **FR-011**: System MUST provide a `--nix-gc` flag to garbage collect unused packages from the store
- **FR-012**: System MUST NOT mount the host's /nix directory to avoid conflicts with host Nix installations

#### Launch Script (bin/clyde)

- **FR-013**: System MUST detect the presence of flake.nix or shell.nix in the current directory
- **FR-014**: System MUST mount user's ~/.config/clyde/ directory (read-only) if it exists
- **FR-015**: System MUST mount the project's flake.nix, shell.nix, and flake.lock files (read-only) if they exist
- **FR-016**: System MUST pass environment variables to indicate which Nix configuration to use

#### Entrypoint Behavior

- **FR-017**: System MUST enter the appropriate Nix environment before launching Claude Code (`nix develop` for flakes, `nix-shell` for shell.nix)
- **FR-018**: System MUST prompt the user interactively when Nix configuration has errors: display the error, then ask "Proceed with defaults? [Y/n]". If user confirms, continue with default environment; if user declines, exit with non-zero status
- **FR-019**: System MUST preserve all existing entrypoint functionality (UID/GID matching, SSH agent forwarding, etc.)

#### User Experience

- **FR-020**: System MUST NOT require users to have any Nix knowledge to use the default experience
- **FR-021**: System MUST show package names as they are fetched/built (e.g., "Fetching git... Fetching nodejs...") to provide progress feedback during startup
- **FR-024**: System MUST suppress verbose Nix internals (build logs, hash calculations, store paths) during normal startup; these are shown only with `--verbose` flag
- **FR-022**: System MUST provide a `--verbose` or `--nix-verbose` flag to show Nix operations for debugging
- **FR-023**: System MUST NOT write persistent Nix operation logs; the `--verbose` flag is the sole mechanism for troubleshooting Nix-related issues

### Key Entities

- **Nix Configuration**: A flake.nix or shell.nix file that declares packages to be available in the environment. Can exist at project level or user level.
- **Nix Store**: A content-addressed store of all Nix packages, persisted in a Docker volume (`clyde-nix-store`). Shared across all clyde sessions.
- **npm Cache**: A persistent Docker volume (`clyde-npm-cache`) storing the global npm installation of Claude Code. Enables fast startup while ensuring latest version.
- **Configuration Layer**: One of: project, user, or default. Each layer can contribute packages to the final environment.
- **Default Environment**: The baseline set of packages (git, gh, node) always available even without user configuration, plus Claude Code installed via npm.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Users with no Nix configuration can start clyde and use Claude Code within 10 seconds (cached state)
- **SC-002**: First-time setup (downloading all default packages) completes within 3 minutes on a typical broadband connection
- **SC-003**: Adding a new package to a project flake.nix and restarting clyde makes that package available within 30 seconds (if cached) or 2 minutes (if download needed)
- **SC-004**: The base Docker image size is reduced by at least 50% compared to the current monolithic image (Ubuntu minimal + Nix only, no application packages)
- **SC-005**: 100% of existing clyde functionality (profiles, SSH forwarding, resource limits, etc.) continues to work unchanged
- **SC-006**: Users can declare project-specific dependencies without modifying any clyde-managed files
- **SC-007**: The Nix store volume can be completely deleted and rebuilt without losing any user configuration

## Assumptions

- Users have Docker installed and working (existing requirement)
- Network access is available for initial package downloads (subsequent runs work offline with cached packages)
- The host system has sufficient disk space for the Nix store (typically 2-10GB depending on packages used)
- Flakes are the preferred modern Nix configuration format; shell.nix support is for compatibility with existing users
- Single-user Nix installation is sufficient for container use (no need for multi-user daemon)
- Users wanting to customize packages have basic familiarity with Nix syntax or can follow provided examples

## Out of Scope

- Sharing Nix store with host system (risk of corruption, version mismatches)
- NixOS as the base image (adds complexity without clear benefit for this use case)
- Nix Darwin support (macOS-specific, clyde targets Linux containers)
- Automatic flake.nix generation from package.json/Cargo.toml/etc. (could be future enhancement)
- GUI package selector (command-line configuration is sufficient for target users)
