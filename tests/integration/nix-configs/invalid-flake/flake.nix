{
  description = "Invalid flake for error handling test";

  # Missing required 'outputs' attribute - this is intentionally broken
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-24.05";
  };

  # Syntax error: missing closing brace
  outputs = { self, nixpkgs }:
    let
      pkgs = nixpkgs.legacyPackages.x86_64-linux;
    in {
      devShells.default = pkgs.mkShellNoCC {
        name = "invalid";
        # Missing closing braces
