#!/usr/bin/env bash
set -euo pipefail

# Clyde entrypoint: Create container user matching host UID/GID
# This ensures file permissions work correctly with mounted volumes

HOST_UID="${HOST_UID:-1000}"
HOST_GID="${HOST_GID:-1000}"

# Check if a user with the target UID already exists
existing_user=$(getent passwd "$HOST_UID" | cut -d: -f1 || true)

if [[ -n "$existing_user" ]]; then
    # User with this UID exists - use it directly
    # Create home directory for claude if it doesn't exist
    mkdir -p /home/claude
    chown "$HOST_UID:$HOST_GID" /home/claude

    # Only chown .claude directory (not read-only mounts like .ssh, .gitconfig)
    if [[ -d /home/claude/.claude ]]; then
        chown -R "$HOST_UID:$HOST_GID" /home/claude/.claude
    fi

    # Modify the existing user to use /home/claude as home
    usermod -d /home/claude "$existing_user" 2>/dev/null || true

    # Execute as the existing user
    exec gosu "$existing_user" "$@"
else
    # No user with this UID exists - create claude user

    # Get the group name for this GID, or create a new one
    existing_group=$(getent group "$HOST_GID" | cut -d: -f1 || true)
    if [[ -z "$existing_group" ]]; then
        groupadd -g "$HOST_GID" claude
        target_group="claude"
    else
        target_group="$existing_group"
    fi

    # Create user with matching UID and the target group
    useradd -u "$HOST_UID" -g "$target_group" -m -d /home/claude -s /bin/bash claude

    # Ensure home directory ownership is correct (not read-only mounts)
    chown "$HOST_UID:$HOST_GID" /home/claude
    if [[ -d /home/claude/.claude ]]; then
        chown -R "$HOST_UID:$HOST_GID" /home/claude/.claude
    fi

    # Execute as claude user
    exec gosu claude "$@"
fi
