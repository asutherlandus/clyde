# Research: Docker Container for Claude Code

**Feature**: 001-docker-claude
**Date**: 2026-01-24

## Research Topics

### 1. Claude Code Installation in Docker

**Decision**: Install Claude Code via npm globally in the container image.

**Rationale**: Claude Code is distributed as an npm package (`@anthropic-ai/claude-code`). Global npm installation is the standard method and ensures the `claude` command is available in PATH for all users.

**Alternatives Considered**:
- Direct binary download: Not officially supported; npm is the canonical distribution method
- Local npm install: Would require PATH manipulation and complicates the entrypoint

**Implementation Notes**:
```dockerfile
RUN npm install -g @anthropic-ai/claude-code
```

---

### 2. Dynamic UID/GID Matching Strategy

**Decision**: Use entrypoint script to create user with matching UID/GID at container start, then use `gosu` to drop privileges.

**Rationale**: This is the established Docker pattern for matching host user permissions. Creating the user at runtime (not build time) allows the same image to work for any host user. `gosu` is preferred over `su` because it properly handles signals and doesn't create a subprocess.

**Alternatives Considered**:
- Fixed UID 1000: Only works for first user on typical Linux systems; breaks on macOS (UID 501) and multi-user systems
- `--user` flag only: Doesn't create home directory or proper user entry, causing issues with applications that expect these
- Docker userns-remap: Requires Docker daemon configuration, not portable

**Implementation Notes**:
```bash
# In entrypoint.sh
HOST_UID="${HOST_UID:-1000}"
HOST_GID="${HOST_GID:-1000}"
groupadd -g "$HOST_GID" claude 2>/dev/null || true
useradd -u "$HOST_UID" -g "$HOST_GID" -m -s /bin/bash claude 2>/dev/null || true
exec gosu claude "$@"
```

---

### 3. X11/Wayland Display Forwarding for OAuth

**Decision**: Pass through DISPLAY and WAYLAND_DISPLAY environment variables; mount X11 socket when available.

**Rationale**: OAuth authentication in Claude Code opens a browser. When running in a container, we need display forwarding to allow the browser to render on the host. X11 forwarding via socket mounting is the most reliable method for Linux. For headless environments, Claude Code falls back to printing the URL.

**Alternatives Considered**:
- VNC/remote desktop in container: Excessive complexity for a CLI tool
- Always headless: Degrades UX when display is available
- xdg-open passthrough via named pipe: Non-standard, fragile

**Implementation Notes**:
```bash
# In clyde script - detect and forward display
if [[ -n "${DISPLAY:-}" ]]; then
    DOCKER_ARGS+=(-e DISPLAY="$DISPLAY")
    DOCKER_ARGS+=(-v /tmp/.X11-unix:/tmp/.X11-unix:ro)
fi
if [[ -n "${WAYLAND_DISPLAY:-}" ]]; then
    DOCKER_ARGS+=(-e WAYLAND_DISPLAY="$WAYLAND_DISPLAY")
    DOCKER_ARGS+=(-v "${XDG_RUNTIME_DIR}/${WAYLAND_DISPLAY}:${XDG_RUNTIME_DIR}/${WAYLAND_DISPLAY}:ro")
fi
```

---

### 4. Node.js Version Selection

**Decision**: Use Node.js 20 LTS (pinned to 20.x via NodeSource repository).

**Rationale**: Node.js 20 is the current LTS release with support until April 2026. Claude Code requires Node.js 18+ per its documentation. Using the NodeSource repository provides consistent, up-to-date packages with proper signing.

**Alternatives Considered**:
- Node.js 18 LTS: Older, EOL October 2025
- Node.js 22: Not yet LTS, potential stability concerns
- Ubuntu's default nodejs package: Often outdated

**Implementation Notes**:
```dockerfile
RUN curl -fsSL https://deb.nodesource.com/setup_20.x | bash - \
    && apt-get install -y nodejs
```

---

### 5. Container Image Size Optimization

**Decision**: Use ubuntu:24.04 as base (not minimal/slim variant); accept larger image for compatibility.

**Rationale**: While Alpine or slim images would be smaller, Claude Code and its dependencies (especially native npm modules) have better compatibility with standard glibc-based distributions. Ubuntu 24.04 also provides better tooling for development tasks Claude Code might execute.

**Alternatives Considered**:
- Alpine Linux: musl libc compatibility issues with some npm packages
- Ubuntu minimal: Missing common utilities that Claude Code might invoke
- Distroless: No shell, incompatible with our entrypoint pattern

**Trade-off**: Larger image (~500MB vs ~100MB Alpine) but better reliability and compatibility.

---

### 6. Signal Handling and Graceful Shutdown

**Decision**: Use `exec` in entrypoint to replace shell process; install `tini` as init process.

**Rationale**: Docker only sends signals (SIGTERM, SIGINT) to PID 1. Without proper init, zombie processes can accumulate and signals won't propagate correctly. `tini` is a minimal init that handles signal forwarding and reaping. Using `exec` ensures Claude Code becomes PID 1's direct child.

**Alternatives Considered**:
- Docker's built-in `--init`: Requires user to remember flag; we want good defaults
- dumb-init: Similar to tini, but tini is more widely used
- No init: Leads to zombie processes and improper signal handling

**Implementation Notes**:
```dockerfile
RUN apt-get install -y tini
ENTRYPOINT ["/usr/bin/tini", "--", "/entrypoint.sh"]
```

---

### 7. Resource Limit Implementation

**Decision**: Use Docker's `--memory` and `--cpus` flags with configurable defaults.

**Rationale**: Docker's cgroup-based resource limits are the standard mechanism. The clyde script will set defaults (8GB, 4 CPUs) but allow override via environment variables or flags.

**Implementation Notes**:
```bash
# In clyde script
MEMORY_LIMIT="${CLYDE_MEMORY:-8g}"
CPU_LIMIT="${CLYDE_CPUS:-4}"
docker run --memory="$MEMORY_LIMIT" --cpus="$CPU_LIMIT" ...
```

---

### 8. Auto-build Strategy

**Decision**: Check for image existence via `docker image inspect`; build from embedded Dockerfile if missing.

**Rationale**: Users shouldn't need to manually build the image. The clyde script will check if the image exists and trigger a build if not. This provides a seamless first-run experience.

**Alternatives Considered**:
- Pull from registry: Requires publishing infrastructure; adds external dependency
- Always rebuild: Wastes time on subsequent runs
- Separate install script: Extra step for users

**Implementation Notes**:
```bash
IMAGE_NAME="clyde:local"
if ! docker image inspect "$IMAGE_NAME" &>/dev/null; then
    echo "Building Clyde image (first run)..." >&2
    docker build -t "$IMAGE_NAME" "$SCRIPT_DIR/../docker"
fi
```

## Summary

All technical decisions have been resolved. Key choices:
- npm global install for Claude Code
- Runtime UID/GID matching with gosu
- X11/Wayland passthrough when available
- Node.js 20 LTS via NodeSource
- Full Ubuntu 24.04 for compatibility
- tini init for proper signal handling
- Docker resource limits with sensible defaults
- Auto-build on first run
