#!/usr/bin/env bash
set -euo pipefail

# Browser setup script — called from user-init.sh when CLYDE_BROWSER=1
# Restores real agent-browser on PATH, symlinks config, and copies skill definition

##############################################################################
# Functions
##############################################################################

restore_agent_browser() {
    # The Dockerfile installs the disabled stub at /usr/local/bin/agent-browser
    # by default. When browser is enabled, we need to restore the real binary.
    # npm installed agent-browser to /usr/local/lib/node_modules/agent-browser/
    # Prefer the native Linux binary, fall back to the node.js wrapper
    local real_bin="/usr/local/lib/node_modules/agent-browser/bin/agent-browser-linux-x64"
    if [[ ! -f "$real_bin" ]]; then
        real_bin="/usr/local/lib/node_modules/agent-browser/bin/agent-browser.js"
    fi

    if [[ -f "$real_bin" ]]; then
        # Create a wrapper script in a user-writable location
        # Note: $HOME/.local/bin may be read-only (host mount), so use $HOME/.clyde/bin
        local wrapper_dir="$HOME/.clyde/bin"
        mkdir -p "$wrapper_dir"
        cat > "$wrapper_dir/agent-browser" <<WRAPPER
#!/usr/bin/env bash
exec "$real_bin" "\$@"
WRAPPER
        chmod +x "$wrapper_dir/agent-browser"
    else
        echo "Error: agent-browser is not installed in this image." >&2
        echo "Rebuild the image: docker build -t clyde:local docker/" >&2
        return 1
    fi
}

check_browser_cache() {
    local cache_dir="$HOME/.cache/ms-playwright"
    if [[ -d "$cache_dir" ]] && find "$cache_dir" -maxdepth 2 -name 'chrome' -type f 2>/dev/null | grep -q .; then
        echo "Browser engine cached, skipping download" >&2
        return 0
    fi
    echo "Browser engine not found in cache — using image-baked version" >&2
    return 0
}

symlink_config() {
    local config_src="/docker/browser/agent-browser.json"
    local config_dst="$PWD/agent-browser.json"

    if [[ -f "$config_src" ]] && [[ ! -f "$config_dst" ]]; then
        ln -sf "$config_src" "$config_dst"
    fi
}

copy_skill_definition() {
    local skill_src="/docker/skills/agent-browser"
    local skill_dst="$HOME/.claude/skills/agent-browser"

    mkdir -p "$skill_dst"
    if [[ -f "$skill_src/SKILL.md" ]]; then
        cp "$skill_src/SKILL.md" "$skill_dst/SKILL.md"
    fi
}

##############################################################################
# Main
##############################################################################

setup_browser() {
    restore_agent_browser
    check_browser_cache
    symlink_config
    copy_skill_definition
    echo "Browser support enabled." >&2
}

setup_browser
