# The microVM guest (Part 1b): kernel, init, and one root image per runtime
# root.
#
# Image identity follows closure identity (D6, R12): each image is a pure
# function of the runtime-root closure plus the init, so the same closure
# produces the same image and a task policy can keep referring to a runtime root
# by content identity across both backends.
{ pkgs, runtimeRoots }:

let
  lib = pkgs.lib;

  kernelParts = import ./guest-kernel.nix { inherit pkgs; };

  # The guest init. Built from this workspace so host and guest share one
  # version of the job contract rather than two encodings that drift.
  init = pkgs.rustPlatform.buildRustPackage {
    pname = "clyde-init";
    version = "0.1.0";

    src = lib.cleanSourceWith {
      src = lib.cleanSource ../.;
      # `target/` is large and irrelevant, and including it would make image
      # identity depend on whether someone had run cargo locally.
      filter = path: type:
        let name = baseNameOf (toString path);
        in !(type == "directory" && (name == "target" || name == ".git"));
    };

    cargoLock.lockFile = ../Cargo.lock;
    cargoBuildFlags = [ "-p" "clyde-init" ];
    # The workspace's tests need a host with sandbox tooling; the image build is
    # not the place to run them.
    doCheck = false;

    meta.description = "Clyde microVM guest init";
  };

  # /etc inside the guest. Minimal on purpose: the task process drops to a
  # non-root uid (D24), and that uid has to exist for anything that resolves it.
  guestEtc = pkgs.runCommand "clyde-guest-etc" { } ''
    mkdir -p $out/etc
    cat > $out/etc/passwd <<'EOF'
    root:x:0:0:root:/root:/noshell
    builder:x:1000:1000:builder:/work:/noshell
    nobody:x:65534:65534:nobody:/:/noshell
    EOF
    cat > $out/etc/group <<'EOF'
    root:x:0:
    builder:x:1000:
    nogroup:x:65534:
    EOF
    # Resolution never leaves the guest: there is no network device in any
    # configuration (D7 amendment).
    echo "hosts: files" > $out/etc/nsswitch.conf
    echo "clyde-guest" > $out/etc/hostname
  '';

  # One read-only root per runtime root kind, containing that root's closure and
  # the init's closure at their real store paths — store paths are baked into
  # every binary, so they cannot be relocated.
  mkGuestImage = name: root:
    pkgs.runCommand "clyde-guest-image-${name}"
      {
        nativeBuildInputs = [ pkgs.erofs-utils pkgs.coreutils ];
        closure = pkgs.closureInfo { rootPaths = [ root init guestEtc ]; };
        passthru = { runtimeRoot = root; };
      } ''
      set -euo pipefail
      tree="$TMPDIR/tree"
      mkdir -p "$tree"/{proc,sys,dev,tmp,run,work,cache,deps,scratch,nix/store,etc,bin}

      while read -r storePath; do
        cp -a --reflink=auto "$storePath" "$tree/nix/store/"
      done < "$closure/store-paths"

      cp -a ${guestEtc}/etc/. "$tree/etc/"

      # The kernel execs /init; everything else it needs it finds by store path
      # from the job contract.
      ln -s ${init}/bin/clyde-init "$tree/init"
      ln -s ${root} "$tree/runtime-root"

      # An erofs image is immutable by construction, so the whole tree is owned
      # by root and readable by the unprivileged uid the task runs as.
      mkfs.erofs \
        -T 0 \
        -U 00000000-0000-0000-0000-000000000000 \
        --all-root \
        "$out" "$tree"
    '';

  images = {
    workspace = mkGuestImage "workspace" runtimeRoots.workspace;
    rust = mkGuestImage "rust" runtimeRoots.rust;
    fetch = mkGuestImage "fetch" runtimeRoots.fetch;
  };

  # The directory layout the daemon expects under its state directory, so
  # bring-up is one symlink rather than a copy procedure.
  vm = pkgs.runCommand "clyde-guest-vm" { } ''
    mkdir -p $out/rootfs
    ln -s ${kernelParts.vmlinux}/vmlinux $out/vmlinux
    ln -s ${images.workspace} $out/rootfs/workspace.img
    ln -s ${images.rust} $out/rootfs/rust.img
    ln -s ${images.fetch} $out/rootfs/fetch.img
    ln -s ${kernelParts.vmlinux}/config $out/kernel-config
  '';

in
{
  inherit init images vm;
  inherit (kernelParts) kernel vmlinux;
}
