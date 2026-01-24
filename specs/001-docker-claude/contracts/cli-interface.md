# CLI Interface Contract: clyde

**Version**: 1.0.0
**Date**: 2026-01-24

## Command Synopsis

```
clyde [OPTIONS] [-- CLAUDE_ARGS...]
```

## Description

Launch Claude Code in an isolated Docker container with the current directory mounted.

## Options

| Option | Environment Variable | Default | Description |
|--------|---------------------|---------|-------------|
| `--memory <SIZE>` | `CLYDE_MEMORY` | `8g` | Container memory limit (e.g., `4g`, `16g`) |
| `--cpus <NUM>` | `CLYDE_CPUS` | `4` | Container CPU limit (e.g., `2`, `8`) |
| `--build` | - | - | Force rebuild of the Docker image |
| `--no-git` | `CLYDE_NO_GIT` | - | Skip mounting ~/.gitconfig and ~/.ssh |
| `--help`, `-h` | - | - | Display help message |
| `--version`, `-v` | - | - | Display version information |

## Arguments

Any arguments after `--` are passed directly to Claude Code inside the container.

```bash
# Example: Pass --help to Claude Code
clyde -- --help

# Example: Start Claude Code with a specific model
clyde -- --model claude-sonnet-4-20250514
```

## Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success (Claude Code exited normally) |
| 1 | General error (see stderr for details) |
| 2 | Docker not available or not running |
| 3 | Image build failed |
| 4 | Invalid arguments |
| 5 | Invalid working directory (e.g., root `/`) |

## Environment Variables

| Variable | Description |
|----------|-------------|
| `CLYDE_MEMORY` | Default memory limit (overridden by `--memory`) |
| `CLYDE_CPUS` | Default CPU limit (overridden by `--cpus`) |
| `CLYDE_NO_GIT` | If set to `1`, skip git/SSH mounts |
| `CLYDE_IMAGE` | Custom image name (default: `clyde:local`) |
| `DISPLAY` | X11 display (forwarded to container if set) |
| `WAYLAND_DISPLAY` | Wayland display (forwarded to container if set) |

## Examples

### Basic Usage

```bash
# Launch Claude Code in current directory
cd ~/projects/myapp
clyde
```

### Custom Resource Limits

```bash
# Run with 16GB RAM and 8 CPUs
clyde --memory 16g --cpus 8
```

### Force Rebuild Image

```bash
# Rebuild the Docker image (e.g., to update Claude Code)
clyde --build
```

### Pass Arguments to Claude Code

```bash
# Start Claude Code with specific options
clyde -- --resume
```

### Environment Variable Configuration

```bash
# Set defaults in shell profile
export CLYDE_MEMORY=16g
export CLYDE_CPUS=8

# Then just run
clyde
```

## Volume Mounts

The following host paths are mounted into the container:

| Host Path | Container Path | Mode | Condition |
|-----------|----------------|------|-----------|
| `$PWD` | `$PWD` | read-write | Always |
| `~/.claude` | `/home/claude/.claude` | read-write | Always (created if missing) |
| `~/.gitconfig` | `/home/claude/.gitconfig` | read-only | Unless `--no-git` |
| `~/.ssh` | `/home/claude/.ssh` | read-only | Unless `--no-git` |
| `/tmp/.X11-unix` | `/tmp/.X11-unix` | read-only | If `$DISPLAY` set |

## Error Messages

### Docker Not Available

```
Error: Docker is not running or not installed.
Please start Docker and try again.
```

### Invalid Working Directory

```
Error: Cannot run clyde from root filesystem (/).
Please change to a project directory first.
```

### Image Build Failed

```
Error: Failed to build Clyde image.
Check the build output above for details.
```

### Missing Claude Credentials

```
Note: No existing Claude credentials found.
You will be prompted to authenticate on first use.
```
