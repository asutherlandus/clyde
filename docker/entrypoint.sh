#!/usr/bin/env bash
set -euo pipefail

# Clyde entrypoint: Create container user matching host UID/GID and activate Nix environment
# This ensures file permissions work correctly with mounted volumes

##############################################################################
# Configuration
##############################################################################

HOST_UID="${HOST_UID:-1000}"
HOST_GID="${HOST_GID:-1000}"
CLYDE_NIX_VERBOSE="${CLYDE_NIX_VERBOSE:-0}"
CLYDE_NIX_GC="${CLYDE_NIX_GC:-0}"
NPM_GLOBAL="/home/claude/.npm-global"

##############################################################################
# Validation
##############################################################################

# Validate UID/GID are numeric and within valid range
if [[ ! "$HOST_UID" =~ ^[0-9]+$ ]] || [[ "$HOST_UID" -lt 1 ]]; then
    echo "Error: HOST_UID must be a positive integer, got '$HOST_UID'" >&2
    exit 1
fi
if [[ ! "$HOST_GID" =~ ^[0-9]+$ ]] || [[ "$HOST_GID" -lt 1 ]]; then
    echo "Error: HOST_GID must be a positive integer, got '$HOST_GID'" >&2
    exit 1
fi

# Warn if UID is below 1000 (system user range)
if [[ "$HOST_UID" -lt 1000 ]]; then
    echo "Warning: HOST_UID=$HOST_UID is in system user range (< 1000)" >&2
fi

##############################################################################
# User Setup
##############################################################################

setup_user() {
    local target_user=""

    # Check if a user with the target UID already exists
    local existing_user
    existing_user=$(getent passwd "$HOST_UID" | cut -d: -f1 || true)

    if [[ -n "$existing_user" ]]; then
        # User with this UID exists - use it directly
        target_user="$existing_user"

        # Create home directory for claude if it doesn't exist
        mkdir -p /home/claude
        chown "$HOST_UID:$HOST_GID" /home/claude

        # Only chown .claude directory (not read-only mounts like .gitconfig)
        if [[ -d /home/claude/.claude ]]; then
            chown -R "$HOST_UID:$HOST_GID" /home/claude/.claude
        fi

        # Modify the existing user to use /home/claude as home
        usermod -d /home/claude "$existing_user" 2>/dev/null || true
    else
        # No user with this UID exists - create claude user
        target_user="claude"

        # Get the group name for this GID, or create a new one
        local existing_group
        existing_group=$(getent group "$HOST_GID" | cut -d: -f1 || true)
        if [[ -z "$existing_group" ]]; then
            groupadd -g "$HOST_GID" claude
        fi

        # Create user with matching UID and the target group
        useradd -u "$HOST_UID" -g "$HOST_GID" -m -d /home/claude -s /bin/bash claude 2>/dev/null || true

        # Ensure home directory ownership is correct (not read-only mounts)
        chown "$HOST_UID:$HOST_GID" /home/claude
        if [[ -d /home/claude/.claude ]]; then
            chown -R "$HOST_UID:$HOST_GID" /home/claude/.claude
        fi
    fi

    echo "$target_user"
}

##############################################################################
# Nix Setup
##############################################################################

setup_nix_ownership() {
    # Transfer /nix ownership to runtime user if needed
    # This enables Nix to work in single-user mode as non-root
    if [[ -d /nix ]] && [[ "$(stat -c '%u' /nix 2>/dev/null || echo 0)" != "$HOST_UID" ]]; then
        echo "Setting up Nix store permissions..." >&2
        chown -R "$HOST_UID:$HOST_GID" /nix 2>/dev/null || true
    fi
}

setup_npm_global() {
    # Create npm global directory and set ownership
    mkdir -p "$NPM_GLOBAL"
    chown -R "$HOST_UID:$HOST_GID" "$NPM_GLOBAL"
}

##############################################################################
# Main
##############################################################################

main() {
    # Setup user matching host UID/GID
    local target_user
    target_user=$(setup_user)

    # Setup Nix ownership and npm directory
    setup_nix_ownership
    setup_npm_global

    # Export environment variables for the user script
    export HOME=/home/claude
    export CLYDE_NIX_VERBOSE
    export CLYDE_NIX_GC
    export NPM_GLOBAL
    export CLYDE_PROJECT_FLAKE="${CLYDE_PROJECT_FLAKE:-}"
    export CLYDE_PROJECT_SHELL="${CLYDE_PROJECT_SHELL:-}"
    export CLYDE_USER_FLAKE="${CLYDE_USER_FLAKE:-}"
    export CLYDE_USER_SHELL="${CLYDE_USER_SHELL:-}"

    # Execute the user script as the target user
    exec gosu "$target_user" /docker/nix/user-init.sh "$@"
}

main "$@"
