# Changelog

All notable changes to Clyde will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.4.0] - 2026-02-03

### Added
- `--shell` flag to launch interactive bash shell instead of Claude Code
- `--x11` flag for X11 forwarding to run graphical applications
- `--exec <command>` flag to run arbitrary commands in the container
- Shell history navigation with arrow keys (bashInteractive + readline)

### Fixed
- Pass TERM environment variable to container for proper terminal handling

## [0.3.0] - 2026-01-30

### Added
- Nix-based dependency management for reproducible environments
- Project-specific dependencies via `flake.nix` or `shell.nix` in project root
- User-level package customization via `~/.config/clyde/flake.nix`
- `--nix-verbose` flag for detailed Nix output during troubleshooting
- `--nix-gc` flag to run Nix garbage collection
- `--list-packages` flag to show available packages
- Named Docker volumes for Nix store and npm cache persistence

### Changed
- Claude Code now installed via npm at runtime (always latest version)
- Base image switched to Ubuntu 24.04 minimal for smaller size
- Project dependencies pinned via Nix flake.lock for reproducibility

## [0.2.0] - 2026-01-27

### Added
- Multi-account profile support (`--profile` flag)
- Token-based authentication (`clyde setup-token`)
- GitHub CLI (gh) included in container
- Mount `~/.local/bin` for user-installed tools
- `CLYDE_DOCKER_DIR` environment variable for flexible installation

### Changed
- SSH agent forwarding instead of mounting `~/.ssh` (improved security)

### Fixed
- OAuth credential persistence across container restarts
- Auth URLs now printed to terminal for OAuth flow
- Read-only mount handling in entrypoint
- Security vulnerabilities in container and profile management

## [0.1.0] - 2026-01-24

### Added
- Initial Docker container implementation for Claude Code
- `clyde` launch script with automatic image building
- Volume mounting preserving host directory paths
- UID/GID matching for correct file permissions
- OAuth authentication via mounted `~/.claude` directory
- Configurable resource limits (`--memory`, `--cpu`)
- `tini` init process for proper signal handling
- `gosu` for secure privilege dropping
