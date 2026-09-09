# The microVM guest kernel (Part 1b).
#
# Built by the flake rather than fetched, for the same reason the runtime roots
# are closures rather than images (D6): the kernel a build task runs under is
# part of the boundary, and a downloaded blob has no identity this project can
# reason about.
#
# Two properties the guest init depends on, and neither is negotiable:
#
#   - **Everything it needs is built in, not a module.** No initramfs exists in
#     this design, so a driver behind `=m` is a driver the guest does not have.
#   - **`virtio_mmio.device` command-line parsing is enabled.** Firecracker has
#     no device tree on x86_64 and appends its virtio devices to the kernel
#     command line; without CONFIG_VIRTIO_MMIO_CMDLINE_DEVICES the guest boots
#     and sees no drives at all.
{ pkgs }:

let
  lib = pkgs.lib;

  # Only what must be *built in* is named here, and nothing is turned off.
  #
  # The temptation is to strip the config down to a microVM minimum. Resist it:
  # nixpkgs' base config is a coherent set, disabling subsystems it enables
  # sends the config generator into questions it cannot answer, and an unused
  # driver in a guest with no such device is inert. What actually matters is
  # that the handful of drivers the guest boots on are `y` rather than `m`,
  # because there is no initramfs to load a module from.
  kernel = pkgs.linuxPackages.kernel.override {
    # `autoModules = false` drops nixpkgs' "enable everything a desktop might
    # want" pass, and `kernelPreferBuiltin` answers every remaining driver
    # question with `y` rather than `m`. Both are load-bearing: there is no
    # initramfs in this design, so a driver behind `=m` is a driver the guest
    # does not have, and the module ordering is not something a config file can
    # fix after the fact — CONFIG_VIRTIO settles to `m` the first time a virtio
    # driver is answered `m`, and every builtin virtio driver asked afterwards
    # is then unanswerable.
    autoModules = false;
    kernelPreferBuiltin = true;
    # A base-config option this file also sets is a warning rather than a build
    # failure; the assertions below check the ones that matter afterwards.
    ignoreConfigErrors = true;
    structuredExtraConfig = lib.mapAttrs (_: lib.mkForce) (with lib.kernel; {
      # Paravirtualised guest. Firecracker's x86_64 loader boots an
      # uncompressed ELF through the PVH entry point.
      HYPERVISOR_GUEST = yes;
      PARAVIRT = yes;
      KVM_GUEST = yes;
      PVH = yes;

      # The device model Firecracker offers, in full. `VIRTIO_MMIO_CMDLINE_DEVICES`
      # is the one whose absence is hardest to diagnose: there is no device tree
      # on x86_64, Firecracker appends `virtio_mmio.device=` arguments to the
      # command line, and without this the guest boots cleanly and sees no
      # drives at all.
      VIRTIO_MMIO = yes;
      VIRTIO_MMIO_CMDLINE_DEVICES = yes;
      VIRTIO_BLK = yes;
      HW_RANDOM_VIRTIO = yes;

      # The guest channel (D7 amendment). AF_VSOCK is the only socket family
      # that reaches the host, and there is no network device in any
      # configuration for the rest of the stack to bind to.
      VSOCKETS = yes;
      VIRTIO_VSOCKETS = yes;
      VIRTIO_VSOCKETS_COMMON = yes;

      # erofs for the runtime root (R12); ext4 for the source snapshot, the
      # mission cache, and dependency bundles.
      EROFS_FS = yes;
      EXT4_FS = yes;
      TMPFS = yes;
      DEVTMPFS = yes;
      DEVTMPFS_MOUNT = yes;

      # Learn-mode observation runs inside the guest from Part 1b (D24).
      FANOTIFY = yes;
      FANOTIFY_ACCESS_PERMISSIONS = yes;

      # The 8250 UART is the only console Firecracker has. It carries early boot
      # diagnostics and nothing else: the log transport is vsock (R11).
      SERIAL_8250 = yes;
      SERIAL_8250_CONSOLE = yes;
    });
  };

  # Firecracker wants the uncompressed ELF. nixpkgs puts it in the `dev` output;
  # this fails the build rather than shipping a kernel the backend cannot boot.
  vmlinux = pkgs.runCommand "clyde-guest-vmlinux"
    {
      nativeBuildInputs = [ pkgs.binutils ];
      inherit (kernel) version;
      meta.description = "Uncompressed guest kernel for the Clyde microVM backend";
    } ''
    mkdir -p $out
    if [ -f "${kernel.dev}/vmlinux" ]; then
      cp "${kernel.dev}/vmlinux" $out/vmlinux
    elif [ -f "${kernel}/vmlinux" ]; then
      cp "${kernel}/vmlinux" $out/vmlinux
    else
      echo "no uncompressed vmlinux in the kernel derivation; Firecracker cannot boot a bzImage-only build" >&2
      exit 1
    fi
    # Firecracker loads the PT_LOAD segments, so debug information is dead
    # weight it reads from disk at every boot. Stripping keeps the ELF
    # Firecracker requires while dropping most of its size.
    chmod 0644 $out/vmlinux
    strip --strip-debug $out/vmlinux
    chmod 0444 $out/vmlinux
    echo "${kernel.version}" > $out/version

    # The three options whose absence produces a guest that boots and then does
    # nothing useful: no drives, no vsock, no root filesystem.
    config="${kernel.configfile}"
    for option in CONFIG_VIRTIO_MMIO_CMDLINE_DEVICES CONFIG_VIRTIO_BLK CONFIG_VIRTIO_VSOCKETS CONFIG_EROFS_FS CONFIG_EXT4_FS; do
      if ! grep -q "^$option=y" "$config"; then
        echo "guest kernel is missing $option=y, which the guest init depends on" >&2
        exit 1
      fi
    done
    cp "$config" $out/config
  '';

in
{
  inherit kernel vmlinux;
}
