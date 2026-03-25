# Data Model: Agent Browser Integration

**Branch**: `005-agent-browser` | **Date**: 2026-03-25

## Entities

This feature does not introduce a traditional data model (no database, no API). The "entities" are runtime concepts managed by agent-browser and Docker.

### Browser Session

A named, isolated browser process managed by the agent-browser daemon.

| Attribute | Type | Description |
|-----------|------|-------------|
| session_name | string | Unique identifier (e.g., "agent1", "agent2"). Maps to `--session` flag. |
| socket_path | path | Unix domain socket at `~/.agent-browser/sessions/{name}.sock` |
| pid | integer | OS process ID of the Chrome instance |
| state | enum | `running`, `idle`, `closed` |
| created_at | timestamp | When the session was started |

**Lifecycle**: Created on first command with `--session <name>` → Active while commands execute → Idle between commands (auto-shutdown via `AGENT_BROWSER_IDLE_TIMEOUT_MS`) → Closed on explicit `close` or container exit.

**Constraints**:
- Maximum 4 concurrent sessions (FR-011)
- Session names must be unique within the container
- Sessions are ephemeral — destroyed when the container exits

### Page Snapshot

An accessibility-tree representation of a page, returned by `agent-browser snapshot`.

| Attribute | Type | Description |
|-----------|------|-------------|
| element_refs | map | `@e1`, `@e2`, etc. → element metadata (role, name, value) |
| url | string | Current page URL |
| title | string | Page title |

**No persistence**: Snapshots exist only in the agent's conversation context. They are not stored to disk.

### Screenshot

A rendered image of the browser viewport or full page.

| Attribute | Type | Description |
|-----------|------|-------------|
| file_path | path | Output path (e.g., `/tmp/screenshot.png`) |
| format | enum | `png` (default), `jpeg` |
| scope | enum | `viewport` (default), `full` (full page) |
| annotated | boolean | Whether interactive elements are labeled with `[N]` overlays |

**Storage**: Written to filesystem. Ephemeral within container — not persisted across sessions.

### Browser Configuration

Static configuration shipped with the container image.

| Attribute | Type | Description |
|-----------|------|-------------|
| ignoreHttpsErrors | boolean | `true` — accept all certificates |
| defaultTimeout | integer | Operation timeout in ms (default: 25000) |

**Location**: `/docker/browser/agent-browser.json`, symlinked to workspace at runtime.

## Relationships

```text
Container (1) ──has──> (0..4) Browser Sessions
Browser Session (1) ──produces──> (0..*) Page Snapshots
Browser Session (1) ──produces──> (0..*) Screenshots
Container (1) ──has──> (1) Browser Configuration
```

## State Transitions

```text
Browser Session States:
  [not created] --first command--> [running]
  [running] --idle timeout--> [closed]
  [running] --explicit close--> [closed]
  [running] --container exit--> [destroyed]
  [closed] --new command--> [running]

Browser Feature (container-level):
  [disabled] --launch with --browser--> [enabled]
  [enabled] --container exit--> [disabled]
```
