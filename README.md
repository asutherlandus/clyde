![CLYDE](clyde_banner.png)

# **CLYDE** - **C**omand **L**ine **YOLO** **D**evelopment **E**nvironment

A Docker container for running Claude Code in an isolated environment.

## Features

- **Multi-Account Profiles** - Switch between multiple Anthropic accounts (Pro, Max, Work) with named profiles
- **Container Isolation** - Run Claude Code in a sandboxed Docker environment
- **Seamless Integration** - Mounts your project directory, git config, and forwards SSH agent automatically
- **Resource Control** - Configurable memory and CPU limits

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
1. Docker image will be built automatically
2. Claude Code will prompt for authentication (copy the URL and open in browser)
3. You're ready to use Claude Code in the isolated container

**Tip**: To skip in-container authentication, set up a profile first:

```bash
clyde --add-profile default
clyde  # Now uses your profile token automatically
```

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
# Rebuild the Docker image (pulls latest Claude Code)
clyde --build
```

### Pass Arguments to Claude Code

```bash
# Resume previous conversation
clyde -- --resume

# Use specific model
clyde -- --model claude-sonnet-4-20250514
```

## Multi-Account Profiles

Clyde supports named profiles for switching between multiple Anthropic accounts. This is useful if you have separate Pro and Max subscriptions, or work/personal accounts.

### Creating a Profile

```bash
# Create a profile (interactive)
clyde --add-profile pro
```

If you have existing OAuth credentials in `~/.claude/.credentials.json`, clyde will offer to use those. Otherwise, you'll need to:

1. Run `claude setup-token` on your host system
2. Complete browser authentication
3. Paste the resulting token when prompted

### Using Profiles

```bash
# Launch with a specific profile
clyde --profile pro
clyde -P max

# Set a default profile (used when --profile not specified)
clyde --set-default pro

# Override default with environment variable
CLYDE_PROFILE=max clyde
```

### Managing Profiles

```bash
# List all profiles (* marks default)
clyde --list-profiles

# Delete a profile
clyde --delete-profile old-account
```

### Profile Storage

Profiles are stored in `~/.claude/profiles/` with mode 600:

```
~/.claude/profiles/
├── .default           # Contains name of default profile
├── pro.json           # Profile with token and metadata
└── max.json
```

### Token Expiration

OAuth tokens expire periodically. If authentication fails, recreate the profile:

```bash
clyde --delete-profile pro
clyde --add-profile pro
```

## Configuration

Set defaults via environment variables in your shell profile:

```bash
# ~/.bashrc or ~/.zshrc
export CLYDE_MEMORY=16g
export CLYDE_CPUS=8
export CLYDE_PROFILE=pro  # Default profile to use
```

## What Gets Mounted

| Your Files | Container Access | Notes |
|------------|------------------|-------|
| Current directory | Read/Write | Full access for Claude Code to work on your project |
| `~/.claude` | Read/Write | OAuth tokens, settings, and profiles (created if missing) |
| `~/.claude.json` | Read/Write | Onboarding state, theme, tips history (skips setup wizard) |
| `~/.gitconfig` | Read-only | Your git user.name, user.email, etc. |
| `$SSH_AUTH_SOCK` | Read-only | SSH agent socket for git operations (see below) |
| `~/.local/bin` | Read-only | User-installed binaries (added to PATH) |

Note: When using `--profile`, the token is passed via a mounted secret file (not environment variable) and the container doesn't need access to profile files directly.

## SSH Setup for Git Operations

Clyde forwards your SSH agent socket to the container rather than mounting your `~/.ssh` directory directly. This is more secure because your private keys never enter the container.

### Prerequisites

Your SSH agent must be running with your keys loaded:

```bash
# Check if SSH agent is running
echo $SSH_AUTH_SOCK

# If empty, start the agent
eval "$(ssh-agent -s)"

# Add your SSH key
ssh-add ~/.ssh/id_ed25519   # or id_rsa, etc.

# Verify keys are loaded
ssh-add -l
```

### Persistent SSH Agent

To avoid running `ssh-add` every time, configure your shell to start the agent automatically.

**For ~/.bashrc or ~/.zshrc:**

```bash
# Start SSH agent if not running
if [ -z "$SSH_AUTH_SOCK" ]; then
    eval "$(ssh-agent -s)" > /dev/null
    ssh-add ~/.ssh/id_ed25519 2>/dev/null
fi
```

**For GNOME/KDE desktop users:** The keyring usually handles this automatically. Your keys should already be available.

**For macOS users:**

```bash
# Add to ~/.ssh/config to use Keychain
Host *
    AddKeysToAgent yes
    UseKeychain yes
    IdentityFile ~/.ssh/id_ed25519
```

### Verifying SSH Works in Container

```bash
# Launch clyde and test SSH
clyde

# Inside Claude Code, ask it to test:
> Run 'ssh -T git@github.com' to verify SSH works
```

### Disabling SSH/Git Integration

If you don't need git operations via SSH:

```bash
clyde --no-git
```

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

### OAuth authentication

Claude Code will print an OAuth URL during authentication. Copy and paste it into a browser, complete authentication, and the token will be saved.

### Image build fails

**Solution**: Check Docker build output for specific errors. Common issues:
- Network problems (can't download packages)
- Disk space (need ~2GB for image)

Try rebuilding:
```bash
clyde --build
```

### Git SSH operations fail in container

```
Warning: SSH agent not running. Git SSH operations will not work in container.
```

**Solution**: Start the SSH agent and add your key:

```bash
# Start agent
eval "$(ssh-agent -s)"

# Add your key
ssh-add ~/.ssh/id_ed25519

# Verify it's loaded
ssh-add -l

# Now launch clyde
clyde
```

See [SSH Setup for Git Operations](#ssh-setup-for-git-operations) for persistent configuration.

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
- **SSH agent forwarding**: Private keys stay on your host; only the agent socket is forwarded (read-only)
- **Read-only mounts**: Your git config is mounted read-only
- **Non-root**: Container runs as your user (not root) for proper file permissions
- **Resource limits**: Default 8GB RAM / 4 CPU prevents runaway processes

## License

See [LICENSE](LICENSE) for details.
