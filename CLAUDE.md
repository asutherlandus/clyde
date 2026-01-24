# Clyde Development Guidelines

Auto-generated from feature plans. Last updated: 2026-01-24

## Project Overview

Clyde is a Docker-based isolated environment for running Claude Code. It provides security isolation while maintaining full functionality through careful volume mounting and privilege management.

## Active Technologies

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
├── Dockerfile           # Container image definition
├── entrypoint.sh        # UID/GID handling entrypoint
└── .dockerignore        # Build context exclusions

bin/
└── clyde                # Main launch script

tests/
├── unit/
│   └── clyde.bats       # Bash unit tests
└── integration/
    └── container.bats   # Container integration tests

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
2. **Display Forwarding**: Auto-detect and forward X11/Wayland for OAuth browser flow
3. **Resource Limits**: Default 8GB RAM, 4 CPUs; configurable via flags
4. **Auto-build**: Script builds image on first run if missing

## Security Considerations

### X11/Wayland Display Forwarding

Display forwarding is enabled by default to support OAuth browser flows. However, this grants the container access to the X11 server, which has security implications:

- **Keylogging risk**: X11 clients can read keyboard events from other windows
- **Screen capture**: Container could potentially capture screen content
- **Input injection**: Container could simulate input to other applications

**Mitigations:**
- Use `--no-display` flag when browser OAuth is not needed
- Use `CLYDE_NO_DISPLAY=1` environment variable for persistent configuration
- Consider using Wayland with proper isolation (more secure than X11)
- A warning is displayed when display forwarding is active

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

- 001-docker-claude: Initial implementation of Docker container for Claude Code

<!-- MANUAL ADDITIONS START -->
<!-- Add project-specific notes below this line -->
<!-- MANUAL ADDITIONS END -->
