# Research: Agent Browser Integration

**Branch**: `005-agent-browser` | **Date**: 2026-03-25

## R1: agent-browser Installation in Docker

**Decision**: Install agent-browser via `npm install -g agent-browser` at build time, then run `agent-browser install --with-deps` to download Chrome for Testing and its system dependencies.

**Rationale**: The npm package is a thin wrapper that downloads a prebuilt Rust binary (~10MB) via postinstall. `agent-browser install` downloads Chrome for Testing (~300-600MB) from Google's official channel. `--with-deps` installs the required shared libraries (X11, GTK, font, NSS, etc.) via apt-get. This approach bakes everything into the image at build time, avoiding runtime downloads.

**Alternatives considered**:
- Cargo install from source: Requires Rust toolchain in image (~1GB+), unnecessary since prebuilt binaries exist.
- Brew install: Not available on Ubuntu/Docker.
- Runtime download only: Would add 30-60s to first launch. Rejected per SC-002 (5s startup overhead max).

## R2: Chrome for Testing Cache Location

**Decision**: Chrome for Testing is stored at `~/.cache/ms-playwright/chromium-{version}/` (agent-browser uses Playwright's installer under the hood). Mount a named volume `clyde-browser-cache` at `/home/claude/.cache/ms-playwright/` for persistence.

**Rationale**: Since Chrome is baked into the image at build time, the volume primarily serves as a cache for any runtime updates or additional browser data. The `AGENT_BROWSER_EXECUTABLE_PATH` env var can point to the baked-in Chrome if the cache path changes.

**Alternatives considered**:
- Volume at `~/.agent-browser/`: Only contains config/session data (~KB), not worth a dedicated volume.
- No volume: Chrome is in the image, so persistence is less critical. However, a volume avoids re-downloading if the user manually runs `agent-browser install` inside the container.

## R3: Browser Sandbox Approach

**Decision**: Rely on agent-browser's auto-detection of container environments. It automatically adds `--no-sandbox` when running inside Docker or as root. Additionally, ship a default `agent-browser.json` config as a safety net.

**Rationale**: agent-browser detects Docker/container environments and automatically passes `--no-sandbox` to Chrome. This means no manual flag configuration is needed. A default config file provides a fallback and also sets `ignoreHttpsErrors: true`.

**Alternatives considered**:
- Passing `--args "--no-sandbox"` on every command: Redundant given auto-detection, adds complexity to skill instructions.
- `--cap-add=SYS_ADMIN`: Rejected per clarification Q1 — too broad a capability grant.
- Custom seccomp profile: More secure but brittle across Chrome versions. Over-engineering for a dev tool.

## R4: SSL Certificate Handling

**Decision**: Set `ignoreHttpsErrors: true` in the project-level `agent-browser.json` config file shipped inside the container at `/docker/browser/agent-browser.json`, symlinked to the working directory at runtime.

**Rationale**: The `--ignore-https-errors` CLI flag would need to be passed on every command. A config file applies it globally within the container. This matches the clarification decision to auto-accept all certificates.

**Alternatives considered**:
- CLI flag per command: Too verbose. Agent would need to remember to add it every time.
- Environment variable: No documented env var exists for this setting.
- User-level config at `~/.agent-browser/config.json`: Would work but requires runtime setup. Project-level config is simpler to ship.

## R5: Concurrent Session Isolation

**Decision**: Each sub-agent uses `AGENT_BROWSER_SESSION=<unique-name>` environment variable (or `--session <name>` flag) to get an isolated browser process with its own cookies, localStorage, and element references. Sessions communicate via Unix domain sockets at `~/.agent-browser/sessions/{name}.sock`.

**Rationale**: agent-browser natively supports named sessions. Each session spawns an independent Chrome process. With 4 max concurrent sessions at ~500MB each, 2GB of the 16GB allocation covers browser processes, leaving ~14GB for the agent, dev servers, and OS.

**Alternatives considered**:
- Single shared session: Rejected per clarification Q3 — concurrent sessions required from day 1.
- Docker-in-Docker (one container per session): Massive overhead, complexity. agent-browser's built-in sessions are sufficient.

## R6: Skill Definition Strategy

**Decision**: Ship the agent-browser SKILL.md as part of the container image at `/docker/skills/agent-browser/SKILL.md`. During browser setup (when `CLYDE_BROWSER=1`), copy or symlink it to the Claude Code skills directory at `~/.claude/skills/agent-browser/SKILL.md`.

**Rationale**: The skill definition teaches Claude Code how to use agent-browser (commands, workflows, trigger phrases). Shipping it in the image avoids runtime `npx skills add` calls (which require network access and add latency). The skill is activated only when browser support is enabled.

**Alternatives considered**:
- `npx skills add vercel-labs/agent-browser` at runtime: Requires network, adds startup time, may fail.
- Embedding instructions in CLAUDE.md: Would always be loaded even without browser support. Skill system is the proper mechanism.
- MCP server: agent-browser has no MCP server. CLI-via-skill is the intended integration.

## R7: Opt-In Flag Implementation Pattern

**Decision**: Follow the established `--x11` pattern in `bin/clyde`:
1. Declare `BROWSER_ENABLED="${CLYDE_BROWSER:-false}"` variable
2. Parse `--browser` in `parse_args()` case statement
3. In `run_container()`, when enabled: pass `CLYDE_BROWSER=1` env var, add browser cache volume, override default memory/CPU to 16GB/8CPU (unless user specified)
4. In `user-init.sh`, conditionally run browser setup when `CLYDE_BROWSER=1`

**Rationale**: This mirrors the existing X11 pattern exactly, maintaining codebase consistency. The environment variable bridge (`CLYDE_BROWSER`) follows the same convention as `CLYDE_NIX_VERBOSE`, `CLYDE_NIX_GC`, etc.

**Alternatives considered**:
- Always-on browser: Rejected per FR-005 — must not add overhead when not enabled.
- Separate entrypoint script: Over-engineered. A conditional block in user-init.sh is sufficient.
