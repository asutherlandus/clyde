# User Config Test Fixture

This flake.nix simulates a user's global configuration (~/.config/clyde/flake.nix).

## Test Steps

1. Copy this flake to user config location:
   ```bash
   mkdir -p ~/.config/clyde
   cp tests/integration/nix-configs/user-config/flake.nix ~/.config/clyde/
   ```

2. Navigate to a directory WITHOUT a project flake.nix:
   ```bash
   cd tests/integration/nix-configs/zero-config
   clyde
   ```

3. Verify jq is available (from user config):
   ```bash
   jq --version
   ```

4. Verify default packages are also available:
   ```bash
   git --version
   node --version
   ```

## Testing Precedence

1. Create a project with conflicting packages
2. Verify project packages take precedence over user packages
