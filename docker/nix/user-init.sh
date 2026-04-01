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

    # Add Claude local bin to PATH
    export PATH="${CLAUDE_LOCAL}/bin:$PATH"
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

    # Use browser devShell when browser mode is enabled and using default config
    local nix_devshell=""
    if [[ "${CLYDE_BROWSER:-}" == "1" ]] && [[ "$config_type" == "default" ]]; then
        nix_devshell="browser"
    fi

    echo "Loading $config_type environment..." >&2

    # For non-default configs, first get the base environment PATH
    # This ensures nodejs (required for claude) is always available
    local base_path=""
    if [[ "$config_type" != "default" ]]; then
        local base_ref="/docker/nix"
        if [[ -n "$nix_devshell" ]]; then
            base_ref="/docker/nix#${nix_devshell}"
        fi
        echo "Including base packages (nodejs, git, gh)..." >&2
        base_path=$(nix develop "$base_ref" --quiet --command bash -c 'echo $PATH' 2>/dev/null) || true
    fi

    # Build the nix command
    local nix_cmd
    local nix_args=()

    # Build the inner script that runs inside nix develop
    # Include base_path to ensure nodejs is available for claude
    # Build browser PATH prefix if browser is enabled
    local browser_path_prefix=""
    if [[ "${CLYDE_BROWSER:-}" == "1" ]]; then
        browser_path_prefix="$HOME/.clyde/bin:"
    fi

    local inner_script
    inner_script=$(cat <<INNER
# Add base packages to PATH (ensures nodejs is available for claude)
if [[ -n "$base_path" ]]; then
    export PATH="$base_path:\$PATH"
fi
# Add Claude local bin to PATH
export PATH="${browser_path_prefix}${CLAUDE_LOCAL}/bin:\$PATH"
# Install/update Claude Code via native installer
# Runs every startup to ensure latest version (fast when already current)
echo "Updating Claude Code..." >&2
curl -fsSL https://claude.ai/install.sh | bash 2>&1
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
        if [[ -n "$nix_devshell" ]]; then
            nix_args+=("develop" "${nix_config}#${nix_devshell}")
        else
            nix_args+=("develop" "$nix_config")
        fi

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

##############################################################################
# Browser Setup
##############################################################################

setup_browser() {
    if [[ "${CLYDE_BROWSER:-}" == "1" ]]; then
        # shellcheck source=/dev/null
        source /docker/browser/setup-browser.sh
        # Ensure the restored wrapper takes precedence over the disabled stub
        export PATH="$HOME/.clyde/bin:$PATH"
    fi
    # When CLYDE_BROWSER is not set, the Dockerfile-installed stub at
    # /usr/local/bin/agent-browser provides a clear "not enabled" message
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

    # Conditionally activate browser support
    setup_browser

    # Activate environment and run command
    activate_environment "$@"
}

main "$@"
