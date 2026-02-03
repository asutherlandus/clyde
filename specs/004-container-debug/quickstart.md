# Quickstart: Container Debugging Options

## New Options

### `--shell` - Interactive Shell Mode

Launch the container with a bash prompt instead of Claude Code:

```bash
clyde --shell
```

You get the same environment Claude uses (Nix packages, mounts, permissions).

### `--exec <command>` - Execute Single Command

Run a command in the container and exit:

```bash
clyde --exec cargo test
clyde --exec npm run build
clyde --exec python -m pytest
```

### `--x11` - Enable X11 Forwarding

Allow graphical applications in the container to display on your host:

```bash
# Interactive shell with X11
clyde --x11 --shell

# Run a GUI command
clyde --x11 --exec xclock

# Normal Claude mode with X11 (for Claude-executed GUI commands)
clyde --x11
```

## Combining Options

| Combination | Behavior |
|-------------|----------|
| `clyde --shell` | Interactive bash shell |
| `clyde --exec cmd` | Run `cmd` and exit |
| `clyde --x11` | Claude Code with X11 forwarding |
| `clyde --x11 --shell` | Interactive shell with X11 |
| `clyde --x11 --exec cmd` | Run graphical `cmd` and exit |
| `clyde --shell --exec cmd` | ERROR: mutually exclusive |

## Environment Variables

Set defaults via environment:

```bash
export CLYDE_SHELL=1      # Always start in shell mode
export CLYDE_X11=1        # Always enable X11

# Override with flags
clyde                     # Uses env defaults
clyde --shell             # Explicit shell (redundant if CLYDE_SHELL=1)
```

## Common Workflows

### Debug a failing test

```bash
# Same environment Claude uses
clyde --shell

# Inside container:
cargo test --test integration_test -- --nocapture
```

### Run GUI debugger

```bash
clyde --x11 --shell

# Inside container:
gdb -tui ./my_program
# or
code .  # If VS Code is available via Nix
```

### CI/scripted testing

```bash
# Run tests in Clyde environment, capture exit code
clyde --exec cargo test
echo "Exit code: $?"
```

## Troubleshooting

### X11: "cannot open display"

1. Check DISPLAY is set: `echo $DISPLAY`
2. Ensure X11 socket exists: `ls /tmp/.X11-unix/`
3. If using Wayland, ensure XWayland is running

### Shell: environment differs from Claude

This shouldn't happen - both use the same entrypoint. If it does:
1. Verify Nix packages load: `which git gh node`
2. Check PATH includes npm-global: `echo $PATH | tr ':' '\n' | grep npm`
