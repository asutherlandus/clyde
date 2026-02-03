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

##############################################################################
# Nix-Related Flag Tests
##############################################################################

@test "help includes --nix-verbose option" {
    run "$CLYDE_SCRIPT" --help
    [[ "$output" =~ "--nix-verbose" ]]
}

@test "help includes --nix-gc option" {
    run "$CLYDE_SCRIPT" --help
    [[ "$output" =~ "--nix-gc" ]]
}

@test "help includes --list-packages option" {
    run "$CLYDE_SCRIPT" --help
    [[ "$output" =~ "--list-packages" ]]
}

@test "help includes CLYDE_NIX_VERBOSE environment variable" {
    run "$CLYDE_SCRIPT" --help
    [[ "$output" =~ "CLYDE_NIX_VERBOSE" ]]
}

@test "help includes CLYDE_NIX_STORE_VOLUME environment variable" {
    run "$CLYDE_SCRIPT" --help
    [[ "$output" =~ "CLYDE_NIX_STORE_VOLUME" ]]
}

@test "clyde --list-packages shows default packages" {
    run "$CLYDE_SCRIPT" --list-packages
    [ "$status" -eq 0 ]
    [[ "$output" =~ "Default packages" ]]
    [[ "$output" =~ "claude" ]]
    [[ "$output" =~ "git" ]]
    [[ "$output" =~ "node" ]]
}

@test "clyde --list-packages detects project flake.nix" {
    # Create a temp directory with a flake.nix
    mkdir -p "$TEST_TMPDIR/project"
    echo '{}' > "$TEST_TMPDIR/project/flake.nix"

    run bash -c "cd \"$TEST_TMPDIR/project\" && \"$CLYDE_SCRIPT\" --list-packages"
    [ "$status" -eq 0 ]
    [[ "$output" =~ "Project packages" ]]
    [[ "$output" =~ "flake.nix" ]]
}

@test "clyde --list-packages detects project shell.nix" {
    # Create a temp directory with a shell.nix
    mkdir -p "$TEST_TMPDIR/project"
    echo '{}' > "$TEST_TMPDIR/project/shell.nix"

    run bash -c "cd \"$TEST_TMPDIR/project\" && \"$CLYDE_SCRIPT\" --list-packages"
    [ "$status" -eq 0 ]
    [[ "$output" =~ "Project packages" ]]
    [[ "$output" =~ "shell.nix" ]]
}

##############################################################################
# Shell Mode Tests
##############################################################################

@test "clyde --shell flag is parsed correctly" {
    # Create mock docker that captures arguments
    mkdir -p "$TEST_TMPDIR/mocks"
    cat > "$TEST_TMPDIR/mocks/docker" <<'EOF'
#!/bin/bash
if [[ "$1" == "info" ]] || [[ "$1" == "image" ]]; then
    exit 0
fi
# Echo the last argument to verify it's 'bash'
echo "CMD=${*: -1}"
exit 0
EOF
    chmod +x "$TEST_TMPDIR/mocks/docker"
    export PATH="$TEST_TMPDIR/mocks:$PATH"

    # Create a temp project directory
    mkdir -p "$TEST_TMPDIR/project"

    run bash -c "cd \"$TEST_TMPDIR/project\" && \"$CLYDE_SCRIPT\" --shell"
    [ "$status" -eq 0 ]
    [[ "$output" =~ "CMD=bash" ]]
}

@test "help includes --shell option" {
    run "$CLYDE_SCRIPT" --help
    [[ "$output" =~ "--shell" ]]
    [[ "$output" =~ "Launch interactive bash shell" ]]
}

@test "help includes CLYDE_SHELL environment variable" {
    run "$CLYDE_SCRIPT" --help
    [[ "$output" =~ "CLYDE_SHELL" ]]
}

##############################################################################
# X11 Mode Tests
##############################################################################

@test "clyde --x11 flag is parsed correctly" {
    # Create mock docker that captures arguments
    mkdir -p "$TEST_TMPDIR/mocks"
    cat > "$TEST_TMPDIR/mocks/docker" <<'EOF'
#!/bin/bash
if [[ "$1" == "info" ]] || [[ "$1" == "image" ]]; then
    exit 0
fi
# Echo all args to check for X11 related
echo "ARGS=$*"
exit 0
EOF
    chmod +x "$TEST_TMPDIR/mocks/docker"
    export PATH="$TEST_TMPDIR/mocks:$PATH"

    # Create a temp project directory
    mkdir -p "$TEST_TMPDIR/project"

    # Set DISPLAY to avoid validation error
    DISPLAY=:0 run bash -c "cd \"$TEST_TMPDIR/project\" && \"$CLYDE_SCRIPT\" --x11"
    [ "$status" -eq 0 ]
    [[ "$output" =~ "/tmp/.X11-unix" ]]
    [[ "$output" =~ "DISPLAY" ]]
}

@test "clyde --x11 fails when DISPLAY is unset" {
    # Create mock docker
    mkdir -p "$TEST_TMPDIR/mocks"
    cat > "$TEST_TMPDIR/mocks/docker" <<'EOF'
#!/bin/bash
exit 0
EOF
    chmod +x "$TEST_TMPDIR/mocks/docker"
    export PATH="$TEST_TMPDIR/mocks:$PATH"

    # Create a temp project directory
    mkdir -p "$TEST_TMPDIR/project"

    # Unset DISPLAY and run with --x11
    run bash -c "cd \"$TEST_TMPDIR/project\" && unset DISPLAY && \"$CLYDE_SCRIPT\" --x11"
    [ "$status" -eq 7 ]
    [[ "$output" =~ "DISPLAY is not set" ]]
}

@test "clyde --x11 mounts host fonts when available" {
    # Create mock docker that captures arguments
    mkdir -p "$TEST_TMPDIR/mocks"
    cat > "$TEST_TMPDIR/mocks/docker" <<'EOF'
#!/bin/bash
if [[ "$1" == "info" ]] || [[ "$1" == "image" ]]; then
    exit 0
fi
# Echo all args to check for font mounts
echo "ARGS=$*"
exit 0
EOF
    chmod +x "$TEST_TMPDIR/mocks/docker"
    export PATH="$TEST_TMPDIR/mocks:$PATH"

    # Create a temp project directory
    mkdir -p "$TEST_TMPDIR/project"

    # Set DISPLAY and run with --x11
    DISPLAY=:0 run bash -c "cd \"$TEST_TMPDIR/project\" && \"$CLYDE_SCRIPT\" --x11"
    [ "$status" -eq 0 ]
    # Check that font directory mount is attempted (if /usr/share/fonts exists on host)
    if [ -d "/usr/share/fonts" ]; then
        [[ "$output" =~ "/usr/share/fonts" ]]
    fi
}

@test "help includes --x11 option" {
    run "$CLYDE_SCRIPT" --help
    [[ "$output" =~ "--x11" ]]
    [[ "$output" =~ "X11 forwarding" ]]
}

@test "help includes CLYDE_X11 environment variable" {
    run "$CLYDE_SCRIPT" --help
    [[ "$output" =~ "CLYDE_X11" ]]
}

##############################################################################
# Exec Mode Tests
##############################################################################

@test "clyde --exec flag is parsed correctly" {
    # Create mock docker that captures arguments
    mkdir -p "$TEST_TMPDIR/mocks"
    cat > "$TEST_TMPDIR/mocks/docker" <<'EOF'
#!/bin/bash
if [[ "$1" == "info" ]] || [[ "$1" == "image" ]]; then
    exit 0
fi
# Echo all args to see the command
echo "ARGS=$*"
exit 0
EOF
    chmod +x "$TEST_TMPDIR/mocks/docker"
    export PATH="$TEST_TMPDIR/mocks:$PATH"

    # Create a temp project directory
    mkdir -p "$TEST_TMPDIR/project"

    run bash -c "cd \"$TEST_TMPDIR/project\" && \"$CLYDE_SCRIPT\" --exec cargo test"
    [ "$status" -eq 0 ]
    [[ "$output" =~ "cargo test" ]]
}

@test "clyde --exec captures remaining arguments" {
    # Create mock docker that captures arguments
    mkdir -p "$TEST_TMPDIR/mocks"
    cat > "$TEST_TMPDIR/mocks/docker" <<'EOF'
#!/bin/bash
if [[ "$1" == "info" ]] || [[ "$1" == "image" ]]; then
    exit 0
fi
# Echo all args to see the full command
echo "ARGS=$*"
exit 0
EOF
    chmod +x "$TEST_TMPDIR/mocks/docker"
    export PATH="$TEST_TMPDIR/mocks:$PATH"

    # Create a temp project directory
    mkdir -p "$TEST_TMPDIR/project"

    run bash -c "cd \"$TEST_TMPDIR/project\" && \"$CLYDE_SCRIPT\" --exec npm run build --production"
    [ "$status" -eq 0 ]
    [[ "$output" =~ "npm run build --production" ]]
}

@test "clyde --shell --exec fails with mutual exclusivity error" {
    # Create mock docker
    mkdir -p "$TEST_TMPDIR/mocks"
    cat > "$TEST_TMPDIR/mocks/docker" <<'EOF'
#!/bin/bash
exit 0
EOF
    chmod +x "$TEST_TMPDIR/mocks/docker"
    export PATH="$TEST_TMPDIR/mocks:$PATH"

    run "$CLYDE_SCRIPT" --shell --exec echo test
    [ "$status" -eq 4 ]
    [[ "$output" =~ "mutually exclusive" ]]
}

@test "clyde --exec without command exits with error" {
    # Create mock docker
    mkdir -p "$TEST_TMPDIR/mocks"
    cat > "$TEST_TMPDIR/mocks/docker" <<'EOF'
#!/bin/bash
exit 0
EOF
    chmod +x "$TEST_TMPDIR/mocks/docker"
    export PATH="$TEST_TMPDIR/mocks:$PATH"

    run "$CLYDE_SCRIPT" --exec
    [ "$status" -eq 4 ]
    [[ "$output" =~ "requires a command" ]]
}

@test "help includes --exec option" {
    run "$CLYDE_SCRIPT" --help
    [[ "$output" =~ "--exec" ]]
    [[ "$output" =~ "Execute a command" ]]
}
