# Dockerfile Contract: Clyde Container Image

**Version**: 1.0.0
**Date**: 2026-01-24

## Image Specification

| Property | Value |
|----------|-------|
| Base Image | `ubuntu:24.04` |
| Image Name | `clyde:local` |
| Working Directory | `/workspace` |
| Entrypoint | `/usr/bin/tini -- /entrypoint.sh` |
| Default Command | `claude --dangerously-skip-permissions` |

## Build Arguments

| Argument | Default | Description |
|----------|---------|-------------|
| `NODE_VERSION` | `20` | Node.js major version |

## Environment Variables (Build-time)

| Variable | Value | Description |
|----------|-------|-------------|
| `DEBIAN_FRONTEND` | `noninteractive` | Suppress apt prompts |

## Environment Variables (Runtime)

| Variable | Required | Description |
|----------|----------|-------------|
| `HOST_UID` | Yes | UID for container user (passed by clyde script) |
| `HOST_GID` | Yes | GID for container user (passed by clyde script) |
| `DISPLAY` | No | X11 display for browser OAuth |
| `WAYLAND_DISPLAY` | No | Wayland display for browser OAuth |

## Installed Packages

### System Packages (apt)

| Package | Purpose |
|---------|---------|
| `curl` | HTTP client for downloads |
| `ca-certificates` | SSL certificate verification |
| `git` | Version control |
| `openssh-client` | SSH for git remotes |
| `tini` | Init process for signal handling |
| `gosu` | Privilege dropping |
| `xdg-utils` | Browser opening (xdg-open) |

### Node.js Packages (npm global)

| Package | Purpose |
|---------|---------|
| `@anthropic-ai/claude-code` | Claude Code CLI |

## Exposed Ports

None. Container uses host network mode.

## Volumes

No declared volumes. All mounts are bind mounts created at runtime by the clyde script.

## Health Check

None defined. Container is interactive and short-lived.

## Build Stages

Single stage build (no multi-stage). Rationale: Development tool where build time matters less than compatibility.

## Layer Order (for cache optimization)

1. Base image
2. System package installation
3. Node.js installation
4. npm global package installation
5. Entrypoint script copy
6. Entrypoint/CMD configuration

## Entrypoint Script Contract

The `/entrypoint.sh` script MUST:

1. Read `HOST_UID` and `HOST_GID` environment variables
2. Create group `claude` with GID matching `HOST_GID`
3. Create user `claude` with UID matching `HOST_UID`
4. Set up home directory at `/home/claude`
5. Use `exec gosu claude` to run the command as the created user
6. Pass all arguments (`$@`) to the final command

### Entrypoint Pseudocode

```bash
#!/usr/bin/env bash
set -euo pipefail

HOST_UID="${HOST_UID:-1000}"
HOST_GID="${HOST_GID:-1000}"

# Create group if it doesn't exist
groupadd -g "$HOST_GID" claude 2>/dev/null || true

# Create user if it doesn't exist
useradd -u "$HOST_UID" -g "$HOST_GID" -m -d /home/claude -s /bin/bash claude 2>/dev/null || true

# Ensure home directory ownership
chown -R "$HOST_UID:$HOST_GID" /home/claude

# Execute command as claude user
exec gosu claude "$@"
```

## Security Considerations

1. **No secrets in image**: OAuth tokens come from mounted ~/.claude
2. **Non-root execution**: User created at runtime with host UID/GID
3. **Read-only mounts**: Git and SSH configs are read-only
4. **No SUID binaries**: Only gosu (required for privilege dropping)
