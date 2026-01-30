{
  description = "Project flake test with ripgrep";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-24.05";
  };

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
    in {
      devShells.default = pkgs.mkShellNoCC {
        name = "project-flake-test";
        packages = with pkgs; [
          ripgrep
        ];

        shellHook = ''
          echo "Project flake environment loaded"
        '';
      };
    };
}
