# Feature Specification: Docker Container for Claude Code

**Feature Branch**: `001-docker-claude`
**Created**: 2026-01-24
**Status**: Draft
**Input**: User description: "Set up a docker container with the capability of running claude code in a secure isolated environment. Create a script 'cly' that launches a container initially based on ubuntu 24.04 LTS with all necessary dependencies installed. The script should mount the current dir in the container preserving the dir name and then launch claude code in skip permissions mode with the full TUI available in the host terminal. The claude code instance in the container needs to be able to authenticate using the users credentials, possibly by mounting ~/.claude in the container as a way to have common settings. It is a requirement that the user has a way to use multiple anthropic accounts from within the container without needing to use an API key."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Basic Container Launch (Priority: P1)

As a developer, I want to run Claude Code inside a Docker container from any project directory so that I can use Claude Code's capabilities while keeping my host system isolated from any commands it executes.

**Why this priority**: This is the core functionality - without the ability to launch the container and run Claude Code, no other features matter.

**Independent Test**: Can be fully tested by running `clyde` from a project directory and verifying Claude Code's TUI appears and can interact with files in the mounted directory.

**Acceptance Scenarios**:

1. **Given** a user is in a project directory (e.g., `/home/user/projects/myapp`), **When** the user runs `clyde`, **Then** a Docker container starts with that directory mounted at the same path inside the container, and Claude Code launches with its full TUI visible in the terminal.

2. **Given** a user runs `clyde` in a directory with files, **When** Claude Code is asked to read or modify a file, **Then** the changes are reflected on the host filesystem via the volume mount.

3. **Given** the container is running, **When** the user interacts with Claude Code's TUI, **Then** all keyboard inputs (including escape sequences, arrow keys, and ctrl combinations) work correctly.

---

### User Story 2 - Authentication with Existing Credentials (Priority: P2)

As a developer with an existing Claude Code subscription, I want the containerized Claude Code to use my existing authentication so that I don't need to re-authenticate every time I launch the container.

**Why this priority**: Authentication is essential for Claude Code to function, but the container must first exist (P1) before we can authenticate within it.

**Independent Test**: Can be tested by launching `clyde` and verifying Claude Code recognizes the user's authentication status without prompting for login.

**Acceptance Scenarios**:

1. **Given** a user has previously authenticated with Claude Code on their host system (credentials stored in `~/.claude`), **When** the user runs `clyde`, **Then** Claude Code inside the container recognizes the existing authentication and does not prompt for login.

2. **Given** a user has never authenticated, **When** the user runs `clyde` and Claude Code prompts for authentication, **Then** the authentication flow completes successfully and credentials are persisted to the host's `~/.claude` directory for future sessions.

---

### User Story 3 - Multiple Account Support (Priority: P3)

As a developer who works with multiple Anthropic accounts (e.g., personal and work), I want to switch between accounts without using API keys so that I can use the appropriate account for different projects.

**Why this priority**: This is an advanced feature that builds upon the basic authentication (P2). Most users will start with a single account.

**Independent Test**: Can be tested by launching `clyde`, switching accounts using Claude Code's built-in account switching mechanism, and verifying the switch persists.

**Acceptance Scenarios**:

1. **Given** a user has multiple Anthropic accounts configured, **When** the user uses Claude Code's account switching feature inside the container, **Then** the account switch is successful and the new account's credentials are used for subsequent requests.

2. **Given** a user switches accounts in the container, **When** the user exits and relaunches `clyde`, **Then** the previously selected account remains active.

---

### User Story 4 - Skip Permissions Mode (Priority: P4)

As a developer who trusts the containerized environment, I want Claude Code to run in skip-permissions mode so that I don't need to approve every file operation, while knowing the container provides isolation.

**Why this priority**: This enhances productivity but depends on the container being properly isolated (P1) to be safe.

**Independent Test**: Can be tested by launching `clyde` and having Claude Code perform file operations without confirmation prompts appearing.

**Acceptance Scenarios**:

1. **Given** the user runs `clyde`, **When** Claude Code attempts to read, write, or execute commands, **Then** these operations proceed without requiring user confirmation for each action.

2. **Given** skip-permissions mode is active, **When** Claude Code executes a command, **Then** the command runs within the container's isolated environment.

---

### Edge Cases

- What happens when the Docker daemon is not running? The script MUST display a clear error message instructing the user to start Docker.
- What happens when the `~/.claude` directory does not exist on the host? The script MUST create it automatically before mounting.
- What happens when the user runs `clyde` from the root filesystem (`/`)? The script MUST refuse with a clear error message and exit code 5. Mounting the root filesystem would expose the entire system to Claude Code, which is a security risk with no legitimate use case.
- What happens when the Docker image is not yet built/pulled? The script MUST build or pull the image automatically on first run.
- What happens when the container is terminated unexpectedly? Any file changes already written to the mounted volume MUST persist.
- What happens when network connectivity is lost mid-session? Claude Code's standard offline behavior applies; the container remains running.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: System MUST provide a `clyde` script that users can invoke from any directory to launch the containerized Claude Code environment.
- **FR-002**: System MUST mount the user's current working directory into the container at the identical path (preserving the full directory structure).
- **FR-003**: System MUST mount the host's `~/.claude` directory into the container at the container user's `~/.claude` (resolves to `/home/claude/.claude`) to share authentication and settings.
- **FR-003a**: System MUST mount the host's `~/.gitconfig` and `~/.ssh` directory into the container as read-only to enable git operations without risking modification of credentials.
- **FR-004**: System MUST launch Claude Code with the `--dangerously-skip-permissions` flag inside the container.
- **FR-005**: System MUST allocate a pseudo-TTY and attach stdin/stdout/stderr to enable Claude Code's full TUI functionality.
- **FR-006**: System MUST use Ubuntu 24.04 LTS as the base image for the container.
- **FR-007**: System MUST install all dependencies required for Claude Code to function (Node.js runtime, Claude Code CLI).
- **FR-008**: System MUST support Claude Code's OAuth-based authentication flow for multiple account support without API keys. The system MUST auto-detect display availability: when a display is available (X11/Wayland), open the browser directly on the host; when no display is detected, print the OAuth URL for manual browser opening.
- **FR-009**: System MUST run the container with non-root user privileges, dynamically matching the host user's UID/GID at container start to ensure correct file permissions on mounted volumes.
- **FR-009a**: System MUST run the container with full host network access to enable Claude Code to reach arbitrary endpoints (APIs, package registries, git remotes).
- **FR-010**: System MUST clean up containers after exit (use `--rm` flag or equivalent).
- **FR-011**: System MUST automatically build the Docker image if it does not exist locally.
- **FR-012**: System MUST verify Docker is available and running before attempting to launch the container.
- **FR-013**: System MUST apply default resource limits (8GB RAM, 4 CPUs) to prevent runaway processes, with command-line flags available to override these defaults.

### Key Entities

- **Container Image**: The Docker image based on Ubuntu 24.04 LTS with Claude Code and all dependencies pre-installed.
- **Launch Script (`clyde`)**: The shell script users invoke to start the containerized environment.
- **Credentials Store**: The `~/.claude` directory containing OAuth tokens, settings, and account configurations.
- **Project Mount**: The volume mount binding the host's current directory to the same path inside the container.

## Clarifications

### Session 2026-01-24

- Q: Script name `cly` vs `clyde`? → A: Changed from original `cly` to `clyde` for clarity and to avoid confusion with other tools. The longer name is more distinctive and searchable.
- Q: Should running from root filesystem (/) be allowed with confirmation? → A: No. Refuse outright with exit code 5. No legitimate use case justifies the security risk of exposing the entire filesystem.
- Q: How should OAuth browser authentication work inside the container? → A: Support both mechanisms (host browser via X11/Wayland forwarding when display available, manual URL copy when not) with auto-detection of display availability.
- Q: How should git and SSH access be handled? → A: Mount `~/.gitconfig` and `~/.ssh` read-only to enable git operations while preventing modification of keys or config.
- Q: What network access should the container have? → A: Full host network access to allow Claude Code to reach arbitrary endpoints (APIs, package registries, git remotes).
- Q: How should container user UID/GID be handled for file permissions? → A: Dynamically match host user's UID/GID at container start to avoid permission issues.
- Q: Should there be default resource limits? → A: Yes, sensible defaults (8GB RAM, 4 CPUs) with command-line flags to override.

## Assumptions

- Users have Docker installed and the Docker daemon running on their host system.
- Users have a valid Anthropic account with Claude Code subscription (Max or similar).
- The host system is Linux or macOS (Windows support via WSL2 may work but is not the primary target).
- Users understand that skip-permissions mode allows Claude Code to execute commands without confirmation.
- The `~/.claude` directory structure used by Claude Code is stable and can be safely shared between host and container.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Users can launch Claude Code in a container within 10 seconds of running `clyde` (after initial image build).
- **SC-002**: 100% of Claude Code TUI features (keyboard navigation, text input, visual rendering) work identically to native execution.
- **SC-003**: File operations performed by Claude Code are immediately visible on the host filesystem with no manual sync required.
- **SC-004**: Users can switch between multiple Anthropic accounts without entering API keys or re-authenticating from scratch.
- **SC-005**: Container provides complete isolation - commands executed by Claude Code cannot access host files outside the mounted directory.
- **SC-006**: First-time setup (image build) completes in under 5 minutes on a standard broadband connection.
