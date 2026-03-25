# Quickstart: Agent Browser Integration

**Branch**: `005-agent-browser` | **Date**: 2026-03-25

## Prerequisites

- Docker 24+ installed and running
- Clyde image built (`docker build -t clyde:local docker/`)

## Usage

### Launch Clyde with browser support

```bash
./bin/clyde --browser
```

This enables the agent-browser tool inside the container with:
- 16GB RAM / 8 CPUs (override with `--memory` / `--cpus`)
- Headless Chrome (no-sandbox, auto-accept certificates)
- Persistent browser cache volume (`clyde-browser-cache`)

### Ask the agent to test a web app

Once inside Clyde, ask the agent:

> Open http://localhost:3000 and verify the login page loads correctly. Fill in the email and password fields and submit the form.

The agent will use the browser tool to:
1. Open the URL
2. Take an accessibility snapshot to identify interactive elements
3. Fill form fields and click buttons
4. Verify the resulting page state

### Combine with other flags

```bash
# Browser + custom project directory
./bin/clyde --browser -- --project /path/to/webapp

# Browser + shell access for debugging
./bin/clyde --browser --shell

# Browser + custom resources
./bin/clyde --browser --memory 32g --cpus 16
```

## Verification

After building the image, verify browser support works:

```bash
# Quick smoke test — should print agent-browser version
./bin/clyde --browser --exec "agent-browser --version"

# Verify Chrome is available
./bin/clyde --browser --exec "agent-browser open https://example.com && agent-browser snapshot"
```

## Without browser support

```bash
# Default launch — no browser overhead
./bin/clyde
```

The agent will not have access to browser tools. If it tries to use them, it will report that browser support is not enabled.

## Troubleshooting

| Issue | Solution |
|-------|----------|
| "agent-browser: command not found" | Rebuild image: `docker build -t clyde:local docker/` |
| Chrome crashes (OOM) | Increase memory: `./bin/clyde --browser --memory 32g` |
| Can't reach localhost app | Ensure your app is running on the host before launching Clyde (host networking is used) |
| SSL errors on HTTPS dev server | Should auto-accept. If not, check that `/docker/browser/agent-browser.json` is symlinked to the workspace |
