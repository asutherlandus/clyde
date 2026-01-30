{
  description = "Clyde base environment - default packages for Claude Code";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-24.05";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
      in {
        devShells.default = pkgs.mkShellNoCC {
          name = "clyde-base";

          packages = with pkgs; [
            # NOTE: claude-code is installed via npm for always-latest version
            # See entrypoint.sh for npm install command

            # Core development tools
            nodejs_20         # Node.js 20 LTS (required for Claude Code)
            git               # Version control
            gh                # GitHub CLI

            # Common utilities
            curl              # HTTP client
            openssh           # SSH client
          ];

          shellHook = ''
            export CLYDE_ENV="base"
          '';
        };
      }
    );
}
