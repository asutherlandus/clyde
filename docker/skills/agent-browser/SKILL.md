# Agent Browser

A headless browser automation tool for testing and interacting with web applications.

## Allowed Tools

- Bash(agent-browser:*)

## Trigger Phrases

- "test this web app"
- "open a website"
- "take a screenshot"
- "check the page"
- "browse to"
- "verify the UI"
- "fill out the form"
- "click the button"

## Core Workflow

1. **Open** a URL: `agent-browser open <url>`
2. **Snapshot** the page to see interactive elements: `agent-browser snapshot -i`
3. **Interact** with elements using `@ref` identifiers from the snapshot
4. **Re-snapshot** to verify the result

## Command Reference

### Navigation

```bash
# Open a URL
agent-browser open <url>

# Close the current session
agent-browser close
```

### Page Inspection

```bash
# Get accessibility snapshot (text content + element refs)
agent-browser snapshot

# Get interactive elements only (forms, buttons, links)
agent-browser snapshot -i

# Evaluate JavaScript in the page
agent-browser eval '<expression>'
```

### Interaction

```bash
# Click an element by ref
agent-browser click @e<N>

# Fill a text input
agent-browser fill @e<N> '<value>'

# Select a dropdown option
agent-browser select @e<N> '<value>'

# Check/uncheck a checkbox
agent-browser check @e<N>
agent-browser uncheck @e<N>

# Press a key or key combination
agent-browser press 'Enter'
agent-browser press 'Control+a'

# Scroll the page
agent-browser scroll down
agent-browser scroll up
agent-browser scroll @e<N>
```

### Screenshots

```bash
# Capture viewport screenshot
agent-browser screenshot

# Capture full-page screenshot
agent-browser screenshot --full

# Capture with element annotations ([N] labels on interactive elements)
agent-browser screenshot --annotate

# Save to specific path
agent-browser screenshot -o /tmp/screenshot.png
```

### Session Management (Concurrent Isolation)

For agent teams or parallel workflows, use named sessions to isolate browser state:

```bash
# Run commands in a named session
agent-browser open <url> --session agent1
agent-browser snapshot -i --session agent1

# Each session has independent cookies, localStorage, and element refs
agent-browser open <url> --session agent2
agent-browser snapshot -i --session agent2

# Close a specific session
agent-browser close --session agent1
```

**Important**: Maximum 4 concurrent sessions to stay within resource limits. Each session uses ~500MB RAM.

## Error Handling

- If `agent-browser open` fails with a connection error, verify the target server is running and accessible from the container (host networking is used)
- If elements are not found after a snapshot, the page may still be loading — wait briefly and re-snapshot
- If Chrome crashes with OOM, increase container memory: `./bin/clyde --browser --memory 32g`
- SSL certificate errors are auto-accepted via configuration — no action needed

## Tips

- Always snapshot after navigation or interaction to see the updated page state
- Use `snapshot -i` to focus on interactive elements (faster, less noise)
- Use `screenshot --annotate` for visual debugging — it overlays element numbers
- For form testing: snapshot → fill fields → click submit → snapshot to verify
- Element refs (`@e1`, `@e2`, etc.) change after page navigation — always re-snapshot
