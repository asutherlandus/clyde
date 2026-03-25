# Tasks: Agent Browser Integration

**Input**: Design documents from `/specs/005-agent-browser/`
**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md

**Tests**: Not explicitly requested in the specification. Test tasks are included in the Polish phase for validation.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Create directory structure and static configuration files

- [X] T001 Create directory `docker/browser/` and `docker/skills/agent-browser/` per implementation plan
- [X] T002 [P] Create default browser configuration at `docker/browser/agent-browser.json` with `{"ignoreHttpsErrors": true, "defaultTimeout": 25000}` per research R4
- [X] T003 [P] Create browser setup script at `docker/browser/setup-browser.sh` — validates agent-browser is installed, symlinks `agent-browser.json` to workspace as `./agent-browser.json`, creates `~/.claude/skills/agent-browser/` directory and copies skill definition into it. Must use `#!/usr/bin/env bash`, `set -euo pipefail`, `local` variables, snake_case functions, and pass shellcheck

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Bake agent-browser and Chrome for Testing into the Docker image so all user stories can function

**⚠️ CRITICAL**: No user story work can begin until this phase is complete

- [X] T004 Add agent-browser and Chrome for Testing layer to `docker/Dockerfile` — after the existing Nix installation layer, add a new `RUN` layer that: (1) installs agent-browser globally via npm (`npm install -g agent-browser@<pinned-version>`), (2) runs `agent-browser install --with-deps` to download Chrome for Testing and system dependencies, (3) cleans up apt cache. Pin the agent-browser version for reproducibility per Constitution Principle II
- [X] T005 Copy `docker/browser/` directory into the image in `docker/Dockerfile` — add a `COPY` instruction to place `docker/browser/` at `/docker/browser/` inside the container (after the browser install layer)
- [X] T006 Copy `docker/skills/` directory into the image in `docker/Dockerfile` — add a `COPY` instruction to place `docker/skills/` at `/docker/skills/` inside the container

**Checkpoint**: Image now contains agent-browser + Chrome + config + skill definition. Not yet activated at runtime.

---

## Phase 3: User Story 1 - Agent Browses and Tests a Local Web App (Priority: P1) 🎯 MVP

**Goal**: The AI agent inside the container can open URLs, take snapshots, interact with elements, take screenshots, and verify page state — all via CLI commands.

**Independent Test**: Launch Clyde with `--browser`, ask the agent to run `agent-browser open https://example.com && agent-browser snapshot -i` and verify element refs are returned.

### Implementation for User Story 1

- [X] T007 [P] [US1] Create the agent-browser skill definition at `docker/skills/agent-browser/SKILL.md` — include: tool name, allowed-tools (`Bash(agent-browser:*)`), trigger phrases ("test this web app", "open a website", "take a screenshot"), core workflow (open → snapshot -i → interact with @refs → re-snapshot), command reference (open, snapshot, click, fill, select, check, press, scroll, screenshot, eval, session management with --session flag), screenshot commands (screenshot, screenshot --full, screenshot --annotate), session isolation instructions (use `--session <name>` for concurrent sub-agents, advise max 4 sessions to stay within resource limits per FR-011), and error handling guidance
- [X] T008 [P] [US1] Modify `docker/entrypoint.sh` — add `export CLYDE_BROWSER="${CLYDE_BROWSER:-}"` to the environment exports block (around line 134-141) so the variable is passed through to user-init.sh
- [X] T009 [US1] Modify `docker/nix/user-init.sh` — add a conditional block that runs when `CLYDE_BROWSER=1`: source `/docker/browser/setup-browser.sh` to activate browser support (symlink config, copy skill definition, restore real agent-browser on PATH). When `CLYDE_BROWSER` is not set or not "1", skip entirely (FR-005: no overhead when disabled). Place this block after the Nix environment activation but before the final exec into Claude Code
- [X] T009b [US1] Create a stub script at `docker/browser/agent-browser-disabled.sh` that prints "Browser support is not enabled. Relaunch Clyde with --browser to enable browser automation." to stderr and exits 1. In `docker/nix/user-init.sh`, when `CLYDE_BROWSER` is NOT "1", symlink this stub to `/usr/local/bin/agent-browser` so that any direct invocation gets a clear error message instead of silently working
- [X] T010 [US1] Add `--browser` flag to argument parsing in `bin/clyde` — declare `BROWSER_ENABLED="${CLYDE_BROWSER:-false}"` with other flag variables (around line 33). Add `--browser)` case in `parse_args()` that sets `BROWSER_ENABLED=true` and shifts. Follow the exact pattern used by `--x11`
- [X] T011 [US1] Add `CLYDE_BROWSER=1` environment variable pass-through in `bin/clyde` `run_container()` — when `BROWSER_ENABLED` is true, add `docker_args+=(-e "CLYDE_BROWSER=1")` to the docker run arguments. Place near the existing environment variable block (around line 638-774)

**Checkpoint**: At this point, `./bin/clyde --browser` launches a container where the agent can use `agent-browser` commands to browse and test web apps. MVP is functional.

---

## Phase 4: User Story 2 - Opt-In Browser Support via Launch Flag (Priority: P2)

**Goal**: The `--browser` flag is a complete, documented opt-in mechanism that adjusts resource defaults and keeps the default experience unchanged.

**Independent Test**: Run `./bin/clyde` (no flag) and verify no browser overhead. Run `./bin/clyde --browser` and verify 16GB/8CPU defaults and browser availability.

### Implementation for User Story 2

- [X] T012 [P] [US2] Add resource limit override in `bin/clyde` `run_container()` — when `BROWSER_ENABLED` is true and the user has NOT explicitly set `--memory` or `--cpus`, override defaults to `MEMORY_LIMIT="16g"` and `CPU_LIMIT="8"` per FR-012. Track whether user explicitly set these flags (add `MEMORY_SET_BY_USER` and `CPU_SET_BY_USER` boolean variables, set to true in the `--memory` and `--cpus` parse_args cases)
- [X] T013 [P] [US2] Add `--browser` flag to `usage()` help text in `bin/clyde` — add a line in the "Container options" section documenting: `--browser          Enable browser automation (agent-browser + Chrome, 16GB/8CPU defaults)`. Include a usage example in the examples section: `./bin/clyde --browser    # Enable browser-based web app testing`
- [X] T014 [US2] Ensure no browser overhead when flag is not set — verify in `docker/nix/user-init.sh` that the conditional browser block is completely skipped when `CLYDE_BROWSER` is unset or empty. The setup-browser.sh script must NOT be sourced, no skill definition copied, no symlinks created. This is a verification/review task against T009

**Checkpoint**: `--browser` is a fully documented opt-in with appropriate resource defaults. Default launch is unaffected.

---

## Phase 5: User Story 3 - Persistent Browser Cache Across Sessions (Priority: P3)

**Goal**: Browser engine cache persists across container restarts via a named Docker volume, avoiding re-downloads.

**Independent Test**: Launch Clyde with `--browser` twice. On second launch, verify `agent-browser` starts instantly without downloading Chrome.

### Implementation for User Story 3

- [X] T015 [US3] Add named volume mount for browser cache in `bin/clyde` `run_container()` — when `BROWSER_ENABLED` is true, add `docker_args+=(-v "clyde-browser-cache:/home/claude/.cache/ms-playwright")` to mount the persistent cache volume. Place near the existing `clyde-nix-store` and `clyde-npm-cache` volume mounts
- [X] T016 [US3] Verify cache detection in `docker/browser/setup-browser.sh` — add a check at the start of the script: if Chrome for Testing already exists in `~/.cache/ms-playwright/`, log "Browser engine cached, skipping download" and skip any install step. If missing (e.g., first run or volume cleared), log "Browser engine not found in cache — using image-baked version" and ensure the baked-in Chrome is available (it should be from the Dockerfile layer)

**Checkpoint**: Browser cache volume persists across sessions. Second launch skips download entirely.

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: Testing, validation, documentation, and quality assurance across all stories

- [X] T017 [P] Add unit tests for `--browser` flag parsing in `tests/unit/clyde.bats` — test cases: (1) `--browser` sets BROWSER_ENABLED=true, (2) default is false, (3) `--browser` combined with `--memory`/`--cpus` respects user overrides, (4) `--browser` combined with `--x11` works, (5) `--browser` combined with `--shell` works
- [X] T018 [P] Create integration test file `tests/integration/browser.bats` — test cases: (1) container with `CLYDE_BROWSER=1` has `agent-browser` on PATH, (2) container without `CLYDE_BROWSER` runs stub that prints "not enabled" message and exits 1, (3) `agent-browser --version` returns successfully inside container, (4) `agent-browser open https://example.com && agent-browser snapshot` returns content, (5) browser cache volume is mounted at correct path, (6) `agent-browser open https://localhost:99999` produces a clear error message (FR-009 validation), (7) no zombie Chrome processes remain after `agent-browser close` (tini cleanup verification)
- [X] T019 Run `shellcheck` on all modified and new scripts: `bin/clyde`, `docker/entrypoint.sh`, `docker/nix/user-init.sh`, `docker/browser/setup-browser.sh` — fix any warnings
- [X] T020 Build Docker image and verify: (1) image builds successfully, (2) `agent-browser --version` works inside container, (3) Chrome for Testing is present at `~/.cache/ms-playwright/`, (4) `/docker/browser/agent-browser.json` exists, (5) `/docker/skills/agent-browser/SKILL.md` exists
- [X] T021 Run quickstart.md validation — execute the smoke test commands from `specs/005-agent-browser/quickstart.md` and verify they pass

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — can start immediately
- **Foundational (Phase 2)**: Depends on Setup completion — BLOCKS all user stories
- **User Story 1 (Phase 3)**: Depends on Foundational (Phase 2) — delivers MVP
- **User Story 2 (Phase 4)**: Depends on US1 (T010, T011 create the flag; US2 refines it)
- **User Story 3 (Phase 5)**: Depends on US1 (T010, T011 create the flag; US3 adds volume)
- **Polish (Phase 6)**: Depends on all user stories being complete

### User Story Dependencies

- **User Story 1 (P1)**: Depends on Phase 2 only. Delivers core browser capability + minimal flag.
- **User Story 2 (P2)**: Builds on US1's `--browser` flag (T010/T011). Adds resource override, help text, zero-overhead guarantee.
- **User Story 3 (P3)**: Builds on US1's `--browser` flag (T010/T011). Adds persistent cache volume.
- **US2 and US3 are independent of each other** — can be done in parallel after US1.

### Within Each User Story

- Config/scripts before integration points
- Entrypoint changes before launch script changes
- Core implementation before refinements

### Parallel Opportunities

- T002 and T003 can run in parallel (different files)
- T007, T008 can start in parallel (different files) once Phase 2 is done
- T012 and T013 can run in parallel within US2 (different sections of bin/clyde)
- T015 and T016 can run in parallel within US3 (different files)
- T017 and T018 can run in parallel (different test files)
- US2 and US3 can run in parallel after US1 completion

---

## Parallel Example: User Story 1

```bash
# After Phase 2 is complete, launch these in parallel (different files):
Task: "T007 - Create skill definition at docker/skills/agent-browser/SKILL.md"
Task: "T008 - Modify docker/entrypoint.sh to export CLYDE_BROWSER"

# Then sequentially:
Task: "T009 - Modify docker/nix/user-init.sh for conditional browser setup"
Task: "T009b - Create agent-browser-disabled stub script"
Task: "T010 - Add --browser flag to bin/clyde parse_args"
Task: "T011 - Add CLYDE_BROWSER=1 env var pass-through in bin/clyde run_container"
```

## Parallel Example: After User Story 1

```bash
# US2 and US3 can proceed in parallel:
# Developer A (US2):
Task: "T012 - Resource limit override in bin/clyde"
Task: "T013 - Help text update in bin/clyde"
Task: "T014 - Verify no-overhead when flag absent"

# Developer B (US3):
Task: "T015 - Named volume mount in bin/clyde"
Task: "T016 - Cache detection in setup-browser.sh"
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup (T001-T003)
2. Complete Phase 2: Foundational (T004-T006) — Chrome baked into image
3. Complete Phase 3: User Story 1 (T007-T011 + T009b) — agent can browse and test
4. **STOP and VALIDATE**: Run `./bin/clyde --browser --exec "agent-browser open https://example.com && agent-browser snapshot -i"`
5. MVP is deliverable at this point

### Incremental Delivery

1. Setup + Foundational → Image ready with Chrome
2. Add User Story 1 → Agent can browse → **MVP deliverable**
3. Add User Story 2 → Polished opt-in experience (resource defaults, help text)
4. Add User Story 3 → Fast repeat launches (cache persistence)
5. Polish → Tests, shellcheck, quickstart validation

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story for traceability
- Each user story should be independently completable and testable
- Commit after each task or logical group
- Stop at any checkpoint to validate story independently
- agent-browser version must be pinned in Dockerfile (Constitution Principle II)
- All bash scripts must pass shellcheck (Constitution Principle III / Shell Scripting Standards)
