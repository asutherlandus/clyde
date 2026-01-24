#!/usr/bin/env bash
set -euo pipefail

# Clyde entrypoint: Create container user matching host UID/GID
# This ensures file permissions work correctly with mounted volumes

HOST_UID="${HOST_UID:-1000}"
HOST_GID="${HOST_GID:-1000}"

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

# Check if a user with the target UID already exists
existing_user=$(getent passwd "$HOST_UID" | cut -d: -f1 || true)

if [[ -n "$existing_user" ]]; then
    # User with this UID exists - use it directly
    # Create home directory for claude if it doesn't exist
    mkdir -p /home/claude
    chown "$HOST_UID:$HOST_GID" /home/claude

    # Only chown .claude directory (not read-only mounts like .gitconfig)
    if [[ -d /home/claude/.claude ]]; then
        chown -R "$HOST_UID:$HOST_GID" /home/claude/.claude
    fi

    # Modify the existing user to use /home/claude as home
    usermod -d /home/claude "$existing_user" 2>/dev/null || true

    # Execute as the existing user with proper HOME
    export HOME=/home/claude
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

    # Execute as claude user with proper HOME
    export HOME=/home/claude
    exec gosu claude "$@"
fi
