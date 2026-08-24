# Runtime roots (D6): each task family's execution root is a nix derivation
# identified by its store path. There is no OCI image, registry, or digest
# pinning pipeline; the closure *is* the pinned identity.
#
# The hard architectural requirement that runtimeRoots.workspace contains no
# project build toolchain is asserted by checks.runtime-root-workspace in
# flake.nix, not by review (D17).
{ pkgs }:

let
  lib = pkgs.lib;

  # Text and code manipulation only. Deliberately no cargo/rustc/node/browser/
  # signing/container tooling: an agent in this environment cannot execute
  # project build code even if it decides to, because the toolchain is absent.
  workspaceTools = [
    pkgs.bashInteractive
    pkgs.coreutils
    pkgs.findutils
    pkgs.gnugrep
    pkgs.gnused
    pkgs.gawk
    pkgs.diffutils
    pkgs.patch
    pkgs.ripgrep
    pkgs.fd
    pkgs.sd
    pkgs.jq
    pkgs.yq-go
    pkgs.tree
    pkgs.less
    pkgs.ast-grep         # structural rewrite tool
    pkgs.python3Minimal   # one-off codemods
    pkgs.gnutar
    pkgs.gzip
    pkgs.which
  ];

  rustTools = [
    pkgs.rustc
    pkgs.cargo
    pkgs.clippy
    pkgs.rustfmt
    pkgs.coreutils
    pkgs.bashInteractive
    pkgs.gnumake
    pkgs.gcc          # linker + cc for build scripts and -sys crates
    pkgs.binutils
    pkgs.pkg-config
  ];

  # Fetch root: cargo plus the network client tooling dependency resolution
  # needs. No project source ever enters this root's sandbox (Phase 3).
  fetchTools = [
    pkgs.cargo
    pkgs.rustc          # cargo shells out to rustc for target detection
    pkgs.coreutils
    pkgs.bashInteractive
    pkgs.cacert
    pkgs.curl
    pkgs.git            # git dependencies are refused, but cargo probes for it
  ];

  mkRoot = name: paths: pkgs.buildEnv {
    name = "clyde-runtime-root-${name}";
    inherit paths;
    pathsToLink = [ "/bin" "/lib" "/libexec" "/share" "/etc" ];
    extraOutputsToInstall = [ "out" "lib" ];
  };

  workspace = mkRoot "workspace" (workspaceTools ++ [ forwarderPlaceholder ]);
  rust = mkRoot "rust" rustTools;
  fetch = mkRoot "fetch" fetchTools;

  # clyde-forward is part of the runtime roots (network egress model): the
  # in-sandbox forwarder must exist inside the sandbox to bridge
  # 127.0.0.1:<port> to the bound unix socket. Until the workspace builds it
  # from this flake, the forwarder is bind-mounted from the host build and this
  # placeholder documents the mount point.
  forwarderPlaceholder = pkgs.runCommand "clyde-forward-mountpoint" { } ''
    mkdir -p $out/libexec/clyde
    cat > $out/libexec/clyde/README <<'EOF'
    clyde-forward is bind-mounted read-only at /run/clyde/clyde-forward by the
    sandbox manager. It is trusted code running in an untrusted netns; see
    docs/network-egress-model.md.
    EOF
  '';

  manifestOf = name: root: pkgs.runCommand "clyde-runtime-root-${name}-manifest"
    { closure = pkgs.closureInfo { rootPaths = [ root ]; }; } ''
    mkdir -p $out
    cp "$closure/store-paths" $out/store-paths
    echo "${root}" > $out/root
    ( cd ${root}/bin 2>/dev/null && ls -1 || true ) > $out/binaries
  '';

in
{
  inherit workspace rust fetch;

  # Each runtime root exposes its store path and closure manifest so a task
  # policy can reference it by content identity (Phase 0 deliverable 1).
  manifests = pkgs.linkFarm "clyde-runtime-root-manifests" [
    { name = "workspace"; path = manifestOf "workspace" workspace; }
    { name = "rust"; path = manifestOf "rust" rust; }
    { name = "fetch"; path = manifestOf "fetch" fetch; }
  ];
}
