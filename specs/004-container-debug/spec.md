# Feature Specification: Container Debugging Options

**Feature Branch**: `004-container-debug`
**Created**: 2026-02-03
**Status**: Draft
**Input**: User description: "Add two command line options to clyde for container debugging: X11 forwarding for graphical programs, and shell-only mode to launch container without starting Claude"

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Shell-Only Mode for Manual Testing (Priority: P1)

A developer working on an application using Clyde wants to run their test suite or application manually in the exact same environment that Claude uses. They need to launch the container and get a shell prompt instead of starting Claude Code, so they can run `cargo test`, debug with `gdb`, or manually execute their program to reproduce issues.

**Why this priority**: This is the most critical debugging capability. Without it, developers cannot verify their code behaves the same way when Claude runs it as when they run it locally. This directly impacts the core value proposition of Clyde as an isolated development environment.

**Independent Test**: Can be fully tested by running `clyde --shell` and verifying you get a bash prompt with the same environment, mounts, and permissions as a normal Clyde session. Delivers immediate value for any developer debugging environment-specific issues.

**Acceptance Scenarios**:

1. **Given** a developer in a project directory, **When** they run `clyde --shell`, **Then** they receive an interactive shell prompt inside the container with access to the project directory and all configured packages
2. **Given** a developer using shell-only mode, **When** they run commands in the shell, **Then** the environment (PATH, mounted volumes, UID/GID, Nix packages) is identical to what Claude Code would see
3. **Given** a developer who needs to run tests, **When** they launch shell-only mode and execute `cargo test` or `npm test`, **Then** the tests run in the same isolated environment as Claude would use

---

### User Story 2 - X11 Forwarding for Graphical Debugging (Priority: P2)

A developer needs to run a graphical application from inside the Clyde container, such as launching a GUI debugger, viewing rendered output, or testing a graphical application they are developing. They want to enable X11 forwarding so that GUI windows appear on their host display.

**Why this priority**: Graphical debugging is valuable but not as universally needed as shell access. Most debugging can be done via CLI, but certain use cases (GUI apps, visual debuggers, browser testing) require X11.

**Independent Test**: Can be fully tested by running `clyde --x11` (or combined with `--shell`) and launching a simple X11 application like `xeyes` or `xclock`. Delivers value for developers working on graphical applications or using GUI-based debugging tools.

**Acceptance Scenarios**:

1. **Given** a developer on a Linux host with X11, **When** they run `clyde --x11 --shell` and execute `xclock`, **Then** the xclock window appears on their host display
2. **Given** a developer with X11 forwarding enabled, **When** they run a graphical application, **Then** the application window appears and is interactive
3. **Given** a developer who needs both debugging features, **When** they run `clyde --x11 --shell`, **Then** they get a shell with X11 forwarding enabled

---

### User Story 3 - Combined Options with Normal Claude Workflow (Priority: P3)

A developer occasionally needs to see graphical output while using Claude Code normally (not in shell mode). They want to enable X11 forwarding while still launching Claude Code, so that if Claude runs a command that produces graphical output, it can be displayed.

**Why this priority**: This is a more specialized use case. Most developers will either use shell mode for debugging or use Claude normally. However, supporting X11 with Claude running allows for scenarios where Claude needs to render or display something graphically.

**Independent Test**: Can be tested by running `clyde --x11` (without `--shell`) and having Claude run a command that produces graphical output. The window should appear on the host display.

**Acceptance Scenarios**:

1. **Given** a developer running `clyde --x11`, **When** Claude executes a command that launches a graphical window, **Then** the window appears on the host display
2. **Given** a developer using X11 mode with Claude, **When** the session ends, **Then** cleanup happens normally with no orphaned X11 processes

---

### Edge Cases

- What happens when X11 forwarding is requested but the host has no DISPLAY set?
  - Display a clear error message and exit gracefully
- What happens when the user passes `--shell` along with Claude arguments after `--`?
  - Shell mode takes precedence; the `--` arguments are ignored with a warning
- What happens when X11 is requested on a headless server?
  - Allow it (may be using remote X11 forwarding via SSH); warn if DISPLAY is unset
- What happens if xhost access control prevents container access?
  - Document the need for `xhost +local:` or provide option to handle automatically

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: System MUST provide a `--shell` command line option that launches the container with an interactive shell instead of Claude Code
- **FR-002**: System MUST ensure shell-only mode provides identical environment to normal Claude Code mode (same mounts, packages, permissions, PATH)
- **FR-003**: System MUST provide an `--x11` command line option that enables X11 forwarding from container to host
- **FR-004**: System MUST mount the host's X11 Unix socket when `--x11` is specified
- **FR-005**: System MUST pass the DISPLAY environment variable to the container when `--x11` is specified
- **FR-006**: System MUST allow combining `--x11` with `--shell` for graphical debugging sessions
- **FR-007**: System MUST allow combining `--x11` with normal Claude Code mode
- **FR-008**: System MUST validate that DISPLAY is set when `--x11` is specified and exit with a clear error if not
- **FR-009**: System MUST update the help text to document both new options
- **FR-010**: System MUST provide corresponding environment variables (`CLYDE_X11`, `CLYDE_SHELL`) for persistent configuration
- **FR-011**: System MUST provide an `--exec <command>` option that runs a single command in the container environment and exits
- **FR-012**: System MUST allow combining `--exec` with `--x11` for graphical command execution

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Developer can launch shell-only mode and run test commands within 5 seconds of container start
- **SC-002**: Commands executed in shell-only mode produce identical results to commands Claude would run in the same environment
- **SC-003**: Graphical applications launched with X11 forwarding display their windows on the host within 2 seconds of launch
- **SC-004**: Users who encounter missing X11 requirements receive actionable error messages that explain how to resolve the issue
- **SC-005**: Both new options work correctly with all existing Clyde options (--memory, --cpus, --profile, --nix-verbose, etc.)

## Assumptions

- Users enabling X11 forwarding are on Linux hosts with X11 (or compatible X server)
- The container base image already has or can easily have X11 client libraries available via Nix
- `--shell` provides interactive shell only; single-command execution uses separate `--exec` flag
- Security: X11 forwarding security implications are documented in help text and README; no runtime warning or explicit acknowledgment required

## Clarifications

### Session 2026-02-03

- Q: What security posture should X11 forwarding adopt? → A: Warn in help text only - document security implications in `--help` and README
- Q: Should shell mode support command passthrough for scripting/CI? → A: Separate `--exec <cmd>` flag for single commands; `--shell` remains interactive-only
