# Tasks: Docker Container for Claude Code

**Input**: Design documents from `/specs/001-docker-claude/`
**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/

**Tests**: Bats-core tests included per plan.md structure for security-critical behavior verification.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3, US4)
- Include exact file paths in descriptions

## Path Conventions

Based on plan.md structure:
- `docker/` - Container-related files (Dockerfile, entrypoint.sh)
- `bin/` - User-facing scripts (clyde)
- `tests/` - Test files (unit/, integration/)

---

## Phase 1: Setup (Project Infrastructure)

**Purpose**: Create project structure and establish foundational files

- [x] T001 Create docker/ directory structure at repository root
- [x] T002 [P] Create docker/.dockerignore with build context exclusions
- [x] T003 [P] Create bin/ directory for launch script
- [x] T004 [P] Create tests/unit/ and tests/integration/ directory structure

---

## Phase 2: Foundational (Container Image)

**Purpose**: Build the Docker image that ALL user stories depend on

**CRITICAL**: No user story can function without a working container image

- [x] T005 Create docker/Dockerfile with Ubuntu 24.04 base image per dockerfile-spec.md
- [x] T006 Add Node.js 20 LTS installation layer to docker/Dockerfile via NodeSource
- [x] T007 Add Claude Code npm global installation to docker/Dockerfile
- [x] T008 Add system packages (curl, ca-certificates, git, openssh-client, tini, gosu, xdg-utils) to docker/Dockerfile
- [x] T009 Create docker/entrypoint.sh with UID/GID matching logic per dockerfile-spec.md
- [x] T010 Configure Dockerfile ENTRYPOINT with tini and entrypoint.sh
- [x] T011 Configure Dockerfile CMD with claude --dangerously-skip-permissions

**Checkpoint**: Docker image can be built successfully with `docker build -t clyde:local docker/`

---

## Phase 3: User Story 1 - Basic Container Launch (Priority: P1) MVP

**Goal**: Users can run `clyde` from any directory to launch Claude Code in an isolated container with the current directory mounted

**Independent Test**: Run `clyde` from a project directory, verify Claude Code TUI appears and can read/modify files in mounted directory

### Implementation for User Story 1

- [x] T012 [US1] Create bin/clyde script skeleton with shebang and set -euo pipefail
- [x] T013 [US1] Implement usage() function with help text per cli-interface.md in bin/clyde
- [x] T014 [US1] Implement version display function in bin/clyde
- [x] T015 [US1] Implement argument parsing (--help, --version, --memory, --cpus, --build, --no-git) with getopts in bin/clyde
- [x] T016 [US1] Implement Docker availability check (docker info) with exit code 2 in bin/clyde
- [x] T017 [US1] Implement root directory check (PWD = /) with exit code 5 in bin/clyde
- [x] T018 [US1] Implement auto-build logic (docker image inspect, docker build) in bin/clyde
- [x] T019 [US1] Implement --build flag for forced rebuild in bin/clyde
- [x] T020 [US1] Implement PWD volume mount at identical path inside container in bin/clyde
- [x] T021 [US1] Implement TTY allocation (-it) and stdin/stdout/stderr attachment in bin/clyde
- [x] T022 [US1] Implement --rm flag for container cleanup in bin/clyde
- [x] T023 [US1] Implement resource limits (--memory, --cpus) with defaults from CLYDE_MEMORY/CLYDE_CPUS or 8g/4 in bin/clyde
- [x] T024 [US1] Implement host network mode (--network host) in bin/clyde
- [x] T025 [US1] Implement HOST_UID and HOST_GID environment variable passing in bin/clyde
- [x] T026 [US1] Implement -- separator for passing arguments to Claude Code in bin/clyde
- [x] T027 [US1] Assemble complete docker run command and execute in bin/clyde

**Checkpoint**: `clyde` launches Claude Code in container, TUI works, files in PWD are accessible

---

## Phase 4: User Story 2 - Authentication with Existing Credentials (Priority: P2)

**Goal**: Claude Code inside container uses existing host authentication from ~/.claude without re-prompting

**Independent Test**: Launch `clyde` with existing ~/.claude credentials, verify no login prompt appears

### Implementation for User Story 2

- [x] T028 [US2] Implement ~/.claude directory existence check and auto-creation in bin/clyde
- [x] T029 [US2] Implement ~/.claude volume mount (rw) to /home/claude/.claude in bin/clyde
- [x] T030 [US2] Add informational message when ~/.claude does not exist in bin/clyde

**Checkpoint**: Credentials persist between sessions, no re-authentication required

---

## Phase 5: User Story 3 - Multiple Account Support (Priority: P3)

**Goal**: Users can switch Anthropic accounts via Claude Code's built-in mechanism, with changes persisting across sessions

**Independent Test**: Switch accounts inside container, exit, relaunch, verify new account remains active

### Implementation for User Story 3

- [x] T031 [US3] Implement X11 display detection and forwarding (DISPLAY, /tmp/.X11-unix mount) in bin/clyde
- [x] T032 [US3] Implement Wayland display detection and forwarding (WAYLAND_DISPLAY, XDG_RUNTIME_DIR socket) in bin/clyde
- [x] T033 [US3] Add conditional display mount logic (only when display available) in bin/clyde

**Checkpoint**: OAuth browser flow works for account switching when display available, URL printed when headless

---

## Phase 6: User Story 4 - Skip Permissions Mode (Priority: P4)

**Goal**: Claude Code runs in skip-permissions mode by default, with container isolation providing safety

**Independent Test**: Launch `clyde`, have Claude Code perform file operations without confirmation prompts

### Implementation for User Story 4

- [x] T034 [US4] **[REVIEW GATE]** Verify docker/Dockerfile CMD includes --dangerously-skip-permissions flag (implemented in T011)
- [x] T035 [US4] Implement ~/.gitconfig mount (ro) to /home/claude/.gitconfig unless --no-git in bin/clyde
- [x] T036 [US4] Implement ~/.ssh mount (ro) to /home/claude/.ssh unless --no-git in bin/clyde
- [x] T037 [US4] Implement CLYDE_NO_GIT environment variable support in bin/clyde

**Checkpoint**: File operations proceed without prompts, git/SSH operations work with host credentials

---

## Phase 7: Polish, Testing & Cross-Cutting Concerns

**Purpose**: Quality improvements, automated tests, and validation

### Static Analysis

- [x] T038 [P] Run shellcheck on bin/clyde and fix any warnings
- [x] T039 [P] Run shellcheck on docker/entrypoint.sh and fix any warnings

### Automated Tests

- [x] T040 [P] Create tests/unit/clyde.bats with tests for argument parsing and validation
- [x] T041 [P] Add unit tests for exit codes (Docker not running=2, root dir=5, build failed=3, invalid args=4)
- [x] T042 [P] Create tests/integration/container.bats with tests for container launch and mounts
- [x] T043 Add integration test verifying UID/GID matching works correctly
- [x] T044 Add integration test verifying container cleanup (--rm) after exit

### Manual Validation

- [x] T045 Verify all exit codes match cli-interface.md contract
- [x] T046 [P] Test error messages match cli-interface.md expected output
- [x] T047 Run quickstart.md validation scenarios manually
- [x] T048 Verify image builds in under 5 minutes per SC-006

### Review Gates

- [x] T049 [P] **[REVIEW GATE]** Verify scripts use mktemp with trap cleanup if any temp files exist (constitution III)
- [x] T050 **[REVIEW GATE]** Verify all bats tests pass before merge

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies - can start immediately
- **Foundational (Phase 2)**: Depends on Phase 1 (docker/ directory must exist)
- **User Story 1 (Phase 3)**: Depends on Phase 2 (container image must be buildable)
- **User Story 2 (Phase 4)**: Depends on Phase 3 (basic launch must work)
- **User Story 3 (Phase 5)**: Depends on Phase 4 (authentication must work)
- **User Story 4 (Phase 6)**: Depends on Phase 3 (basic launch must work); can run parallel to US2/US3
- **Polish (Phase 7)**: Depends on all user stories being complete

### User Story Dependencies

- **User Story 1 (P1)**: Core functionality - MUST complete first
- **User Story 2 (P2)**: Builds on US1 (credential mounting extends basic mounts)
- **User Story 3 (P3)**: Builds on US2 (display forwarding enables OAuth for account switching)
- **User Story 4 (P4)**: Extends US1 (git/SSH mounts complement basic functionality)

### Within Each User Story

- Earlier tasks set up prerequisites for later tasks
- T012-T015: Script skeleton and argument parsing (foundation for US1)
- T016-T019: Validation and auto-build (safety checks)
- T020-T027: Docker run construction (core functionality)

### Parallel Opportunities

Phase 1:
- T002, T003, T004 can run in parallel

Phase 7:
- T038 and T039 can run in parallel (shellcheck)
- T040, T041, T042 can run in parallel (test file creation)
- T046 can run in parallel with other polish tasks

---

## Parallel Example: Phase 1 Setup

```bash
# Launch all setup tasks together:
Task: "Create docker/.dockerignore with build context exclusions"
Task: "Create bin/ directory for launch script"
Task: "Create tests/unit/ and tests/integration/ directory structure"
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup (T001-T004)
2. Complete Phase 2: Foundational (T005-T011)
3. Complete Phase 3: User Story 1 (T012-T027)
4. **STOP and VALIDATE**: Test that `clyde` launches Claude Code with PWD mounted
5. Deploy/demo MVP

### Incremental Delivery

1. Setup + Foundational -> Container image builds
2. Add User Story 1 -> Basic launch works (MVP!)
3. Add User Story 2 -> Credentials persist
4. Add User Story 3 -> Account switching works
5. Add User Story 4 -> Git/SSH integration complete
6. Polish -> Production ready

### Single Developer Flow

Execute phases sequentially (P1 -> P2 -> P3 -> P4) since user stories have dependencies:
- US1 is prerequisite for all others
- US2 and US4 can be done in any order after US1
- US3 depends on US2 (authentication must work before account switching)

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story for traceability
- Bats tests verify security-critical behaviors (exit codes, UID/GID, cleanup)
- Commit after each task or logical group
- Validate at each checkpoint before proceeding
- All scripts must pass shellcheck before Phase 7 completion
