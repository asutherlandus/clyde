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

    # For non-default configs, first get the base environment PATH
    # This ensures nodejs (required for claude) is always available
    local base_path=""
    if [[ "$config_type" != "default" ]]; then
        echo "Including base packages (nodejs, git, gh)..." >&2
        base_path=$(nix develop /docker/nix --quiet --command bash -c 'echo $PATH' 2>/dev/null) || true
    fi

    # Build the nix command
    local nix_cmd
    local nix_args=()

    # Build the inner script that runs inside nix develop
    # Include base_path to ensure nodejs is available for claude
    local inner_script
    inner_script=$(cat <<INNER
# Add base packages to PATH (ensures nodejs is available for claude)
if [[ -n "$base_path" ]]; then
    export PATH="$base_path:\$PATH"
fi
# Add npm global to PATH
export PATH="${NPM_GLOBAL}/bin:\$PATH"
# Install/update Claude Code via npm (cached in volume)
# Always run npm install to ensure latest version
echo "Updating Claude Code..." >&2
npm install -g @anthropic-ai/claude-code 2>&1 | grep -v "^npm" || true
echo "Environment ready!" >&2
exec "\$@"
INNER
)

    if [[ "$config_type" == *"shell.nix"* ]]; then
        nix_cmd="nix-shell"
        nix_args+=("$nix_config")
        nix_args+=("--run" "bash -c '$inner_script' -- \"\$@\"")
    else
        nix_cmd="nix"
        nix_args+=("develop" "$nix_config")

        # Add verbosity flags
        if [[ "${CLYDE_NIX_VERBOSE:-0}" == "1" ]]; then
            nix_args+=("-vv")
        else
            nix_args+=("--quiet")
        fi

        nix_args+=("--command" "bash" "-c" "$inner_script" "--")
    fi

    # Execute Nix environment
    exec "$nix_cmd" "${nix_args[@]}" "$@"
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
