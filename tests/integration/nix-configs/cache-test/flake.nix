{
  description = "Cache test environment with a unique package";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-24.05";
  };

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
    in {
      devShells.default = pkgs.mkShellNoCC {
        name = "cache-test";
        packages = with pkgs; [
          # cowsay is a small, unique package for testing cache persistence
          cowsay
        ];

        shellHook = ''
          echo "Cache test environment loaded"
        '';
      };
    };
}
