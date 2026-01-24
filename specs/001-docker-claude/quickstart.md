# Quickstart: Clyde - Docker Container for Claude Code

## Prerequisites

- Docker Engine 24+ installed and running
- Anthropic account with Claude Code subscription (Max or similar)
- Linux or macOS (Windows via WSL2 may work but is untested)

## Installation

### Option 1: Clone and Install

```bash
# Clone the repository
git clone https://github.com/your-org/clyde.git
cd clyde

# Add bin directory to PATH (add to ~/.bashrc or ~/.zshrc for persistence)
export PATH="$PATH:$(pwd)/bin"
```

### Option 2: Direct Download

```bash
# Download the clyde script
curl -fsSL https://raw.githubusercontent.com/your-org/clyde/main/bin/clyde -o ~/.local/bin/clyde
chmod +x ~/.local/bin/clyde

# Ensure ~/.local/bin is in PATH
export PATH="$PATH:$HOME/.local/bin"
```

## First Run

```bash
# Navigate to any project directory
cd ~/projects/myapp

# Launch clyde (will build Docker image on first run)
clyde
```

On first run:
1. Docker image will be built (~2-5 minutes depending on connection)
2. Claude Code will prompt for authentication (browser will open if display available)
3. You're ready to use Claude Code in the isolated container

## Basic Usage

### Standard Launch

```bash
# Run Claude Code in current directory
clyde
```

### Custom Resource Limits

```bash
# More memory for large projects
clyde --memory 16g

# More CPUs for parallel operations
clyde --cpus 8

# Both
clyde --memory 16g --cpus 8
```

### Update Claude Code

```bash
# Force rebuild to get latest Claude Code version
clyde --build
```

### Pass Arguments to Claude Code

```bash
# Resume previous conversation
clyde -- --resume

# Use specific model
clyde -- --model claude-sonnet-4-20250514
```

## Configuration

Set defaults via environment variables in your shell profile:

```bash
# ~/.bashrc or ~/.zshrc
export CLYDE_MEMORY=16g
export CLYDE_CPUS=8
```

## What Gets Mounted

| Your Files | Container Access | Notes |
|------------|------------------|-------|
| Current directory | Read/Write | Full access for Claude Code to work on your project |
| `~/.claude` | Read/Write | OAuth tokens and settings (created if missing) |
| `~/.gitconfig` | Read-only | Your git user.name, user.email, etc. |
| `~/.ssh` | Read-only | SSH keys for git operations |

## Verification

After launching, verify the setup:

```bash
# Inside Claude Code, ask it to check the environment
> What directory am I in? Can you read the files here?

# Verify git works
> Run 'git status' to see the repository state

# Verify network access
> Can you access the internet? Try fetching a webpage.
```

## Troubleshooting

### Docker not running

```
Error: Docker is not running or not installed.
```

**Solution**: Start Docker Desktop (macOS/Windows) or the Docker daemon (Linux):
```bash
sudo systemctl start docker
```

### Permission denied on project files

**Cause**: UID/GID mismatch (rare edge case)

**Solution**: Check that your user has permission to access the project files:
```bash
ls -la .
```

### OAuth browser doesn't open (headless/SSH)

**Expected behavior**: When no display is available, Claude Code will print the OAuth URL. Copy and paste it into a browser on any machine, complete authentication, and the token will be saved.

### Image build fails

**Solution**: Check Docker build output for specific errors. Common issues:
- Network problems (can't download packages)
- Disk space (need ~2GB for image)

Try rebuilding:
```bash
clyde --build
```

## Uninstallation

```bash
# Remove the clyde script
rm ~/.local/bin/clyde  # or wherever you installed it

# Remove the Docker image
docker rmi clyde:local

# Optionally remove Claude credentials (shared with native Claude Code)
# rm -rf ~/.claude  # WARNING: This logs you out of Claude Code everywhere
```

## Security Notes

- **Container isolation**: Commands run by Claude Code cannot access files outside your mounted project directory
- **Skip-permissions mode**: Enabled by default inside the container; the container boundary provides isolation
- **Read-only mounts**: Your SSH keys and git config are mounted read-only
- **Non-root**: Container runs as your user (not root) for proper file permissions
- **Resource limits**: Default 8GB RAM / 4 CPU prevents runaway processes
