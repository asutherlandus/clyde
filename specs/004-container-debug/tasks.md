# Tasks: Container Debugging Options

**Input**: Design documents from `/specs/004-container-debug/`
**Prerequisites**: plan.md, spec.md, research.md, quickstart.md

**Tests**: Not explicitly requested in spec - tests will be added following existing bats patterns.

**Organization**: Tasks grouped by user story for independent implementation and testing.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (US1, US2, US3)
- Include exact file paths in descriptions

## Path Conventions

```text
bin/clyde                    # Main launch script (MODIFY)
docker/nix/user-init.sh      # Nix environment activation (NO CHANGES - research confirms)
tests/unit/clyde.bats        # Unit tests (MODIFY)
tests/integration/container.bats  # Integration tests (MODIFY)
```

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Project initialization - add foundational variables and functions

- [X] T001 Add SHELL_MODE and X11_ENABLED variables with env defaults in bin/clyde
- [X] T002 Add EXEC_COMMAND array variable in bin/clyde
- [X] T003 [P] Add validate_x11() function to check DISPLAY in bin/clyde

**Checkpoint**: Foundational variables and validation functions ready

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core argument parsing that all user stories depend on

**CRITICAL**: No user story work can begin until this phase is complete

- [X] T005 Add --shell option to parse_args() in bin/clyde
- [X] T006 Add --x11 option to parse_args() in bin/clyde
- [X] T007 Add --exec option to parse_args() that captures remaining args in bin/clyde
- [X] T008 Add mutual exclusivity check for --shell and --exec in bin/clyde
- [X] T009 Add warning when --shell used with -- CLAUDE_ARGS in bin/clyde

**Checkpoint**: All new CLI flags are parsed; mutual exclusivity enforced

---

## Phase 3: User Story 1 - Shell-Only Mode for Manual Testing (Priority: P1) MVP

**Goal**: Launch container with interactive bash shell instead of Claude Code

**Independent Test**: Run `clyde --shell` and verify bash prompt with same environment as normal Clyde

### Implementation for User Story 1

- [X] T010 [US1] Modify run_container() to pass 'bash' as command when SHELL_MODE=true in bin/clyde
- [X] T011 [US1] Ensure shell mode uses same docker_args (mounts, env vars, permissions) in bin/clyde
- [X] T012 [US1] Add CLYDE_SHELL to usage() help text in bin/clyde
- [X] T013 [US1] Add --shell to usage() Options section in bin/clyde
- [X] T014 [US1] Add CLYDE_SHELL to Environment Variables section in usage() in bin/clyde
- [X] T015 [US1] Add shell mode example to Examples section in usage() in bin/clyde
- [X] T016 [P] [US1] Add unit test for --shell flag parsing in tests/unit/clyde.bats
- [X] T017 [P] [US1] Add unit test for help including --shell option in tests/unit/clyde.bats
- [X] T018 [P] [US1] Add unit test for help including CLYDE_SHELL env var in tests/unit/clyde.bats

**Checkpoint**: Shell-only mode fully functional and documented

---

## Phase 4: User Story 2 - X11 Forwarding for Graphical Debugging (Priority: P2)

**Goal**: Enable X11 forwarding from container to host display

**Independent Test**: Run `clyde --x11 --shell` and execute `xclock` or `xeyes`

### Implementation for User Story 2

- [X] T019 [US2] Call validate_x11() when X11_ENABLED=true before run_container() in bin/clyde
- [X] T020 [US2] Add X11 socket mount (-v /tmp/.X11-unix:/tmp/.X11-unix:rw) when X11_ENABLED in run_container() in bin/clyde
- [X] T021 [US2] Add DISPLAY env passthrough (-e DISPLAY=$DISPLAY) when X11_ENABLED in run_container() in bin/clyde
- [X] T022 [US2] Add --x11 to usage() Options section with security note and xhost guidance in bin/clyde
- [X] T023 [US2] Add CLYDE_X11 to Environment Variables section in usage() in bin/clyde
- [X] T024 [US2] Add X11 examples to Examples section in usage() in bin/clyde
- [X] T025 [P] [US2] Add unit test for --x11 flag parsing in tests/unit/clyde.bats
- [X] T026 [P] [US2] Add unit test for validate_x11() error when DISPLAY unset in tests/unit/clyde.bats
- [X] T027 [P] [US2] Add unit test for help including --x11 option in tests/unit/clyde.bats
- [X] T028 [P] [US2] Add unit test for help including CLYDE_X11 env var in tests/unit/clyde.bats

**Checkpoint**: X11 forwarding mode fully functional and documented

---

## Phase 5: User Story 3 - Exec Mode and Combined Options (Priority: P3)

**Goal**: Run single command in container and exit; allow combining --x11 with Claude mode

**Independent Test**: Run `clyde --exec cargo test` and verify command executes in container environment

### Implementation for User Story 3

- [X] T029 [US3] Modify run_container() to pass EXEC_COMMAND array as command when set in bin/clyde
- [X] T030 [US3] Add --exec to usage() Options section in bin/clyde
- [X] T031 [US3] Add --exec examples to Examples section in usage() in bin/clyde
- [X] T032 [US3] Add combined options table to usage() or quick reference in bin/clyde
- [X] T033 [P] [US3] Add unit test for --exec flag parsing in tests/unit/clyde.bats
- [X] T034 [P] [US3] Add unit test for --exec capturing remaining arguments in tests/unit/clyde.bats
- [X] T035 [P] [US3] Add unit test for --shell --exec mutual exclusivity error in tests/unit/clyde.bats
- [X] T036 [P] [US3] Add unit test for --exec without command exits with error in tests/unit/clyde.bats
- [X] T037 [P] [US3] Add unit test for help including --exec option in tests/unit/clyde.bats

**Checkpoint**: Exec mode and all option combinations working

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: Integration tests, cleanup, and final validation

- [X] T038 [P] Add integration test for shell mode environment parity in tests/integration/container.bats
- [X] T039 [P] Add integration test for X11 mode (if running in X11 environment) in tests/integration/container.bats
- [X] T040 [P] Add integration test for exec mode command execution in tests/integration/container.bats
- [X] T041 Run shellcheck on bin/clyde and fix any warnings
- [X] T042 Run full test suite: bats tests/
- [ ] T043 Manual validation: Run quickstart.md scenarios

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies - can start immediately
- **Foundational (Phase 2)**: Depends on Setup completion - BLOCKS all user stories
- **User Story 1 (Phase 3)**: Depends on Phase 2 completion
- **User Story 2 (Phase 4)**: Depends on Phase 2 completion - can run parallel to US1
- **User Story 3 (Phase 5)**: Depends on Phase 2 completion - can run parallel to US1/US2
- **Polish (Phase 6)**: Depends on all user stories being complete

### User Story Dependencies

- **User Story 1 (P1)**: Can start after Foundational (Phase 2) - No dependencies on other stories
- **User Story 2 (P2)**: Can start after Foundational (Phase 2) - Independent of US1
- **User Story 3 (P3)**: Can start after Foundational (Phase 2) - Independent of US1/US2

### Within Each User Story

- Implementation tasks → Help/docs → Tests
- Core functionality before convenience features

### Parallel Opportunities

**Phase 1 parallelism:**
```bash
# T003 and T004 can run in parallel (different functions)
Task: T003 "Add validate_x11() function"
Task: T004 "Add warn() function"
```

**Phase 3 (US1) parallelism:**
```bash
# T016, T017, T018 can run in parallel (different test functions)
Task: T016 "Add unit test for --shell flag parsing"
Task: T017 "Add unit test for help including --shell option"
Task: T018 "Add unit test for help including CLYDE_SHELL env var"
```

**Phase 4 (US2) parallelism:**
```bash
# T025-T028 can run in parallel (different test functions)
Task: T025 "Add unit test for --x11 flag parsing"
Task: T026 "Add unit test for validate_x11() error"
Task: T027 "Add unit test for help including --x11 option"
Task: T028 "Add unit test for help including CLYDE_X11 env var"
```

**Phase 5 (US3) parallelism:**
```bash
# T033-T037 can run in parallel (different test functions)
Task: T033 "Add unit test for --exec flag parsing"
Task: T034 "Add unit test for --exec capturing remaining arguments"
Task: T035 "Add unit test for --shell --exec mutual exclusivity error"
Task: T036 "Add unit test for --exec without command exits with error"
Task: T037 "Add unit test for help including --exec option"
```

**Phase 6 parallelism:**
```bash
# T038-T040 can run in parallel (different test files/functions)
Task: T038 "Integration test for shell mode"
Task: T039 "Integration test for X11 mode"
Task: T040 "Integration test for exec mode"
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup (T001-T004)
2. Complete Phase 2: Foundational (T005-T009)
3. Complete Phase 3: User Story 1 - Shell Mode (T010-T018)
4. **STOP and VALIDATE**: Test `clyde --shell` independently
5. Deploy/demo if ready - shell mode provides immediate debugging value

### Incremental Delivery

1. Complete Setup + Foundational → CLI flags ready
2. Add User Story 1 (Shell Mode) → Test independently → **MVP deliverable**
3. Add User Story 2 (X11 Forwarding) → Test independently → Enhanced debugging
4. Add User Story 3 (Exec Mode) → Test independently → CI/scripted workflows
5. Each story adds value without breaking previous stories

### File Modification Summary

| File | Changes |
|------|---------|
| bin/clyde | Add variables (T001-T002), validation (T003), parsing (T005-T009), run_container logic (T010-T011, T019-T021, T029), help text (T012-T015, T022-T024, T030-T032) |
| tests/unit/clyde.bats | Add ~12 new unit tests (T016-T018, T025-T028, T033-T037) |
| tests/integration/container.bats | Add ~3 integration tests (T038-T040) |

---

## Notes

- [P] tasks = different files or different functions, no dependencies
- [Story] label maps task to specific user story for traceability
- Each user story should be independently completable and testable
- Commit after each phase completion
- Stop at any checkpoint to validate story independently
- No changes needed to docker/nix/user-init.sh (research confirms Docker CMD override is sufficient)
