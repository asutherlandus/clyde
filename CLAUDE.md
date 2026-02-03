# Clyde Development Guidelines

Auto-generated from feature plans. Last updated: 2026-01-30

## Project Overview

Clyde is a Docker-based isolated environment for running Claude Code. It provides security isolation while maintaining full functionality through careful volume mounting and privilege management.

## Active Technologies
- Bash 5.x (launch script, entrypoint), Nix (package management) + Nix 2.18+ (single-user mode), Docker 24+ (003-nix-dependencies)
- Named Docker volume `clyde-nix-store` for /nix persistence (003-nix-dependencies)
- Bash 5.x (clyde script), Ubuntu 24.04 (container) + Docker 24+, X11 (optional, for `--x11`) (004-container-debug)
- N/A (no persistent state changes) (004-container-debug)

| Component | Technology | Version |
|-----------|------------|---------|
| Launch Script | Bash | 5.x |
| Container | Docker | 24+ |
| Runtime | Node.js | 20 LTS |
| CLI Tool | Claude Code | @anthropic-ai/claude-code |
| Base Image | Ubuntu | 24.04 |

## Project Structure

```text
docker/
├── Dockerfile           # Container image definition (Ubuntu minimal + Nix)
├── Dockerfile.old       # Pre-Nix backup of Dockerfile
├── entrypoint.sh        # UID/GID handling + Nix environment activation
├── nix/
│   ├── flake.nix        # Default packages (git, gh, node, curl, ssh)
│   ├── flake.lock       # Pinned package versions (auto-generated)
│   └── clyde-packages   # Helper script to list available packages
└── open-url.sh          # OAuth URL handler

bin/
└── clyde                # Main launch script

tests/
├── unit/
│   └── clyde.bats       # Bash unit tests
└── integration/
    ├── container.bats   # Container integration tests
    └── nix-configs/     # Test fixtures for Nix configurations

specs/                   # Feature specifications
```

## Constitution Compliance

All code MUST comply with `.specify/memory/constitution.md`:

### Shell Scripts
- Start with `#!/usr/bin/env bash` and `set -euo pipefail`
- Quote all variable expansions (`"$var"`)
- Errors to stderr, output to stdout
- Must pass `shellcheck` with no warnings

### Dockerfile
- Pin base image version (never use `latest`)
- Pin all package versions
- Order layers for cache efficiency
- Use `tini` for init, `gosu` for privilege drop

### Container Security
- Non-root user with dynamic UID/GID matching
- Only mount explicitly declared volumes
- Document any security exceptions

## Commands

```bash
# Build Docker image
docker build -t clyde:local docker/

# Run shellcheck on scripts
shellcheck bin/clyde docker/entrypoint.sh

# Run tests (requires bats-core)
bats tests/

# Launch clyde
./bin/clyde
```

## Code Style

### Bash
- Functions use `snake_case`
- Use `local` for function variables
- Use `getopts` for argument parsing
- Provide `usage()` function for help

### Dockerfile
- One `RUN` command per logical operation
- Use `&&` to chain commands in same layer
- Clean apt cache after installs

## Key Implementation Notes

1. **UID/GID Matching**: Container creates user at runtime matching host user's UID/GID
2. **SSH Agent Forwarding**: Forward SSH agent socket (not keys) for git operations
3. **Resource Limits**: Default 8GB RAM, 4 CPUs; configurable via flags
4. **Auto-build**: Script builds image on first run if missing
5. **Nix Package Management**: Project dependencies managed via Nix flakes for reproducibility
6. **Claude Code via npm**: Always-latest Claude Code installed via npm at runtime
7. **Persistent Volumes**: `clyde-nix-store` for Nix cache, `clyde-npm-cache` for Claude Code

## Security Considerations

### Token Storage

OAuth tokens are stored in `~/.claude/profiles/` with 600 permissions. For additional security:
- Store tokens on encrypted filesystems
- Use separate profiles for different accounts/projects
- Tokens are passed to containers via mounted secret files (not environment variables)

### Profile Names

Profile names are validated to prevent path traversal attacks. Only alphanumeric characters, dashes, and underscores are allowed.

### Dependencies

- `jq` is required for profile management (no fallback to insecure grep/sed parsing)
- Dockerfile pins all package versions and base image digest for reproducibility

## Recent Changes
- 004-container-debug: Added Bash 5.x (clyde script), Ubuntu 24.04 (container) + Docker 24+, X11 (optional, for `--x11`)
- 003-nix-dependencies: Complete implementation of Nix-based dependency management
  - Hybrid approach: Nix for project dependencies, npm for always-latest Claude Code
  - Named Docker volumes for persistence (clyde-nix-store, clyde-npm-cache)
  - Project and user Nix configuration support (flake.nix, shell.nix)
  - New flags: --nix-verbose, --nix-gc, --list-packages

- 001-docker-claude: Initial implementation of Docker container for Claude Code

<!-- MANUAL ADDITIONS START -->
<!-- Add project-specific notes below this line -->
<!-- MANUAL ADDITIONS END -->
