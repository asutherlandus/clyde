{
  description = "Clyde: bounded delegation of autonomous coding work";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };

        # The flake is the source of truth for tooling (AGENTS.md). There is no
        # separate MSRV policy: this toolchain *is* the supported version (D4).
        rustToolchain = pkgs.symlinkJoin {
          name = "clyde-rust-toolchain";
          paths = [
            pkgs.rustc
            pkgs.cargo
            pkgs.clippy
            pkgs.rustfmt
            pkgs.rust-analyzer
          ];
        };

        runtimeRoots = import ./nix/runtime-roots.nix { inherit pkgs; };

        # The microVM guest: kernel, init, and one root image per runtime root
        # (Part 1b). Kept out of the default package so a developer without KVM
        # never pays for a kernel build.
        guest = import ./nix/guest.nix { inherit pkgs runtimeRoots; };

        # Tools every developer and every CI job gets.
        devTools = [
          rustToolchain
          pkgs.cargo-nextest
          pkgs.cargo-deny
          pkgs.cargo-insta
          pkgs.bubblewrap
          # Guest image tooling (Part 1b): `mke2fs -d` builds the source and
          # mission-cache images unprivileged, `mkfs.erofs` builds the read-only
          # roots (R12).
          pkgs.e2fsprogs
          pkgs.erofs-utils
          pkgs.sqlite
          pkgs.git
          pkgs.jq
          pkgs.pkg-config
          pkgs.openssl
          pkgs.ripgrep
          pkgs.util-linux
        ];

        # Shared shell prologue for check derivations: cargo must not try to
        # reach the network or write outside the build directory.
        offlineCargoEnv = ''
          export CARGO_HOME="$TMPDIR/cargo-home"
          export CARGO_TARGET_DIR="$TMPDIR/target"
          export HOME="$TMPDIR/home"
          mkdir -p "$CARGO_HOME" "$CARGO_TARGET_DIR" "$HOME"
        '';

        # Assertion over the workspace runtime-root closure (Phase 0 deliverable 2).
        # Uses closureInfo so this covers the transitive closure, not just the
        # top-level buildEnv, and fails if a project build toolchain is reachable.
        runtimeRootAssertion = pkgs.runCommand "runtime-root-workspace-assertion"
          {
            nativeBuildInputs = [ pkgs.coreutils pkgs.findutils pkgs.gnugrep ];
            closure = pkgs.closureInfo { rootPaths = [ runtimeRoots.workspace ]; };
          } ''
          set -euo pipefail
          forbidden="cargo rustc rustup node npm pnpm yarn chromium firefox google-chrome gpg gpg2 ssh ssh-agent docker podman nix-env apt apt-get dpkg"
          fail=0
          while read -r storePath; do
            for d in "$storePath/bin" "$storePath/sbin" "$storePath/libexec"; do
              [ -d "$d" ] || continue
              for name in $forbidden; do
                if [ -e "$d/$name" ]; then
                  echo "FORBIDDEN: $d/$name is reachable from runtimeRoots.workspace" >&2
                  fail=1
                fi
              done
            done
          done < "$closure/store-paths"
          if [ "$fail" -ne 0 ]; then
            echo "runtimeRoots.workspace must contain no project build toolchain (D6, D17)" >&2
            exit 1
          fi
          # Also assert the tools the workspace environment is required to have.
          for name in sh env grep rg sed find fd jq diff patch python3; do
            if [ ! -e "${runtimeRoots.workspace}/bin/$name" ]; then
              echo "MISSING: runtimeRoots.workspace/bin/$name" >&2
              exit 1
            fi
          done
          touch $out
        '';
      in
      {
        packages = {
          inherit (runtimeRoots) workspace rust fetch;
          runtimeRootManifests = runtimeRoots.manifests;
          default = runtimeRoots.workspace;

          # Part 1b: the microVM guest. `guestVm` is the whole directory the
          # daemon expects under its state directory, so bring-up is a symlink
          # rather than a copy procedure.
          guestKernel = guest.vmlinux;
          guestInit = guest.init;
          # One attribute per image rather than the set, so a single root can be
          # rebuilt on its own and `nix flake check` sees derivations.
          guestImageWorkspace = guest.images.workspace;
          guestImageRust = guest.images.rust;
          guestImageFetch = guest.images.fetch;
          guestVm = guest.vm;
        };

        # Runtime roots are addressed by content identity so a task policy can
        # reference them by store path (D6).
        legacyPackages.runtimeRoots = runtimeRoots;

        devShells.default = pkgs.mkShell {
          name = "clyde-dev";
          packages = devTools;
          # Runtime root store paths are exported so `clyde doctor` and the
          # integration tests can find them without a nix invocation.
          shellHook = ''
            export CLYDE_RUNTIME_ROOT_WORKSPACE="${runtimeRoots.workspace}"
            export CLYDE_RUNTIME_ROOT_RUST="${runtimeRoots.rust}"
            export CLYDE_RUNTIME_ROOT_FETCH="${runtimeRoots.fetch}"
            export CLYDE_BWRAP="${pkgs.bubblewrap}/bin/bwrap"
            export CLYDE_MKE2FS="${pkgs.e2fsprogs}/bin/mke2fs"
            echo "clyde devshell: $(rustc --version)" >&2
          '';
        };

        checks = {
          runtime-root-workspace = runtimeRootAssertion;

          fmt = pkgs.runCommand "clyde-fmt" { nativeBuildInputs = [ rustToolchain ]; } ''
            ${offlineCargoEnv}
            cd ${self}
            cargo fmt --all --check
            touch $out
          '';

          # `clippy`, `nextest` and `deny` need the crate index, which is not
          # available inside a nix build. They run in CI inside `nix develop`
          # (Phase 0 deliverable 9) and are exposed here as apps instead.
        };

        apps = {
          ci-clippy = {
            type = "app";
            program = toString (pkgs.writeShellScript "clyde-ci-clippy" ''
              export PATH=${pkgs.lib.makeBinPath devTools}:$PATH
              exec cargo clippy --all-targets --all-features -- -D warnings
            '');
          };
          ci-test = {
            type = "app";
            program = toString (pkgs.writeShellScript "clyde-ci-test" ''
              export PATH=${pkgs.lib.makeBinPath devTools}:$PATH
              exec cargo nextest run --all-features
            '');
          };
          ci-deny = {
            type = "app";
            program = toString (pkgs.writeShellScript "clyde-ci-deny" ''
              export PATH=${pkgs.lib.makeBinPath devTools}:$PATH
              exec cargo deny check advisories licenses bans sources
            '');
          };
          no-panic-lint = {
            type = "app";
            program = toString (pkgs.writeShellScript "clyde-no-panic-lint" ''
              export PATH=${pkgs.lib.makeBinPath devTools}:$PATH
              exec ${self}/ci/no-panic-lint.sh
            '');
          };
        };
      });
}
