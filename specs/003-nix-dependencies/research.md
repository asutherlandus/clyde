# Research: Nix-Based Dependency Management

**Feature**: 003-nix-dependencies
**Date**: 2026-01-30

## Executive Summary

All research questions have been resolved. Key findings:

| Question | Resolution |
|----------|------------|
| Nix in Docker | Single-user mode with `--no-daemon`; chown /nix to runtime user |
| Flake merging | Use `inputsFrom` in `mkShellNoCC` to combine base + user + project |
| claude-code | **Hybrid approach**: npm install at runtime for always-latest; Nix provides Node.js |
| Output filtering | Use `--quiet --log-format=bar` + sed/grep for friendly progress |

---

## 1. Nix Single-User Installation in Docker

### Decision
Install Nix in single-user mode (no daemon) during Docker build, then transfer ownership to the runtime user via entrypoint.

### Rationale
- Single-user mode avoids systemd/daemon complexity in containers
- Ownership transfer enables non-root operation while preserving /nix persistence
- Official Nix installer supports this pattern with `--no-daemon` flag

### Implementation

**Dockerfile:**
```dockerfile
# Base: Ubuntu 24.04 minimal (~40MB vs ~78MB full)
FROM ubuntu:24.04-minimal

# Install Nix dependencies + security tools
RUN apt-get update && apt-get install -y --no-install-recommends \
    curl ca-certificates xz-utils \
    tini gosu \
    && rm -rf /var/lib/apt/lists/*

# Install Nix as root in single-user mode
RUN curl -L https://nixos.org/nix/install | sh -s -- --no-daemon

# Enable flakes
RUN mkdir -p /root/.config/nix && \
    echo "experimental-features = nix-command flakes" > /root/.config/nix/nix.conf

# Copy to system-wide location for non-root users
RUN cp /root/.config/nix/nix.conf /etc/nix/nix.conf
```

**Entrypoint (ownership transfer):**
```bash
# Transfer /nix ownership to runtime user
if [[ "$(stat -c '%u' /nix)" != "$HOST_UID" ]]; then
    chown -R "$HOST_UID:$HOST_GID" /nix 2>/dev/null || true
fi

# Source Nix profile
source /nix/var/nix/profiles/default/etc/profile.d/nix.sh
```

### Alternatives Considered
- **Multi-user daemon**: Rejected - adds complexity, requires systemd
- **NixOS base image**: Rejected - larger image, different entrypoint model
- **Install at runtime**: Rejected - too slow (3+ minutes on each start)

---

## 2. Flake Merging Strategy

### Decision
Use `inputsFrom` in `mkShellNoCC` to merge packages from multiple layers. Priority: project > user > base.

### Rationale
- `inputsFrom` is the idiomatic Nix way to compose devShells
- Cleanly handles package deduplication (rightmost wins)
- Works with both flake.nix and shell.nix (via wrapper)

### Implementation

**Base flake (container default):**
```nix
# docker/nix/flake.nix
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
            claude-code
            nodejs_20
            git
            gh
            curl
            openssh
          ];
        };
      }
    );
}
```

**Dynamic merging in entrypoint:**
```bash
# Detect configs and build nix develop command
NIX_ARGS=("/docker/nix")  # Base always included

if [[ -f "$HOME/.config/clyde/flake.nix" ]]; then
    # User layer exists - will be merged via inputsFrom
    export CLYDE_USER_FLAKE="$HOME/.config/clyde"
fi

if [[ -f "$PWD/flake.nix" ]]; then
    # Project layer exists - highest priority
    export CLYDE_PROJECT_FLAKE="$PWD"
fi
```

### Alternatives Considered
- **lib.recursiveUpdate**: More complex, manual deduplication needed
- **Wrapper flake generation**: Runtime flake generation is fragile
- **Sequential nix-shell**: Loses isolation benefits, slower

---

## 3. claude-code Installation Strategy

### Decision
**Hybrid approach**: Install Claude Code via npm at runtime (always latest), while Nix provides Node.js and other dependencies (pinned).

### Rationale
- **Always latest**: Users should always have the newest Claude Code without manual updates
- **Separation of concerns**: Claude Code (the tool) vs project dependencies (reproducible)
- **Fast startup**: npm cache in Docker volume means subsequent installs are fast (~2-5s check)
- **No nixpkgs lag**: npm releases are immediately available; nixpkgs packaging can lag

### Implementation

```bash
# In entrypoint.sh - after entering Nix environment (which provides Node.js)
NPM_GLOBAL="/home/claude/.npm-global"
export PATH="$NPM_GLOBAL/bin:$PATH"

# Install/update Claude Code (npm checks if update needed)
echo "Checking Claude Code version..."
npm install -g @anthropic-ai/claude-code --prefix "$NPM_GLOBAL" 2>/dev/null

# Now claude command is available
exec claude "$@"
```

### Volume Persistence
The npm global directory is persisted in `clyde-npm-cache` Docker volume:
- First run: ~30s to download Claude Code
- Subsequent runs: ~2-5s to check for updates (usually no-op)
- Update available: ~15-30s to download new version

### Alternatives Considered
- **pkgs.claude-code from nixpkgs**: Rejected - version lags behind npm, requires flake update for new releases
- **Pin to nixpkgs-unstable**: Rejected - still lags npm, less stable base packages
- **Build from source in Nix**: Rejected - complex, slow, maintenance burden
- **Install in Dockerfile**: Rejected - requires image rebuild for updates

---

## 4. Progress Output Filtering

### Decision
Use `nix develop --quiet --log-format=bar` combined with sed/grep filtering to show package names during fetch/build.

### Rationale
- Native `--log-format=bar` handles most verbosity suppression
- Simple sed/grep extracts package names from remaining output
- No external dependencies (nix-output-monitor is optional)
- Works for both flake and shell.nix

### Implementation

```bash
#!/usr/bin/env bash
# Wrapper for friendly progress output

show_progress() {
    local line="$1"
    # Extract package name from store path
    if [[ $line =~ /nix/store/[^-]+-([^/\ ]+) ]]; then
        local pkg="${BASH_REMATCH[1]}"
        echo "Fetching $pkg..."
    fi
}

run_nix_develop() {
    local config="$1"

    if [[ "${CLYDE_VERBOSE:-0}" == "1" ]]; then
        # Debug mode: full output
        nix develop "$config" -vv
    else
        # User-friendly mode
        nix develop "$config" --quiet --log-format=bar 2>&1 | \
            while IFS= read -r line; do
                show_progress "$line"
            done
    fi
}
```

### Output Examples

**Default (quiet):**
```
Fetching git-2.45.0...
Fetching nodejs-20.12.0...
Fetching claude-code-2.1.19...
Environment ready!
```

**Verbose (--nix-verbose):**
```
building '/nix/store/abc123-git-2.45.0.drv'...
unpacking source archive /nix/store/def456-git-2.45.0.tar.xz
... (full build logs)
```

### Alternatives Considered
- **nix-output-monitor**: Great UX but adds dependency
- **--log-format=internal-json + jq**: Complex, requires jq in image
- **Complete suppression**: Loses user feedback during long fetches

---

## 5. Architecture Decisions

### Nix Store Persistence

**Decision**: Named Docker volume `clyde-nix-store` mounted at `/nix`

**Mount command in bin/clyde:**
```bash
docker_args+=(-v "clyde-nix-store:/nix")
```

**Rationale**:
- Persists across container restarts (critical for <10s startup)
- Isolated from host Nix installation (avoids conflicts)
- Can be cleared with `docker volume rm clyde-nix-store`

### Configuration Discovery Order

1. `$PWD/flake.nix` (project - highest priority)
2. `$PWD/shell.nix` (project legacy)
3. `~/.config/clyde/flake.nix` (user)
4. `~/.config/clyde/shell.nix` (user legacy)
5. `/docker/nix/flake.nix` (container default)

### Environment Activation

**For flakes:** `nix develop <path>`
**For shell.nix:** `nix-shell <path>`

Both activated in entrypoint before exec'ing to claude.

---

## 6. Risk Mitigations

| Risk | Mitigation |
|------|------------|
| First-time download slow | Pre-populate store with defaults in image build |
| Invalid user config | Interactive prompt: "Proceed with defaults? [Y/n]" |
| Nix store corruption | Volume can be deleted; configs persist on host |
| Version conflicts | Project flake takes precedence; `follows` for alignment |
| Network unavailable | Clear error; cached packages still work |

---

## 7. References

- [Nix Installation Manual - Single User](https://nix.dev/manual/nix/2.28/installation/single-user.html)
- [NixOS Wiki - Flakes](https://wiki.nixos.org/wiki/Flakes)
- [nixpkgs JavaScript Documentation](https://github.com/NixOS/nixpkgs/blob/master/doc/languages-frameworks/javascript.section.md)
- [claude-code in nixpkgs](https://search.nixos.org/packages?channel=unstable&show=claude-code)
- [Using Nix with Dockerfiles - Mitchell Hashimoto](https://mitchellh.com/writing/nix-with-dockerfiles)
