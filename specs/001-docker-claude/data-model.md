# Data Model: Docker Container for Claude Code

**Feature**: 001-docker-claude
**Date**: 2026-01-24

## Overview

This is a CLI tool with minimal persistent state. All persistence is handled through host filesystem mounts. This document describes the logical entities and their relationships.

## Entities

### Container Image

The Docker image containing Claude Code and all dependencies.

| Attribute | Type | Description |
|-----------|------|-------------|
| name | string | Image tag (e.g., `clyde:local`) |
| base | string | Base image (`ubuntu:24.04`) |
| node_version | string | Node.js version (`20.x`) |
| claude_version | string | Claude Code npm package version |
| created_at | timestamp | Build timestamp |

**Lifecycle**: Built on first run if missing. Rebuilt manually when updates needed.

---

### Launch Configuration

Runtime configuration for container execution, derived from environment and flags.

| Attribute | Type | Default | Description |
|-----------|------|---------|-------------|
| memory_limit | string | `8g` | Container memory limit |
| cpu_limit | string | `4` | Container CPU limit |
| project_dir | path | `$PWD` | Host directory to mount |
| claude_home | path | `~/.claude` | Claude credentials directory |
| git_config | path | `~/.gitconfig` | Git configuration file |
| ssh_dir | path | `~/.ssh` | SSH keys directory |
| display | string | `$DISPLAY` | X11 display (if available) |
| wayland_display | string | `$WAYLAND_DISPLAY` | Wayland display (if available) |
| host_uid | integer | Current user UID | UID for container user |
| host_gid | integer | Current user GID | GID for container user |

**Lifecycle**: Computed fresh on each `clyde` invocation. Not persisted.

---

### Volume Mounts

Host-to-container filesystem bindings.

| Mount | Host Path | Container Path | Mode | Purpose |
|-------|-----------|----------------|------|---------|
| project | `$PWD` | `$PWD` | rw | User's project files |
| claude_home | `~/.claude` | `/home/claude/.claude` | rw | OAuth tokens, settings |
| git_config | `~/.gitconfig` | `/home/claude/.gitconfig` | ro | Git user configuration |
| ssh_keys | `~/.ssh` | `/home/claude/.ssh` | ro | SSH authentication |
| x11_socket | `/tmp/.X11-unix` | `/tmp/.X11-unix` | ro | X11 display (optional) |

**Note**: The container user's home is `/home/claude`, but `.claude` credentials from host are mapped there.

---

### Container User

The non-root user created inside the container at runtime.

| Attribute | Type | Source | Description |
|-----------|------|--------|-------------|
| username | string | `claude` | Fixed username |
| uid | integer | `HOST_UID` env | Matches host user |
| gid | integer | `HOST_GID` env | Matches host group |
| home | path | `/home/claude` | User home directory |
| shell | path | `/bin/bash` | Default shell |

**Lifecycle**: Created by entrypoint.sh on container start. Destroyed with container.

## State Diagram

```
┌─────────────────────────────────────────────────────────────────┐
│                        Host System                               │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐        │
│  │ ~/.claude│  │~/.gitconf│  │  ~/.ssh  │  │ Project  │        │
│  │ (rw)     │  │ ig (ro)  │  │  (ro)    │  │ Dir (rw) │        │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬─────┘        │
│       │             │             │             │               │
└───────┼─────────────┼─────────────┼─────────────┼───────────────┘
        │             │             │             │
        ▼             ▼             ▼             ▼
┌───────────────────────────────────────────────────────────────┐
│                     Docker Container                           │
│  ┌─────────────────────────────────────────────────────────┐  │
│  │                   /home/claude/                          │  │
│  │  .claude/  .gitconfig  .ssh/  [project mounted at PWD]  │  │
│  └─────────────────────────────────────────────────────────┘  │
│                           │                                    │
│                           ▼                                    │
│                    ┌─────────────┐                             │
│                    │ Claude Code │                             │
│                    │    CLI      │                             │
│                    └─────────────┘                             │
└───────────────────────────────────────────────────────────────┘
```

## Validation Rules

1. **Project directory must exist**: The current working directory must be a valid path.
2. **Docker must be running**: `docker info` must succeed before launch.
3. **Not root filesystem**: Refuse to mount `/` as project directory.
4. **UID/GID must be positive integers**: Extracted from `id -u` and `id -g`.
5. **Memory limit format**: Must match Docker format (e.g., `8g`, `512m`).
6. **CPU limit format**: Must be positive number (e.g., `4`, `2.5`).
