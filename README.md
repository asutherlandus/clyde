![CLYDE](clyde_banner.png)

# **CLYDE** - **C**omand **L**ine **YOLO** **D**evelopment **E**nvironment

A Docker container for running Claude Code in an isolated environment.

## Features

- **Multi-Account Profiles** - Switch between multiple Anthropic accounts (Pro, Max, Work) with named profiles
- **Container Isolation** - Run Claude Code in a sandboxed Docker environment
- **Seamless Integration** - Mounts your project directory, git config, and forwards SSH agent automatically
- **Resource Control** - Configurable memory and CPU limits
- **Nix Package Management** - Add project-specific tools via `flake.nix` without rebuilding the image
- **Debugging Options** - Shell mode, X11 forwarding, and single-command execution for troubleshooting

## Prerequisites

- Docker Engine 24+ installed and running
- Anthropic account with Claude Code subscription (Max or similar)
- Linux or macOS (Windows via WSL2 may work but is untested)

## Comparison with Alternatives

You may want to consider one of these alternative options for running Claude in a Docker container:

| Feature | Clyde | [ClaudeBox](https://github.com/RchGrav/claudebox) | [Docker Official](https://docs.docker.com/ai/sandboxes/claude-code/) | [claude-code-container](https://github.com/tintinweb/claude-code-container) |
|---------|-------|----------|-----------------|------------------------|
| **Agents Supported** | Claude Code | Claude Code | Claude Code | Claude Code |
| **Operating Systems** | Linux, macOS, WSL2 | Linux, macOS | Linux, macOS, Windows | Linux, macOS |
| **Multi-Account Profiles** | ✅ Named profiles with token management | ❌ Per-project auth | ❌ Single credential volume | ❌ |
| **UID/GID Matching** | ✅ Dynamic at runtime | ✅ | ❌ Fixed `agent` user | ❌ Fixed `claude` user (1001) |
| **SSH Handling** | ✅ Agent forwarding (keys stay on host) | ❌ GitHub CLI only | ❌ | ❌ |
| **Network Isolation** | ❌ Full network access | ✅ Firewall with allowlists | ❌ Full network access | ✅ Bridge networking |
| **Resource Limits** | ✅ Configurable RAM/CPU | ❌ | ❌ | ✅ PID limit (100) |
| **Security Hardening** | Non-root, read-only mounts | Non-root, optional sudo | Non-root with sudo | Capability dropping, no-new-privileges, tmpfs isolation |
| **Pre-installed Tools** | Nix-based (customizable via flake.nix) | 15+ language profiles | Node.js, Go, Python, gh, Docker CLI | Minimal + MCP servers |
| **Project Isolation** | Shared image, per-directory | Separate image per project | Shared image | Shared image |
| **Ease of Setup** | `clyde` (auto-builds) | `claudebox` | `docker sandbox run claude` | `docker compose up` |
| **X11/GUI Support** | ✅ `--x11` flag with font mounting | ❌ | ❌ | ❌ |
| **Shell/Exec Mode** | ✅ `--shell`, `--exec` | ❌ | ❌ | ❌ |
| **IDE Integration** | ❌ | ❌ | ❌ | ❌ |
| **Use Case** | Daily development with account switching | Multi-project with network control | Quick start, official support | Security-focused analysis |

**Why choose Clyde?**
- You have multiple Anthropic accounts (Pro, Max, Work) and need to switch between them
- You need SSH git operations (others rely on GitHub CLI or don't support SSH)
- You prefer configurable resource limits for different project sizes

## Installation

### Option 1: Clone and Install

```bash
# Clone the repository
git clone https://github.com/asutherlandus/clyde.git
cd clyde

# Add bin directory to PATH (add to ~/.bashrc or ~/.zshrc for persistence)
export PATH="$PATH:$(pwd)/bin"
```

### Option 2: Direct Download

```bash
# Download the clyde script
curl -fsSL https://raw.githubusercontent.com/asutherlandus/clyde/main/bin/clyde -o ~/.local/bin/clyde
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

### Custom Development Tools

Clyde uses Nix to manage development dependencies. Add project-specific tools by creating a `flake.nix` in your project root:

```nix
{
  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-24.05";
  outputs = { self, nixpkgs }:
    let pkgs = nixpkgs.legacyPackages.x86_64-linux;
    in {
      devShells.default = pkgs.mkShellNoCC {
        packages = with pkgs; [ python312 poetry rustc cargo ];
      };
    };
}
```

Packages are cached in a Docker volume - first run downloads them, subsequent runs start in under a second.

See [Nix Dependency Management](docs/nix-dependency-management.md) for more examples (Python, Rust, Go, etc.) and configuration options.

### Pass Arguments to Claude Code

```bash
# Resume previous conversation
clyde -- --resume

# Use specific model
clyde -- --model claude-sonnet-4-20250514
```

## Container Debugging

Clyde provides options for debugging and testing in the container environment without starting Claude Code.

### Shell Mode

Launch an interactive bash shell with the same environment Claude Code uses:

```bash
# Get a shell prompt inside the container
clyde --shell

# Inside the container, you have access to the same tools
git status
npm test
cargo build
```

This is useful for:
- Debugging failing tests in the exact environment Claude sees
- Verifying Nix packages are installed correctly
- Manual testing before running Claude Code

### Execute a Single Command

Run a command in the container and exit immediately:

```bash
# Run tests
clyde --exec npm test

# Build project
clyde --exec cargo build --release

# Any command with arguments
clyde --exec python -m pytest -v tests/
```

Useful for CI/CD pipelines or scripted workflows.

### X11 Forwarding

Enable graphical application support (Linux only):

```bash
# Shell with X11 for GUI debugging tools
clyde --x11 --shell

# Run a graphical application
clyde --x11 --exec xclock

# Normal Claude mode with X11 (if Claude needs to run GUI apps)
clyde --x11
```

X11 forwarding mounts `/tmp/.X11-unix` and passes your `DISPLAY` environment variable. Host fonts are automatically mounted for proper text rendering.

### Combining Options

| Command | Behavior |
|---------|----------|
| `clyde --shell` | Interactive bash shell |
| `clyde --exec cmd` | Run `cmd` and exit |
| `clyde --x11` | Claude Code with X11 |
| `clyde --x11 --shell` | Shell with X11 |
| `clyde --x11 --exec cmd` | Run graphical `cmd` |
| `clyde --shell --exec cmd` | Error (mutually exclusive) |

### Environment Variables

Set debugging defaults in your shell profile:

```bash
export CLYDE_SHELL=true   # Always start in shell mode
export CLYDE_X11=true     # Always enable X11
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
| `~/.local/bin` | Read-only | User-installed binaries (mounted to separate path, added to PATH) |
| `~/.config/clyde` | Read-only | User's global Nix packages (optional) |
| `clyde-nix-store` (volume) | Read/Write | Nix package cache (persists across runs) |
| `clyde-claude-cache` (volume) | Read/Write | Claude Code installer data cache |

**With `--x11` enabled:**

| Your Files | Container Access | Notes |
|------------|------------------|-------|
| `/tmp/.X11-unix` | Read/Write | X11 socket for display forwarding |
| `/usr/share/fonts` | Read-only | System fonts |
| `~/.local/share/fonts` | Read-only | User fonts (XDG location) |
| `~/.fonts` | Read-only | User fonts (legacy location) |
| `/var/cache/fontconfig` | Read-only | Font cache for faster loading |

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

### First run is slow / Nix downloading packages

The first time you run Clyde (or add new packages to `flake.nix`), Nix downloads the required packages. This can take 30-60 seconds depending on your network.

Subsequent runs use the cached packages and start in under a second.

To free disk space from unused packages:
```bash
clyde --nix-gc
```

### X11: "cannot open display" or missing fonts

```
Error: X11 forwarding requested but DISPLAY is not set.
```

**Solution**: Ensure you're running from a graphical session with DISPLAY set:
```bash
echo $DISPLAY  # Should show something like ":0" or ":1"
```

If DISPLAY is empty, you're likely in a non-graphical terminal (SSH without X11 forwarding, TTY console, etc.).

**For SSH with X11 forwarding:**
```bash
ssh -X user@host  # Enable X11 forwarding
```

**If fonts are missing in GUI apps:**

Clyde mounts host fonts automatically, but if you still see missing fonts:
1. Verify fonts exist on host: `ls /usr/share/fonts`
2. Rebuild font cache: `fc-cache -fv`
3. Try again with `clyde --x11 --shell`

## Uninstallation

```bash
# Remove the clyde script
rm ~/.local/bin/clyde  # or wherever you installed it

# Remove the Docker image
docker rmi clyde:local

# Remove Nix package cache volumes
docker volume rm clyde-nix-store clyde-claude-cache

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
- **X11 security**: When using `--x11`, container processes can interact with your X11 display. This is a known X11 limitation. Only use with trusted code.

## License

See [LICENSE](LICENSE) for details.
