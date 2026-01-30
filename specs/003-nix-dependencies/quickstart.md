# Quickstart: Customizing Your Clyde Environment

This guide shows how to add custom packages to your Clyde development environment using Nix.

## Zero Configuration (Default)

By default, Clyde provides:
- `claude` - Claude Code CLI (**always latest version** - updated automatically via npm)
- `node` / `npm` - Node.js 20 LTS
- `git` - Version control
- `gh` - GitHub CLI
- `curl` - HTTP client
- `ssh` - SSH client

Just run `clyde` - no configuration needed.

> **Note**: Claude Code is installed via npm to ensure you always have the latest version. Project dependencies (via `flake.nix`) are pinned for reproducibility. This separation means your tools stay current while your project builds stay consistent.

---

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

---

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

### Data Science

```nix
{
  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-24.05";
  outputs = { self, nixpkgs }:
    let pkgs = nixpkgs.legacyPackages.x86_64-linux;
    in {
      devShells.default = pkgs.mkShellNoCC {
        packages = with pkgs; [
          python312
          python312Packages.pandas
          python312Packages.numpy
          python312Packages.jupyter
          duckdb
        ];
      };
    };
}
```

---

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

---

## Finding Packages

Search for packages at: https://search.nixos.org/packages

Example searches:
- "python" → `python312`, `python311`, etc.
- "rust" → `rustc`, `cargo`, `rust-analyzer`
- "postgres" → `postgresql`, `pgcli`

---

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

---

## Troubleshooting

### See what packages are available

```bash
clyde --list-packages
```

### Show detailed Nix output

```bash
clyde --nix-verbose
```

### Clear package cache

```bash
clyde --nix-gc
```

### Config has errors

If your flake.nix has syntax errors, Clyde will show the error and ask:

```
Error: flake.nix line 5: unexpected '}'

Proceed with defaults? [Y/n]
```

Press `Y` to continue with default packages, or `N` to fix your config first.

### First run is slow

The first time you add a new package, Nix downloads it. Subsequent runs use the cache and start in seconds.

---

## Tips

1. **Pin nixpkgs version** - Use a release like `nixos-24.05` for stability, or `nixos-unstable` for latest packages.

2. **Commit flake.lock** - The generated `flake.lock` ensures your team gets identical packages.

3. **Layer your configs** - Use global config for personal tools, project config for project-specific tools.

4. **Use mkShellNoCC** - It's faster than `mkShell` for pure package environments.
