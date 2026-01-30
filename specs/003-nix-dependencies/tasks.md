# Tasks: Nix-Based Dependency Management

**Input**: Design documents from `/specs/003-nix-dependencies/`
**Prerequisites**: plan.md ✓, spec.md ✓, research.md ✓, data-model.md ✓, quickstart.md ✓

**Tests**: Not explicitly requested in specification - omitted per template guidelines.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

## Path Conventions

Per plan.md, this is a single-project structure:
- `docker/` - Dockerfile and container configuration
- `docker/nix/` - Nix configuration files (new)
- `bin/` - Launch script
- `tests/` - Test fixtures and bats tests

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Backup existing files and create new directory structure

- [ ] T001 Backup current Dockerfile to docker/Dockerfile.old
- [ ] T002 Create docker/nix/ directory structure for Nix configurations
- [ ] T003 [P] Create tests/integration/nix-configs/ directory for test fixtures

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core Nix infrastructure that MUST be complete before ANY user story can be implemented

**⚠️ CRITICAL**: No user story work can begin until this phase is complete

- [ ] T004 Create minimal Dockerfile with Ubuntu 24.04 minimal + Nix + tini/gosu in docker/Dockerfile
- [ ] T005 [P] Create base Nix flake with default packages (git, gh, nodejs, curl, openssh) in docker/nix/flake.nix (NOTE: claude-code NOT included - installed via npm)
- [ ] T006 Create docker/nix/flake.lock by running nix flake lock in docker/nix/
- [ ] T007 Add Nix profile sourcing and /nix ownership transfer to docker/entrypoint.sh
- [ ] T008 Add clyde-nix-store volume mount to bin/clyde
- [ ] T008a Add clyde-npm-cache volume mount to bin/clyde for Claude Code installation
- [ ] T009 Add Nix environment variables (CLYDE_NIX_VERBOSE, CLYDE_NIX_STORE_VOLUME) to bin/clyde

**Checkpoint**: Foundation ready - Nix installed in container, base flake created, both volume mounts configured (nix-store + npm-cache)

---

## Phase 3: User Story 1 - Zero-Config Default Experience (Priority: P1) 🎯 MVP

**Goal**: Users with no Nix configuration can start clyde with claude-code (latest via npm), git, gh, and node available - exactly like today

**Independent Test**: Run `clyde` in a directory with no flake.nix/shell.nix, verify claude, git, gh, and node commands are available

### Implementation for User Story 1

- [ ] T010 [US1] Add Nix environment activation to docker/entrypoint.sh (default flake path: /docker/nix)
- [ ] T010a [US1] Add Claude Code npm installation to docker/entrypoint.sh (npm install -g @anthropic-ai/claude-code with cached prefix)
- [ ] T011 [US1] Add progress output filtering (show package names during fetch) to docker/entrypoint.sh
- [ ] T012 [US1] Add --nix-verbose flag handling to bin/clyde (sets CLYDE_NIX_VERBOSE=1)
- [ ] T013 [US1] Update docker/entrypoint.sh to respect CLYDE_NIX_VERBOSE for full Nix output
- [ ] T014 [P] [US1] Create test fixture: empty directory for zero-config test in tests/integration/nix-configs/zero-config/
- [ ] T015 [US1] Verify startup time is within acceptable range (<10s cached) - manual validation

**Checkpoint**: Zero-config users can run clyde with default packages via Nix

---

## Phase 4: User Story 4 - Persistent Nix Store (Priority: P2)

**Goal**: Packages downloaded once are cached and reused across sessions

**Independent Test**: Run clyde twice with the same configuration, verify second run starts significantly faster

**Note**: User Story 4 is implemented before US2/US3 because caching makes the other stories usable in practice

### Implementation for User Story 4

- [ ] T016 [US4] Add automatic clyde-nix-store volume creation to bin/clyde (if not exists)
- [ ] T017 [US4] Add --nix-gc flag to bin/clyde for garbage collection
- [ ] T018 [US4] Implement nix-collect-garbage command execution in docker/entrypoint.sh (when --nix-gc passed)
- [ ] T019 [P] [US4] Create test fixture: flake.nix with unique package for cache testing in tests/integration/nix-configs/cache-test/flake.nix

**Checkpoint**: Nix store persists across container restarts, --nix-gc works

---

## Phase 5: User Story 2 - Project-Specific Dependencies (Priority: P2)

**Goal**: Projects with flake.nix or shell.nix get those packages automatically

**Independent Test**: Create a minimal flake.nix with ripgrep, run clyde, verify rg is available

### Implementation for User Story 2

- [ ] T020 [US2] Add project flake.nix/shell.nix detection to bin/clyde
- [ ] T021 [US2] Add mount of project's flake.nix, shell.nix, flake.lock (read-only) to bin/clyde
- [ ] T022 [US2] Add project config detection and nix develop/nix-shell activation to docker/entrypoint.sh
- [ ] T023 [US2] Add error handling with "Proceed with defaults? [Y/n]" prompt to docker/entrypoint.sh
- [ ] T023a [US2] Add disk space error detection with "clyde --nix-gc" suggestion to docker/entrypoint.sh
- [ ] T023b [US2] Add network error handling with clear message listing unfetched packages to docker/entrypoint.sh
- [ ] T024 [P] [US2] Create test fixture: valid flake.nix with ripgrep in tests/integration/nix-configs/project-flake/flake.nix
- [ ] T025 [P] [US2] Create test fixture: valid shell.nix in tests/integration/nix-configs/project-shell/shell.nix
- [ ] T026 [P] [US2] Create test fixture: invalid flake.nix (syntax error) in tests/integration/nix-configs/invalid-flake/flake.nix

**Checkpoint**: Project-specific packages work with flake.nix and shell.nix, errors handled gracefully

---

## Phase 6: User Story 3 - User-Global Default Packages (Priority: P3)

**Goal**: Users can add packages to ~/.config/clyde/flake.nix that apply to all sessions

**Independent Test**: Create ~/.config/clyde/flake.nix with jq, run clyde in project without config, verify jq is present

### Implementation for User Story 3

- [ ] T027 [US3] Add ~/.config/clyde/ mount (read-only) detection to bin/clyde
- [ ] T028 [US3] Add user config detection (flake.nix/shell.nix in ~/.config/clyde/) to docker/entrypoint.sh
- [ ] T029 [US3] Implement configuration layer merging (inputsFrom) in docker/entrypoint.sh
- [ ] T030 [US3] Ensure project config takes precedence over user config on conflicts
- [ ] T031 [P] [US3] Create test fixture: user flake.nix with jq in tests/integration/nix-configs/user-config/flake.nix

**Checkpoint**: User global packages merged with project packages, precedence correct

---

## Phase 7: User Story 5 - Inspect Available Packages (Priority: P4)

**Goal**: Users can see what packages are available in their environment

**Independent Test**: Run the inspection command and verify output lists expected packages

### Implementation for User Story 5

- [ ] T032 [US5] Add --list-packages flag to bin/clyde
- [ ] T033 [US5] Implement package listing without starting full session (nix flake show or similar) in bin/clyde
- [ ] T034 [P] [US5] Create clyde-packages helper script for in-container package listing in docker/nix/clyde-packages

**Checkpoint**: Package inspection works both outside (--list-packages) and inside (clyde-packages) container

---

## Phase 8: Polish & Cross-Cutting Concerns

**Purpose**: Improvements that affect multiple user stories

- [ ] T035 [P] Update bin/clyde --help text to include new Nix-related flags
- [ ] T036 [P] Update docker/Dockerfile comments to explain Nix installation
- [ ] T037 Run shellcheck on modified scripts (bin/clyde, docker/entrypoint.sh)
- [ ] T038 [P] Extend tests/unit/clyde.bats with Nix flag parsing tests
- [ ] T039 Validate quickstart.md scenarios work end-to-end (manual testing)
- [ ] T039a Verify existing clyde features work post-migration: SSH agent forwarding, --profile flag, --memory/--cpus limits, git config mounting
- [ ] T040 Verify SC-004: Docker image size reduced by at least 50% (manual validation)

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies - can start immediately
- **Foundational (Phase 2)**: Depends on Setup completion - BLOCKS all user stories
- **User Story 1 (Phase 3)**: Depends on Foundational - core default experience
- **User Story 4 (Phase 4)**: Depends on Foundational - enables practical use of US2/US3
- **User Story 2 (Phase 5)**: Depends on Foundational (can run parallel to US1/US4 if desired)
- **User Story 3 (Phase 6)**: Depends on US2 implementation (uses same detection patterns)
- **User Story 5 (Phase 7)**: Depends on Foundational (can run parallel to other stories)
- **Polish (Phase 8)**: Depends on all user stories being complete

### User Story Dependencies

- **User Story 1 (P1)**: No dependencies on other stories - MVP
- **User Story 4 (P2)**: No dependencies on other stories - critical for usability
- **User Story 2 (P2)**: No dependencies on other stories, but shares patterns with US1
- **User Story 3 (P3)**: Builds on US2's config detection patterns
- **User Story 5 (P4)**: Independent, nice-to-have

### Within Each Phase

- Tasks marked [P] can run in parallel
- Sequential tasks within a phase should be completed in order
- Test fixtures can be created in parallel with implementation

### Parallel Opportunities

**Within Phase 2 (Foundational):**
```
Sequential: T005 (base flake) → T006 (flake.lock)
Note: T006 requires T005 to complete first
```

**Within Phase 5 (User Story 2):**
```
Parallel: T024 (valid flake fixture), T025 (shell.nix fixture), T026 (invalid flake fixture)
```

**Cross-Phase Parallelism (with team):**
- US1 and US4 can proceed in parallel after Foundational
- US2 and US5 can proceed in parallel
- Test fixtures can be created while implementation is ongoing

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup (T001-T003)
2. Complete Phase 2: Foundational (T004-T009)
3. Complete Phase 3: User Story 1 (T010-T015)
4. **STOP and VALIDATE**: Verify clyde works with Nix-provided defaults
5. Deploy/demo if ready - users get same experience as before

### Incremental Delivery

1. Complete Setup + Foundational → Nix infrastructure ready
2. Add User Story 1 → Default experience works via Nix (MVP!)
3. Add User Story 4 → Caching makes it practical
4. Add User Story 2 → Project-specific packages
5. Add User Story 3 → User global packages
6. Add User Story 5 → Package inspection (nice-to-have)

### Key Files Modified

| File | Stories | Changes |
|------|---------|---------|
| docker/Dockerfile | Foundation | Complete rewrite: Ubuntu minimal + Nix only |
| docker/nix/flake.nix | Foundation, US1 | New file: base package definitions (git, gh, node - NOT claude-code) |
| docker/entrypoint.sh | US1, US2, US3, US4 | Nix activation, npm install claude-code, config detection, error handling |
| bin/clyde | All | Two volume mounts (nix-store + npm-cache), config detection, new flags |

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story for traceability
- Each user story should be independently completable and testable
- Commit after each task or logical group
- Stop at any checkpoint to validate story independently
- FR-xxx references in spec.md map to implementation tasks as noted
