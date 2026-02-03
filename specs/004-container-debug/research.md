# Research: Container Debugging Options

**Feature**: 004-container-debug
**Date**: 2026-02-03

## Research Topics

### 1. X11 Forwarding in Docker Containers

**Decision**: Mount X11 Unix socket directly with DISPLAY passthrough

**Rationale**:
- Direct socket mounting (`/tmp/.X11-unix`) is the standard approach for local X11 forwarding
- Simpler than network-based X11 (no need for `xhost +` TCP access)
- Works with Wayland hosts running XWayland (common on modern Linux)
- No additional packages needed in container for basic X11 clients

**Alternatives Considered**:
| Alternative | Rejected Because |
|-------------|------------------|
| X11 over TCP | Requires `xhost +` modification and has higher security exposure |
| VNC/remote desktop | Overkill for debugging; adds container complexity |
| Wayland native | Not universally supported; X11 still dominant for dev tools |

**Implementation Details**:
```bash
# Docker args for X11 forwarding
docker_args+=(-v "/tmp/.X11-unix:/tmp/.X11-unix:rw")
docker_args+=(-e "DISPLAY=$DISPLAY")
```

**Security Note**: The X11 socket gives container processes ability to read keystrokes and screen content from other X11 applications. Document in help text per spec clarification.

---

### 2. Shell-Only Mode Implementation

**Decision**: Pass `bash` as command instead of `claude --dangerously-skip-permissions`

**Rationale**:
- Docker CMD can be overridden at runtime - no Dockerfile changes needed
- The `user-init.sh` script already handles arbitrary commands via `exec "$@"`
- Shell inherits the same Nix-activated environment as Claude would

**Alternatives Considered**:
| Alternative | Rejected Because |
|-------------|------------------|
| Separate shell entrypoint | Duplicates environment setup logic; maintenance burden |
| Skip Nix activation for shell | Violates environment parity requirement (FR-002) |
| Add shell flag to entrypoint | Unnecessary; Docker command override is sufficient |

**Implementation Details**:
```bash
# In bin/clyde, when SHELL_MODE=true
# Replace default CMD with bash
if [[ "$SHELL_MODE" == true ]]; then
    docker_args+=(bash)
else
    # Default: pass CLAUDE_ARGS to container
    exec docker run "${docker_args[@]}" "$IMAGE_NAME" "${CLAUDE_ARGS[@]}"
fi
```

---

### 3. Exec Mode Implementation

**Decision**: Pass command array directly to container, replacing Claude invocation

**Rationale**:
- Same mechanism as shell mode but with user-specified command
- Enables CI/scripted workflows without interactive shell
- Command is passed to `user-init.sh` which activates Nix then execs it

**Alternatives Considered**:
| Alternative | Rejected Because |
|-------------|------------------|
| Run command inside running shell | Adds complexity; defeats purpose of "same environment" |
| Separate --run flag | `--exec` is clearer semantic meaning |

**Implementation Details**:
```bash
# In bin/clyde, when EXEC_COMMAND is set
# EXEC_COMMAND contains full command as array
exec docker run "${docker_args[@]}" "$IMAGE_NAME" "${EXEC_COMMAND[@]}"
```

**Edge Case**: `--exec` must capture all arguments after it as the command:
- `clyde --exec cargo test` → command is `cargo test`
- `clyde --x11 --exec make gui` → X11 enabled, command is `make gui`

---

### 4. Argument Parsing Order

**Decision**: Process `--shell` and `--exec` as mutually exclusive; `--exec` captures remaining args

**Rationale**:
- Clear semantics: `--shell` = interactive, `--exec` = single command
- `--exec` must be last flag before command arguments
- Error if both `--shell` and `--exec` specified

**Implementation Pattern**:
```bash
# In parse_args()
--shell)
    SHELL_MODE=true
    shift
    ;;
--exec)
    shift
    if [[ $# -eq 0 ]]; then
        error "Option --exec requires a command" 4
    fi
    EXEC_COMMAND=("$@")
    break  # Stop parsing; rest is the command
    ;;
```

---

### 5. Environment Variable Support

**Decision**: Add `CLYDE_SHELL` and `CLYDE_X11` environment variables

**Rationale**:
- Consistent with existing pattern (`CLYDE_MEMORY`, `CLYDE_CPUS`, etc.)
- Enables persistent configuration via shell profile
- Flag overrides environment variable (explicit > implicit)

**Implementation**:
```bash
# At top of script
SHELL_MODE="${CLYDE_SHELL:-false}"
X11_ENABLED="${CLYDE_X11:-false}"

# In parse_args(), flags override env vars
--shell)
    SHELL_MODE=true
    ;;
--x11)
    X11_ENABLED=true
    ;;
```

---

### 6. DISPLAY Validation

**Decision**: Validate DISPLAY is set and non-empty when `--x11` requested

**Rationale**:
- Fail fast with clear error rather than cryptic X11 connection failures
- Allow users on headless servers with SSH X11 forwarding (DISPLAY will be set)

**Implementation**:
```bash
validate_x11() {
    if [[ -z "${DISPLAY:-}" ]]; then
        error "X11 forwarding requested but DISPLAY is not set.
Set DISPLAY or run from a graphical session." 7
    fi
    if [[ ! -S "/tmp/.X11-unix/X${DISPLAY#:}" ]] && [[ ! -S "/tmp/.X11-unix/X0" ]]; then
        warn "X11 socket not found at expected location. X11 may not work."
    fi
}
```

---

## Resolved Unknowns

All technical unknowns from the spec have been resolved:

| Unknown | Resolution |
|---------|------------|
| X11 forwarding mechanism | Unix socket mount + DISPLAY passthrough |
| Shell environment parity | Same entrypoint/user-init.sh flow, different final command |
| Exec command capture | `--exec` captures all remaining args |
| Mutual exclusivity | `--shell` and `--exec` are mutually exclusive; error if both |

## Dependencies

No new dependencies required:
- X11 socket mount uses standard Docker volume syntax
- Bash command override uses existing Docker CMD mechanism
- No container image changes needed
