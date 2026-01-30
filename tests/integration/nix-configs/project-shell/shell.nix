{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  name = "project-shell-test";

  packages = with pkgs; [
    fd
  ];

  shellHook = ''
    echo "Project shell.nix environment loaded"
  '';
}
