# Zero-Config Test Fixture

This directory is intentionally empty (no flake.nix or shell.nix).

When running `clyde` from this directory, the default Clyde packages should be used:
- claude (via npm)
- node / npm (via Nix)
- git (via Nix)
- gh (via Nix)
- curl (via Nix)
- ssh (via Nix)

## Test Steps

1. Navigate to this directory
2. Run `clyde`
3. Verify all default commands are available:
   ```bash
   claude --version
   node --version
   git --version
   gh --version
   curl --version
   ssh -V
   ```
