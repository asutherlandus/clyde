#!/usr/bin/env bats
# Integration tests for Clyde browser support

CLYDE_SCRIPT="$BATS_TEST_DIRNAME/../../bin/clyde"
IMAGE_NAME="clyde:local"

# Skip all tests if Docker is not available or image not built
setup() {
    if ! docker info &>/dev/null; then
        skip "Docker not available"
    fi
    if ! docker image inspect "$IMAGE_NAME" &>/dev/null; then
        skip "Clyde image not built (run: docker build -t clyde:local docker/)"
    fi
    export TEST_TMPDIR="$(mktemp -d)"
}

teardown() {
    rm -rf "$TEST_TMPDIR"
}

##############################################################################
# Browser Enabled Tests
##############################################################################

@test "container with CLYDE_BROWSER=1 has agent-browser on PATH" {
    run docker run --rm \
        -e "HOST_UID=$(id -u)" \
        -e "HOST_GID=$(id -g)" \
        -e "CLYDE_BROWSER=1" \
        "$IMAGE_NAME" which agent-browser

    [ "$status" -eq 0 ]
    [[ "$output" =~ "agent-browser" ]]
}

@test "container without CLYDE_BROWSER runs stub that prints not enabled message" {
    run docker run --rm \
        -e "HOST_UID=$(id -u)" \
        -e "HOST_GID=$(id -g)" \
        "$IMAGE_NAME" agent-browser --version

    [ "$status" -ne 0 ]
    [[ "$output" =~ "not enabled" ]]
}

@test "agent-browser --version returns successfully inside container" {
    run docker run --rm \
        -e "HOST_UID=$(id -u)" \
        -e "HOST_GID=$(id -g)" \
        -e "CLYDE_BROWSER=1" \
        "$IMAGE_NAME" agent-browser --version

    [ "$status" -eq 0 ]
}

@test "agent-browser open and snapshot returns content" {
    run docker run --rm \
        -e "HOST_UID=$(id -u)" \
        -e "HOST_GID=$(id -g)" \
        -e "CLYDE_BROWSER=1" \
        --network host \
        "$IMAGE_NAME" bash -c 'agent-browser open https://example.com && agent-browser snapshot'

    [ "$status" -eq 0 ]
    [[ "$output" =~ "Example Domain" ]]
}

@test "browser cache volume is mounted at correct path" {
    run docker run --rm \
        -e "HOST_UID=$(id -u)" \
        -e "HOST_GID=$(id -g)" \
        -e "CLYDE_BROWSER=1" \
        -v "clyde-browser-cache:/home/claude/.cache/ms-playwright" \
        "$IMAGE_NAME" sh -c 'test -d /home/claude/.cache/ms-playwright && echo "CACHE_MOUNT_OK"'

    [ "$status" -eq 0 ]
    [[ "$output" =~ "CACHE_MOUNT_OK" ]]
}

@test "agent-browser connection error produces clear message" {
    run docker run --rm \
        -e "HOST_UID=$(id -u)" \
        -e "HOST_GID=$(id -g)" \
        -e "CLYDE_BROWSER=1" \
        "$IMAGE_NAME" bash -c 'agent-browser open https://localhost:99999 2>&1 || true'

    # Should get an error (connection refused or similar), not a crash
    [[ "$output" =~ "ERR" ]] || [[ "$output" =~ "error" ]] || [[ "$output" =~ "Error" ]] || [[ "$output" =~ "refused" ]] || [[ "$output" =~ "failed" ]]
}

@test "no zombie Chrome processes after agent-browser close" {
    run docker run --rm \
        -e "HOST_UID=$(id -u)" \
        -e "HOST_GID=$(id -g)" \
        -e "CLYDE_BROWSER=1" \
        "$IMAGE_NAME" bash -c '
            agent-browser open https://example.com &>/dev/null
            agent-browser close &>/dev/null
            sleep 1
            # Count chrome/chromium processes (should be 0 after close)
            chrome_procs=$(pgrep -c chrome 2>/dev/null || echo 0)
            echo "CHROME_PROCS=$chrome_procs"
        '

    [ "$status" -eq 0 ]
    [[ "$output" =~ "CHROME_PROCS=0" ]]
}

##############################################################################
# Browser Config Tests
##############################################################################

@test "browser config file exists in container" {
    run docker run --rm \
        -e "HOST_UID=$(id -u)" \
        -e "HOST_GID=$(id -g)" \
        "$IMAGE_NAME" sh -c 'test -f /docker/browser/agent-browser.json && echo "CONFIG_OK"'

    [ "$status" -eq 0 ]
    [ "$output" = "CONFIG_OK" ]
}

@test "skill definition exists in container" {
    run docker run --rm \
        -e "HOST_UID=$(id -u)" \
        -e "HOST_GID=$(id -g)" \
        "$IMAGE_NAME" sh -c 'test -f /docker/skills/agent-browser/SKILL.md && echo "SKILL_OK"'

    [ "$status" -eq 0 ]
    [ "$output" = "SKILL_OK" ]
}
