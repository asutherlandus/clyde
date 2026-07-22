{
  description = "Clyde base environment - default packages for Claude Code";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-25.05";
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

            # Shell with readline support (for arrow key history navigation)
            bashInteractive

            # Core development tools
            nodejs_22         # Node.js 22 LTS (pi requires >= 22.19.0; nixos-25.05 ships 22.20)
            git               # Version control
            gh                # GitHub CLI

            # Common utilities
            curl              # HTTP client
            just              # Command runner
            openssh           # SSH client
            tmux              # Terminal multiplexer
            gnupg             # GPG for signing commits
          ];

          shellHook = ''
            export CLYDE_ENV="base"
          '';
        };

        devShells.browser = pkgs.mkShellNoCC {
          name = "clyde-browser";

          inputsFrom = [ self.devShells.${system}.default ];

          packages = with pkgs; [
            xvfb-run          # Virtual framebuffer for headless Chrome
          ];

          shellHook = ''
            export CLYDE_ENV="browser"
          '';
        };
      }
    );
}
