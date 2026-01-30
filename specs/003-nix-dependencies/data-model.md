# Data Model: Nix-Based Dependency Management

**Feature**: 003-nix-dependencies
**Date**: 2026-01-30

## Configuration Files

### 1. Container Default Flake

**Location**: `/docker/nix/flake.nix` (in container)
**Purpose**: Provides baseline packages for all Clyde sessions (NOTE: Claude Code is installed via npm, not Nix)

```nix
{
  description = "Clyde base environment";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-24.05";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let pkgs = nixpkgs.legacyPackages.${system};
      in {
        devShells.default = pkgs.mkShellNoCC {
          name = "clyde-base";
          packages = with pkgs; [
            # NOTE: claude-code installed via npm for always-latest
            nodejs_20         # Node.js 20 LTS (required for Claude Code)
            git               # Version control
            gh                # GitHub CLI
            curl              # HTTP client
            openssh           # SSH client
          ];

          shellHook = ''
            export CLYDE_ENV="base"
          '';
        };
      }
    );
}
```

### 2. User Global Configuration

**Location**: `~/.config/clyde/flake.nix` (on host, mounted read-only)
**Purpose**: User's preferred packages across all projects

```nix
{
  description = "My Clyde user preferences";

  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-24.05";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";  # or "aarch64-linux"
      pkgs = nixpkgs.legacyPackages.${system};
    in {
      devShells.default = pkgs.mkShellNoCC {
        name = "clyde-user";
        packages = with pkgs; [
          # Add your preferred packages here
          python312
          jq
          ripgrep
        ];
      };
    };
}
```

**Legacy format**: `~/.config/clyde/shell.nix`
```nix
{ pkgs ? import <nixpkgs> {} }:
pkgs.mkShell {
  packages = with pkgs; [ python312 jq ripgrep ];
}
```

### 3. Project Configuration

**Location**: `./flake.nix` (in project root)
**Purpose**: Project-specific dependencies

```nix
{
  description = "Development environment for my-project";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-24.05";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let pkgs = nixpkgs.legacyPackages.${system};
      in {
        devShells.default = pkgs.mkShellNoCC {
          name = "my-project";
          packages = with pkgs; [
            # Project-specific tools
            rustc
            cargo
            rust-analyzer
          ];

          shellHook = ''
            echo "Rust project environment loaded"
          '';
        };
      }
    );
}
```

**Legacy format**: `./shell.nix`

---

## Environment Variables

### Host-Side (bin/clyde)

| Variable | Default | Description |
|----------|---------|-------------|
| `CLYDE_IMAGE` | `clyde:local` | Docker image name |
| `CLYDE_MEMORY` | `8g` | Container memory limit |
| `CLYDE_CPUS` | `4` | Container CPU limit |
| `CLYDE_PROFILE` | (none) | Authentication profile name |
| `CLYDE_NO_GIT` | (unset) | Skip git/SSH mounts if set to `1` |
| `CLYDE_DOCKER_DIR` | `script/../docker` | Path to docker build context |

### New Variables (this feature)

| Variable | Default | Description |
|----------|---------|-------------|
| `CLYDE_NIX_VERBOSE` | `0` | Show full Nix output if `1` |
| `CLYDE_NIX_STORE_VOLUME` | `clyde-nix-store` | Docker volume name for /nix |
| `CLYDE_NPM_CACHE_VOLUME` | `clyde-npm-cache` | Docker volume name for npm global (Claude Code) |

### Container-Side (entrypoint.sh)

| Variable | Set By | Description |
|----------|--------|-------------|
| `HOST_UID` | bin/clyde | Host user's UID |
| `HOST_GID` | bin/clyde | Host user's GID |
| `CLYDE_ENV` | entrypoint | Active environment layer (base/user/project) |
| `CLYDE_USER_FLAKE` | entrypoint | Path to user flake (if exists) |
| `CLYDE_PROJECT_FLAKE` | entrypoint | Path to project flake (if exists) |
| `NIX_PATH` | nix profile | Nix package path |

---

## Docker Volumes

### Nix Store Volume

| Property | Value |
|----------|-------|
| Name | `clyde-nix-store` |
| Mount Point | `/nix` |
| Purpose | Persist Nix packages across container restarts |
| Typical Size | 2-10 GB |
| Creation | Automatic on first `clyde` run |
| Cleanup | `clyde --nix-gc` or `docker volume rm clyde-nix-store` |

### npm Cache Volume

| Property | Value |
|----------|-------|
| Name | `clyde-npm-cache` |
| Mount Point | `/home/claude/.npm-global` |
| Purpose | Persist Claude Code installation; enables always-latest while keeping fast startup |
| Typical Size | 100-500 MB |
| Creation | Automatic on first `clyde` run |
| Cleanup | `docker volume rm clyde-npm-cache` |

### Mount Points (in container)

| Host Path | Container Path | Mode | Purpose |
|-----------|----------------|------|---------|
| `$PWD` | `$PWD` | rw | Current working directory |
| `~/.claude` | `/home/claude/.claude` | rw | Claude credentials |
| `~/.claude.json` | `/home/claude/.claude.json` | rw | Claude config |
| `~/.gitconfig` | `/home/claude/.gitconfig` | ro | Git configuration |
| `$SSH_AUTH_SOCK` | `/ssh-agent` | ro | SSH agent socket |
| `~/.config/gh` | `/home/claude/.config/gh` | ro | GitHub CLI auth |
| `~/.local/bin` | `/home/claude/.local/bin` | ro | User binaries |
| `~/.config/clyde` | `/home/claude/.config/clyde` | ro | User Nix config |
| `clyde-nix-store` | `/nix` | rw | Nix store (volume) |
| `clyde-npm-cache` | `/home/claude/.npm-global` | rw | npm global for Claude Code (volume) |

---

## Command-Line Flags

### Existing Flags (unchanged)

| Flag | Description |
|------|-------------|
| `--memory <SIZE>` | Container memory limit |
| `--cpus <NUM>` | Container CPU limit |
| `--build` | Force rebuild Docker image |
| `--no-git` | Skip git/SSH mounts |
| `-P, --profile <NAME>` | Use authentication profile |
| `--add-profile <NAME>` | Create new profile |
| `--delete-profile <NAME>` | Delete profile |
| `--list-profiles` | List profiles |
| `--set-default <NAME>` | Set default profile |
| `-h, --help` | Show help |
| `-v, --version` | Show version |

### New Flags (this feature)

| Flag | Description |
|------|-------------|
| `--nix-verbose` | Show full Nix output during environment setup |
| `--nix-gc` | Garbage collect unused packages from Nix store |
| `--list-packages` | Show packages that would be available (without starting session) |

---

## State Transitions

### Configuration Discovery Flow

```
┌──────────────────────────────────────────────────────────────┐
│                        bin/clyde                              │
│  1. Check $PWD/flake.nix → set CLYDE_PROJECT_FLAKE           │
│  2. Check $PWD/shell.nix → set CLYDE_PROJECT_SHELL           │
│  3. Mount ~/.config/clyde if exists                          │
│  4. Mount clyde-nix-store volume at /nix                     │
│  5. Mount clyde-npm-cache volume at ~/.npm-global            │
│  6. Pass detection env vars to container                     │
└──────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────┐
│                     entrypoint.sh                             │
│  1. Create user matching HOST_UID/HOST_GID                   │
│  2. Fix /nix ownership if needed                             │
│  3. Source Nix profile, enter base Nix environment           │
│  4. Install/update Claude Code via npm (cached in volume)    │
│     npm install -g @anthropic-ai/claude-code --prefix ...    │
│  5. Detect active configuration:                             │
│     - Project flake.nix? → nix develop $PWD                  │
│     - Project shell.nix? → nix-shell $PWD/shell.nix          │
│     - User flake.nix? → nix develop ~/.config/clyde          │
│     - User shell.nix? → nix-shell ~/.config/clyde/shell.nix  │
│     - Default → nix develop /docker/nix                      │
│  6. Merge with inputsFrom if multiple configs                │
│  7. Exec claude with merged environment                      │
└──────────────────────────────────────────────────────────────┘
```

### Error Recovery Flow

```
┌─────────────────┐
│ Load Nix Config │
└────────┬────────┘
         │
         ▼
    ┌────────────┐
    │ Valid Nix? │
    └─────┬──────┘
          │
    ┌─────┴─────┐
    │           │
   Yes          No
    │           │
    ▼           ▼
┌────────┐  ┌──────────────────────┐
│ Apply  │  │ Show error message   │
│ Config │  │ "Config invalid.     │
└────────┘  │  Proceed with        │
            │  defaults? [Y/n]"    │
            └──────────┬───────────┘
                       │
                 ┌─────┴─────┐
                 │           │
                Yes          No
                 │           │
                 ▼           ▼
            ┌────────┐  ┌────────┐
            │ Use    │  │ Exit   │
            │ Default│  │ (1)    │
            └────────┘  └────────┘
```

---

## Validation Rules

### Profile Names (existing)
- Pattern: `^[a-zA-Z0-9_-]+$`
- Max length: 64 characters

### Nix Configuration Files
- Must be valid Nix syntax
- Flakes must have `outputs` attribute
- shell.nix must be callable with `{ pkgs ? ... }` pattern

### Volume Names
- Pattern: `^[a-zA-Z0-9][a-zA-Z0-9_.-]*$`
- Defaults: `clyde-nix-store`, `clyde-npm-cache`
