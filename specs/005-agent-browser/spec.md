# Feature Specification: Agent Browser Integration

**Feature Branch**: `005-agent-browser`
**Created**: 2026-03-25
**Status**: Draft
**Input**: User description: "Integrate agent-browser into Clyde (Strategy A — self-contained Chrome) for automated web browser-based app testing by the AI agent"

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Agent Browses and Tests a Local Web App (Priority: P1)

A developer launches Clyde in a project directory containing a web application. They ask the AI agent to open the locally running app in a browser, navigate through key pages, fill in forms, click buttons, and verify that the UI behaves as expected. The agent uses the browser tool to perform these actions headlessly inside the container without any manual setup.

**Why this priority**: This is the core value proposition — enabling the AI agent to autonomously test web applications running on the host or inside the container, providing immediate feedback without the developer needing to manually verify UI behavior.

**Independent Test**: Can be fully tested by launching Clyde, starting a local web server, and asking the agent to navigate to it and verify page content. Delivers value as a standalone browser automation capability.

**Acceptance Scenarios**:

1. **Given** a Clyde container is running with browser support enabled, **When** the agent runs a browser open command targeting a URL, **Then** the browser loads the page headlessly and the agent can retrieve a page snapshot.
2. **Given** the agent has a page open, **When** the agent interacts with elements (click, fill, scroll), **Then** the page responds and a new snapshot reflects the updated state.
3. **Given** the container is launched without browser support, **When** the agent or user attempts to invoke the browser tool, **Then** the command prints a clear message that browser support is not enabled and instructs the user to relaunch with `--browser`.

---

### User Story 2 - Opt-In Browser Support via Launch Flag (Priority: P2)

A developer launches Clyde with a dedicated flag to enable browser capabilities. When the flag is not provided, the container behaves exactly as before — no additional overhead, no Chrome installation at runtime, no extra capabilities granted. This keeps the default experience lean.

**Why this priority**: Browser support adds significant container size and grants additional capabilities. Making it opt-in respects the existing user experience and keeps the default container lightweight.

**Independent Test**: Can be tested by launching Clyde with and without the browser flag and verifying the presence or absence of the browser tool.

**Acceptance Scenarios**:

1. **Given** a developer runs Clyde with the browser flag, **When** the container starts, **Then** browser automation tooling is available to the agent.
2. **Given** a developer runs Clyde without the browser flag, **When** the container starts, **Then** no browser-related overhead is incurred and the browser tool is not available.
3. **Given** a developer runs Clyde with the browser flag, **When** they view the help text, **Then** the browser flag and its purpose are documented.

---

### User Story 3 - Persistent Browser Cache Across Sessions (Priority: P3)

A developer uses browser capabilities across multiple Clyde sessions. The browser engine download and cached data persist between sessions so that subsequent launches with browser support are fast, without re-downloading the browser engine each time.

**Why this priority**: The browser engine is large. Re-downloading it every session would create an unacceptable startup delay. Persistence via a named volume keeps repeat launches fast.

**Independent Test**: Can be tested by launching Clyde with browser support twice and verifying the second launch skips the browser engine download.

**Acceptance Scenarios**:

1. **Given** a developer launches Clyde with browser support for the first time, **When** the browser engine is downloaded, **Then** it is stored in a persistent location.
2. **Given** the browser engine was previously downloaded, **When** the developer launches Clyde with browser support again, **Then** the existing engine is reused without re-downloading.

---

### Edge Cases

- What happens when the browser engine download fails mid-way (e.g., network interruption)? The system should detect the incomplete download and retry on next launch.
- What happens when the host port the web app runs on is not reachable from the container? Since Clyde uses host networking, localhost should be reachable — but the agent should report a clear error if the page fails to load.
- What happens when Chrome crashes inside the container (e.g., out of memory)? The agent should receive a clear error and be able to restart the browser session.
- What happens when multiple browser sessions are requested concurrently by sub-agents? Each sub-agent must operate in an isolated browser session so parallel testing workflows are supported without interference between agents.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The Clyde launch script MUST accept a flag to enable browser automation support in the container.
- **FR-002**: When browser support is enabled, the container MUST include a working headless browser automation tool that the AI agent can invoke via shell commands.
- **FR-003**: The browser tool MUST support core interactions: opening URLs, taking page snapshots, clicking elements, filling form fields, scrolling, and running page queries.
- **FR-004**: Browser engine data MUST persist in a named volume to survive container restarts and avoid redundant downloads from manual reinstalls.
- **FR-005**: When browser support is not enabled (default), the container MUST NOT incur additional startup time or resource overhead from browser-related components.
- **FR-012**: When browser support is enabled, the container resource defaults MUST automatically increase to 16GB RAM and 8 CPUs to accommodate the browser engine and concurrent sessions. Users MUST be able to override these defaults with existing `--memory` and `--cpus` flags.
- **FR-006**: The container MUST provide the AI agent with instructions (via a skill definition or equivalent) on how to use the browser tool effectively.
- **FR-007**: The container MUST disable the browser's built-in sandbox, relying on Docker's container isolation as the security boundary. No additional Linux capabilities (such as `SYS_ADMIN`) should be granted to the container.
- **FR-008**: The browser tool MUST work with applications accessible via the container's network (localhost via host networking, and remote URLs). The browser MUST auto-accept all certificates (including self-signed) to support local HTTPS development servers without additional configuration.
- **FR-009**: The system MUST provide clear error messages when browser operations fail (page load timeout, element not found, browser crash).
- **FR-010**: The browser tool MUST support the agent taking screenshots for visual verification of rendered pages, enabling CSS and visual regression testing in addition to the accessibility-tree snapshot workflow.
- **FR-011**: The browser tool MUST support concurrent isolated browser sessions so that multiple sub-agents (agent teams) can browse and test independently without interfering with each other. The skill definition SHOULD advise a maximum of 4 concurrent sessions to stay within resource limits.

### Key Entities

- **Browser Session**: A running headless browser instance managed by the automation daemon. Has a lifecycle (start, interact, close) and is scoped to a single container run.
- **Page Snapshot**: An accessibility-tree representation of the current page state, with element references the agent uses for interaction.
- **Browser Cache Volume**: A persistent storage location for the browser engine binary, surviving across container sessions.

## Clarifications

### Session 2026-03-25

- Q: Should the browser engine be in a separate image, the same image, or downloaded at runtime? → A: Single image with Chrome baked in; the browser flag controls runtime activation only.
- Q: Should resource limits increase when browser support is enabled? → A: Auto-increase to 16GB RAM / 8 CPUs when browser flag is enabled (user can still override with --memory/--cpus).
- Q: What is the upper bound for concurrent browser sessions? → A: Cap at 4 concurrent isolated sessions.
- Q: How should the browser handle self-signed/invalid HTTPS certificates? → A: Auto-accept all certificates (ignore SSL errors), standard for dev/test tooling in an isolated container.

## Assumptions

- Clyde's existing host networking (`--network host`) means the browser inside the container can access any service running on the host's localhost.
- The browser engine is installed at container build time (baked into the single Clyde image), not downloaded at runtime. The `--browser` flag controls runtime activation only — the image size increase applies to all users.
- The AI agent interacts with the browser via CLI commands (not an API or MCP server), consistent with agent-browser's design.
- The skill definition (instructions for the agent) is shipped as part of the container image, not installed at runtime.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: The AI agent can open a locally running web application, interact with UI elements, and report page state — all within a single Clyde session without manual intervention.
- **SC-002**: Launching Clyde with browser support on a warm cache (browser engine already downloaded) adds no more than 5 seconds to container startup time.
- **SC-003**: Launching Clyde without the browser flag shows no measurable difference in startup time or resource usage compared to the current baseline.
- **SC-004**: The agent can complete a basic browser test workflow (open page, snapshot, click, fill, verify) in under 30 seconds for a typical single-page application.
- **SC-005**: 90% of first-time users who enable the browser flag can successfully have the agent test a web page without reading additional documentation beyond the built-in help.
