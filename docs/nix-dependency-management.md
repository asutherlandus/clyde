# Nix Dependency Management

Clyde uses [Nix](https://nixos.org/) to manage development dependencies. This provides reproducible environments where you can customize the available tools without rebuilding the Docker image.

## How It Works

- **Default packages** (git, gh, Node.js, curl, ssh) are provided via Nix
- **Claude Code** is installed via npm at runtime (always latest version)
- **Custom packages** can be added via `flake.nix` or `shell.nix` files
- **Packages are cached** in a Docker volume for fast startup after first run

## Zero Configuration (Default)

By default, Clyde provides:
- `claude` - Claude Code CLI (always latest version via npm)
- `node` / `npm` - Node.js 20 LTS
- `git` - Version control
- `gh` - GitHub CLI
- `curl` - HTTP client
- `ssh` - SSH client

Just run `clyde` - no configuration needed.

## Adding Project-Specific Packages

Create a `flake.nix` in your project root:

```nix
{
  description = "My project environment";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-24.05";
  };

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";  # Change to "aarch64-linux" for ARM
      pkgs = nixpkgs.legacyPackages.${system};
    in {
      devShells.default = pkgs.mkShellNoCC {
        packages = with pkgs; [
          # Add your packages here
          python312
          poetry
        ];
      };
    };
}
```

Now when you run `clyde` from this directory, Python and Poetry are available alongside the defaults.

## Common Examples

### Python Project

```nix
{
  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-24.05";
  outputs = { self, nixpkgs }:
    let pkgs = nixpkgs.legacyPackages.x86_64-linux;
    in {
      devShells.default = pkgs.mkShellNoCC {
        packages = with pkgs; [
          python312
          poetry
          ruff
          mypy
        ];
      };
    };
}
```

### Rust Project

```nix
{
  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-24.05";
  outputs = { self, nixpkgs }:
    let pkgs = nixpkgs.legacyPackages.x86_64-linux;
    in {
      devShells.default = pkgs.mkShellNoCC {
        packages = with pkgs; [
          rustc
          cargo
          rust-analyzer
          clippy
        ];
      };
    };
}
```

### Go Project

```nix
{
  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-24.05";
  outputs = { self, nixpkgs }:
    let pkgs = nixpkgs.legacyPackages.x86_64-linux;
    in {
      devShells.default = pkgs.mkShellNoCC {
        packages = with pkgs; [
          go
          gopls
          golangci-lint
        ];
      };
    };
}
```

### Web Development

```nix
{
  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-24.05";
  outputs = { self, nixpkgs }:
    let pkgs = nixpkgs.legacyPackages.x86_64-linux;
    in {
      devShells.default = pkgs.mkShellNoCC {
        packages = with pkgs; [
          nodejs_20
          pnpm
          typescript
          biome
        ];
      };
    };
}
```

## Adding Global User Packages

To have packages available in ALL Clyde sessions, create `~/.config/clyde/flake.nix`:

```nix
{
  description = "My global Clyde packages";

  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-24.05";

  outputs = { self, nixpkgs }:
    let pkgs = nixpkgs.legacyPackages.x86_64-linux;
    in {
      devShells.default = pkgs.mkShellNoCC {
        packages = with pkgs; [
          # Tools I always want
          jq
          ripgrep
          fd
          bat
          eza
        ];
      };
    };
}
```

These packages will be merged with project-specific packages.

## Using Legacy shell.nix

If you prefer the traditional Nix format, create `shell.nix`:

```nix
{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  packages = with pkgs; [
    python312
    poetry
  ];

  shellHook = ''
    echo "Python environment ready"
  '';
}
```

Clyde will use `shell.nix` if no `flake.nix` is found.

## Finding Packages

Search for packages at: https://search.nixos.org/packages

Example searches:
- "python" → `python312`, `python311`, etc.
- "rust" → `rustc`, `cargo`, `rust-analyzer`
- "postgres" → `postgresql`, `pgcli`

## Nix-Related Commands

```bash
# See what packages would be available
clyde --list-packages

# Show detailed Nix output during startup
clyde --nix-verbose

# Clear package cache (frees disk space)
clyde --nix-gc
```

## Troubleshooting

### First run is slow

The first time you add a new package, Nix downloads it. Subsequent runs use the cache and start in under a second.

### Config has errors

If your flake.nix has syntax errors, Clyde will show the error and ask:

```
Error: flake.nix line 5: unexpected '}'

Proceed with defaults? [Y/n]
```

Press `Y` to continue with default packages, or `n` to fix your config first.

### Clearing the cache

If you need to free disk space or reset the Nix store:

```bash
# Garbage collect unused packages
clyde --nix-gc

# Or remove the volume entirely
docker volume rm clyde-nix-store
```

## Tips

1. **Pin nixpkgs version** - Use a release like `nixos-24.05` for stability, or `nixos-unstable` for latest packages.

2. **Commit flake.lock** - The generated `flake.lock` ensures your team gets identical packages.

3. **Layer your configs** - Use global config for personal tools, project config for project-specific tools.

4. **Use mkShellNoCC** - It's faster than `mkShell` for pure package environments.
