#!/usr/bin/env bats
# Integration tests for Clyde container

CLYDE_SCRIPT="$BATS_TEST_DIRNAME/../../bin/clyde"
IMAGE_NAME="clyde:local"

# Skip all tests if Docker is not available
setup() {
    if ! docker info &>/dev/null; then
        skip "Docker not available"
    fi
    export TEST_TMPDIR="$(mktemp -d)"
}

teardown() {
    rm -rf "$TEST_TMPDIR"
}

##############################################################################
# Image Build Tests
##############################################################################

@test "Docker image can be built successfully" {
    local docker_dir="$BATS_TEST_DIRNAME/../../docker"
    run docker build -t "$IMAGE_NAME-test" "$docker_dir"
    [ "$status" -eq 0 ]
    # Cleanup
    docker rmi "$IMAGE_NAME-test" &>/dev/null || true
}

@test "Docker image contains tini" {
    # Build image first if needed
    local docker_dir="$BATS_TEST_DIRNAME/../../docker"
    docker build -t "$IMAGE_NAME" "$docker_dir" &>/dev/null || true

    run docker run --rm "$IMAGE_NAME" which tini
    [ "$status" -eq 0 ]
    [[ "$output" =~ "/usr/bin/tini" ]]
}

@test "Docker image contains gosu" {
    run docker run --rm "$IMAGE_NAME" which gosu
    [ "$status" -eq 0 ]
    [[ "$output" =~ "/usr/sbin/gosu" ]]
}

@test "Docker image contains node" {
    run docker run --rm "$IMAGE_NAME" node --version
    [ "$status" -eq 0 ]
    [[ "$output" =~ "v20" ]]
}

@test "Docker image contains claude command" {
    run docker run --rm "$IMAGE_NAME" which claude
    [ "$status" -eq 0 ]
}

##############################################################################
# UID/GID Matching Tests
##############################################################################

@test "Container creates user with matching UID" {
    local expected_uid=$(id -u)
    run docker run --rm -e "HOST_UID=$expected_uid" -e "HOST_GID=$(id -g)" "$IMAGE_NAME" id -u
    [ "$status" -eq 0 ]
    [ "$output" -eq "$expected_uid" ]
}

@test "Container creates user with matching GID" {
    local expected_gid=$(id -g)
    run docker run --rm -e "HOST_UID=$(id -u)" -e "HOST_GID=$expected_gid" "$IMAGE_NAME" id -g
    [ "$status" -eq 0 ]
    [ "$output" -eq "$expected_gid" ]
}

@test "Container user runs with matching UID" {
    # Verify the user inside container has the correct UID (may be named 'ubuntu' or 'claude')
    local expected_uid=$(id -u)
    run docker run --rm -e "HOST_UID=$expected_uid" -e "HOST_GID=$(id -g)" "$IMAGE_NAME" id -u
    [ "$status" -eq 0 ]
    [ "$output" -eq "$expected_uid" ]
}

@test "Container has writable home directory" {
    run docker run --rm -e "HOST_UID=$(id -u)" -e "HOST_GID=$(id -g)" "$IMAGE_NAME" sh -c 'touch /home/claude/testfile && rm /home/claude/testfile && echo success'
    [ "$status" -eq 0 ]
    [ "$output" = "success" ]
}

##############################################################################
# Volume Mount Tests
##############################################################################

@test "Mounted directory is accessible with correct permissions" {
    # Create a test file
    echo "test content" > "$TEST_TMPDIR/testfile.txt"
    chmod 644 "$TEST_TMPDIR/testfile.txt"

    run docker run --rm \
        -e "HOST_UID=$(id -u)" \
        -e "HOST_GID=$(id -g)" \
        -v "$TEST_TMPDIR:$TEST_TMPDIR" \
        "$IMAGE_NAME" cat "$TEST_TMPDIR/testfile.txt"

    [ "$status" -eq 0 ]
    [ "$output" = "test content" ]
}

@test "Container can write to mounted directory" {
    run docker run --rm \
        -e "HOST_UID=$(id -u)" \
        -e "HOST_GID=$(id -g)" \
        -v "$TEST_TMPDIR:$TEST_TMPDIR" \
        "$IMAGE_NAME" sh -c "echo 'written from container' > $TEST_TMPDIR/container-file.txt"

    [ "$status" -eq 0 ]
    [ -f "$TEST_TMPDIR/container-file.txt" ]
    [ "$(cat "$TEST_TMPDIR/container-file.txt")" = "written from container" ]
}

@test "File created in container has correct ownership" {
    docker run --rm \
        -e "HOST_UID=$(id -u)" \
        -e "HOST_GID=$(id -g)" \
        -v "$TEST_TMPDIR:$TEST_TMPDIR" \
        "$IMAGE_NAME" touch "$TEST_TMPDIR/owned-file.txt"

    local file_uid=$(stat -c %u "$TEST_TMPDIR/owned-file.txt")
    local file_gid=$(stat -c %g "$TEST_TMPDIR/owned-file.txt")

    [ "$file_uid" -eq "$(id -u)" ]
    [ "$file_gid" -eq "$(id -g)" ]
}

##############################################################################
# Container Cleanup Tests
##############################################################################

@test "Container is removed after exit with --rm flag" {
    local container_name="clyde-test-cleanup-$$"

    # Run container with a name and --rm
    docker run --rm --name "$container_name" \
        -e "HOST_UID=$(id -u)" \
        -e "HOST_GID=$(id -g)" \
        "$IMAGE_NAME" echo "test"

    # Verify container no longer exists
    run docker ps -a --filter "name=$container_name" --format '{{.Names}}'
    [ -z "$output" ]
}
