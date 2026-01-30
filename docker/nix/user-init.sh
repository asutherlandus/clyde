#!/usr/bin/env bash
set -euo pipefail

# User initialization script - runs as the non-root user
# Activates Nix environment and installs/updates Claude Code

##############################################################################
# Progress Display
##############################################################################

show_nix_progress() {
    local line="$1"
    # Extract package name from Nix store path for user-friendly output
    if [[ $line =~ /nix/store/[^-]+-([^/\ ]+) ]]; then
        local pkg="${BASH_REMATCH[1]}"
        echo "  Fetching $pkg..." >&2
    fi
}

##############################################################################
# Nix Environment Setup
##############################################################################

setup_nix_env() {
    # Set USER variable if not set (required by Nix profile script)
    export USER="${USER:-$(whoami)}"

    # Add Nix to PATH directly (the profile sourcing may not work correctly after chown)
    export PATH="/nix/var/nix/profiles/default/bin:$PATH"

    # Source Nix profile for any additional environment setup
    if [[ -f /nix/var/nix/profiles/default/etc/profile.d/nix.sh ]]; then
        # shellcheck source=/dev/null
        source /nix/var/nix/profiles/default/etc/profile.d/nix.sh 2>/dev/null || true
    elif [[ -f "$HOME/.nix-profile/etc/profile.d/nix.sh" ]]; then
        # shellcheck source=/dev/null
        source "$HOME/.nix-profile/etc/profile.d/nix.sh" 2>/dev/null || true
    fi

    # Add npm global to PATH
    export PATH="${NPM_GLOBAL}/bin:$PATH"
    export npm_config_prefix="$NPM_GLOBAL"
}

##############################################################################
# Garbage Collection
##############################################################################

run_garbage_collection() {
    echo "Running Nix garbage collection..." >&2
    nix-collect-garbage -d
    echo "Garbage collection complete." >&2
    exit 0
}

##############################################################################
# Environment Activation
##############################################################################

activate_environment() {
    # Determine which Nix configuration to use (priority: project > user > base)
    local nix_config=""
    local config_type=""

    if [[ -n "${CLYDE_PROJECT_FLAKE:-}" ]] && [[ -f "$CLYDE_PROJECT_FLAKE/flake.nix" ]]; then
        nix_config="$CLYDE_PROJECT_FLAKE"
        config_type="project flake"
    elif [[ -n "${CLYDE_PROJECT_SHELL:-}" ]] && [[ -f "$CLYDE_PROJECT_SHELL" ]]; then
        nix_config="$CLYDE_PROJECT_SHELL"
        config_type="project shell.nix"
    elif [[ -n "${CLYDE_USER_FLAKE:-}" ]] && [[ -f "$CLYDE_USER_FLAKE/flake.nix" ]]; then
        nix_config="$CLYDE_USER_FLAKE"
        config_type="user flake"
    elif [[ -n "${CLYDE_USER_SHELL:-}" ]] && [[ -f "$CLYDE_USER_SHELL" ]]; then
        nix_config="$CLYDE_USER_SHELL"
        config_type="user shell.nix"
    else
        nix_config="/docker/nix"
        config_type="default"
    fi

    echo "Loading $config_type environment..." >&2

    # Build the nix command
    local nix_cmd
    local nix_args=()

    if [[ "$config_type" == *"shell.nix"* ]]; then
        nix_cmd="nix-shell"
        nix_args+=("$nix_config")
        nix_args+=("--run" "exec bash -c 'install_claude_and_run \"\$@\"' -- \"\$@\"")
    else
        nix_cmd="nix"
        nix_args+=("develop" "$nix_config")

        # Add verbosity flags
        if [[ "${CLYDE_NIX_VERBOSE:-0}" == "1" ]]; then
            nix_args+=("-vv")
        else
            nix_args+=("--quiet")
        fi

        nix_args+=("--command" "bash" "-c" 'install_claude_and_run "$@"' "--")
    fi

    # Export the function for use in subshell
    export -f install_claude_and_run

    # Execute Nix environment
    exec "$nix_cmd" "${nix_args[@]}" "$@"
}

##############################################################################
# Claude Code Installation
##############################################################################

install_claude_and_run() {
    # Install/update Claude Code via npm (cached in volume)
    echo "Checking Claude Code..." >&2
    if ! command -v claude &>/dev/null; then
        echo "Installing Claude Code..." >&2
        npm install -g @anthropic-ai/claude-code 2>&1 | grep -v "^npm" || true
    fi

    echo "Environment ready!" >&2

    # Execute the requested command
    exec "$@"
}

##############################################################################
# Main
##############################################################################

main() {
    # Setup Nix environment
    setup_nix_env

    # Handle garbage collection request
    if [[ "${CLYDE_NIX_GC:-0}" == "1" ]]; then
        run_garbage_collection
    fi

    # Activate environment and run command
    activate_environment "$@"
}

main "$@"
