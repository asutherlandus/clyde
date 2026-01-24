#!/usr/bin/env bats
# Unit tests for clyde launch script

CLYDE_SCRIPT="$BATS_TEST_DIRNAME/../../bin/clyde"

# Helper: Create a mock docker command
setup() {
    export PATH="$BATS_TEST_DIRNAME/mocks:$PATH"
    export TEST_TMPDIR="$(mktemp -d)"
}

teardown() {
    rm -rf "$TEST_TMPDIR"
}

##############################################################################
# Argument Parsing Tests
##############################################################################

@test "clyde --help displays usage and exits 0" {
    run "$CLYDE_SCRIPT" --help
    [ "$status" -eq 0 ]
    [[ "$output" =~ "Usage: clyde" ]]
}

@test "clyde -h displays usage and exits 0" {
    run "$CLYDE_SCRIPT" -h
    [ "$status" -eq 0 ]
    [[ "$output" =~ "Usage: clyde" ]]
}

@test "clyde --version displays version and exits 0" {
    run "$CLYDE_SCRIPT" --version
    [ "$status" -eq 0 ]
    [[ "$output" =~ "clyde version" ]]
}

@test "clyde -v displays version and exits 0" {
    run "$CLYDE_SCRIPT" -v
    [ "$status" -eq 0 ]
    [[ "$output" =~ "clyde version" ]]
}

@test "clyde with unknown option exits with code 4" {
    # Mock docker to return success
    mkdir -p "$TEST_TMPDIR/mocks"
    cat > "$TEST_TMPDIR/mocks/docker" <<'EOF'
#!/bin/bash
exit 0
EOF
    chmod +x "$TEST_TMPDIR/mocks/docker"
    export PATH="$TEST_TMPDIR/mocks:$PATH"

    run "$CLYDE_SCRIPT" --unknown-option
    [ "$status" -eq 4 ]
    [[ "$output" =~ "Unknown option" ]]
}

@test "clyde --memory without value exits with code 4" {
    mkdir -p "$TEST_TMPDIR/mocks"
    cat > "$TEST_TMPDIR/mocks/docker" <<'EOF'
#!/bin/bash
exit 0
EOF
    chmod +x "$TEST_TMPDIR/mocks/docker"
    export PATH="$TEST_TMPDIR/mocks:$PATH"

    run "$CLYDE_SCRIPT" --memory
    [ "$status" -eq 4 ]
    [[ "$output" =~ "requires a value" ]]
}

@test "clyde --cpus without value exits with code 4" {
    mkdir -p "$TEST_TMPDIR/mocks"
    cat > "$TEST_TMPDIR/mocks/docker" <<'EOF'
#!/bin/bash
exit 0
EOF
    chmod +x "$TEST_TMPDIR/mocks/docker"
    export PATH="$TEST_TMPDIR/mocks:$PATH"

    run "$CLYDE_SCRIPT" --cpus
    [ "$status" -eq 4 ]
    [[ "$output" =~ "requires a value" ]]
}

##############################################################################
# Exit Code Tests
##############################################################################

@test "clyde exits with code 2 when Docker is not running" {
    # Create mock docker that fails on 'info'
    mkdir -p "$TEST_TMPDIR/mocks"
    cat > "$TEST_TMPDIR/mocks/docker" <<'EOF'
#!/bin/bash
if [[ "$1" == "info" ]]; then
    exit 1
fi
exit 0
EOF
    chmod +x "$TEST_TMPDIR/mocks/docker"
    export PATH="$TEST_TMPDIR/mocks:$PATH"

    run "$CLYDE_SCRIPT"
    [ "$status" -eq 2 ]
    [[ "$output" =~ "Docker is not running" ]]
}

@test "clyde exits with code 5 when run from root filesystem" {
    # Create mock docker that succeeds
    mkdir -p "$TEST_TMPDIR/mocks"
    cat > "$TEST_TMPDIR/mocks/docker" <<'EOF'
#!/bin/bash
exit 0
EOF
    chmod +x "$TEST_TMPDIR/mocks/docker"
    export PATH="$TEST_TMPDIR/mocks:$PATH"

    # Use a subshell to change PWD
    run bash -c "cd / && export PATH=\"$TEST_TMPDIR/mocks:\$PATH\" && \"$CLYDE_SCRIPT\""
    [ "$status" -eq 5 ]
    [[ "$output" =~ "Cannot run clyde from root filesystem" ]]
}

##############################################################################
# Help Content Validation
##############################################################################

@test "help includes --memory option" {
    run "$CLYDE_SCRIPT" --help
    [[ "$output" =~ "--memory" ]]
}

@test "help includes --cpus option" {
    run "$CLYDE_SCRIPT" --help
    [[ "$output" =~ "--cpus" ]]
}

@test "help includes --build option" {
    run "$CLYDE_SCRIPT" --help
    [[ "$output" =~ "--build" ]]
}

@test "help includes --no-git option" {
    run "$CLYDE_SCRIPT" --help
    [[ "$output" =~ "--no-git" ]]
}

@test "help includes CLYDE_MEMORY environment variable" {
    run "$CLYDE_SCRIPT" --help
    [[ "$output" =~ "CLYDE_MEMORY" ]]
}

@test "help includes CLYDE_CPUS environment variable" {
    run "$CLYDE_SCRIPT" --help
    [[ "$output" =~ "CLYDE_CPUS" ]]
}
