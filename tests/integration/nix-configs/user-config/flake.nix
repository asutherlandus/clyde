{
  description = "User global packages test with jq";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-24.05";
  };

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
    in {
      devShells.default = pkgs.mkShellNoCC {
        name = "user-global-test";
        packages = with pkgs; [
          jq
        ];

        shellHook = ''
          echo "User global environment loaded"
        '';
      };
    };
}
