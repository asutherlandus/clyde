//! Drive discovery and mounting.
//!
//! Guest device names are positional and `drive_id` is host-side metadata the
//! guest cannot read, so the contract addresses drives by **filesystem label**
//! (D24). The label is read out of the superblock directly rather than through
//! `/dev/disk/by-label`, because that directory is made by udev and this guest
//! has no udev: there is one job to do and a device manager is not part of it.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use clyde_guest_api::{DriveMount, TaskUser, ext4};
use rustix::mount::MountFlags;

use crate::error::{GuestError, Result};

/// Every virtio block device the guest can see, in device-name order.
///
/// `/sys/block` rather than a glob over `/dev`, so a device that exists but has
/// not been created by devtmpfs is a visible discrepancy rather than a silent
/// omission.
pub fn block_devices() -> Result<Vec<PathBuf>> {
    let entries = std::fs::read_dir("/sys/block")
        .map_err(|source| GuestError::io("listing /sys/block", source))?;
    let mut devices: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name())
        .filter_map(|name| name.into_string().ok())
        .filter(|name| name.starts_with("vd"))
        .map(|name| PathBuf::from("/dev").join(name))
        .collect();
    devices.sort();
    Ok(devices)
}

/// Reads an ext filesystem label, or `None` if the device carries no ext
/// superblock.
///
/// The parse itself is in the shared contract crate, so the label the host
/// wrote at `mkfs` time and the label the guest looks for cannot drift apart.
pub fn ext_label(device: &Path) -> Option<String> {
    let mut file = File::open(device).ok()?;
    file.seek(SeekFrom::Start(ext4::SUPERBLOCK_OFFSET)).ok()?;
    let mut superblock = [0_u8; ext4::SUPERBLOCK_BYTES];
    file.read_exact(&mut superblock).ok()?;
    ext4::label_from_superblock(&superblock)
}

/// Maps every label the guest can see to its device.
///
/// A duplicate label is a refusal rather than a choice: two drives claiming the
/// same identity means the host built an image table this guest cannot honour
/// unambiguously, and picking one would be picking at random.
pub fn labelled_devices() -> Result<BTreeMap<String, PathBuf>> {
    let mut found: BTreeMap<String, PathBuf> = BTreeMap::new();
    for device in block_devices()? {
        let Some(label) = ext_label(&device) else {
            continue;
        };
        if let Some(existing) = found.get(&label) {
            return Err(GuestError::DuplicateLabel {
                label,
                first: existing.clone(),
                second: device,
            });
        }
        found.insert(label, device);
    }
    Ok(found)
}

/// Mounts one drive from the job's mount table.
pub fn mount_drive(mount: &DriveMount, device: &Path) -> Result<()> {
    std::fs::create_dir_all(&mount.target)
        .map_err(|source| GuestError::io("creating a mount point", source))?;

    // `nosuid` and `nodev` on every drive without exception: none of these
    // images is a place a setuid binary or a device node could legitimately
    // come from, and both would be attacker-supplied if they appeared.
    let mut flags = MountFlags::NOSUID | MountFlags::NODEV;
    if !mount.writable {
        flags |= MountFlags::RDONLY;
    }

    rustix::mount::mount(
        device,
        &mount.target,
        mount.filesystem.as_str(),
        flags,
        None,
    )
    .map_err(|source| GuestError::Mount {
        target: mount.target.clone(),
        device: device.to_path_buf(),
        filesystem: mount.filesystem,
        source,
    })
}

/// Mounts every drive the job names, in the order given.
pub fn mount_all(mounts: &[DriveMount], user: TaskUser) -> Result<()> {
    let available = labelled_devices()?;
    for mount in mounts {
        let device = available
            .get(&mount.label)
            .ok_or_else(|| GuestError::NoSuchLabel {
                label: mount.label.clone(),
                available: available.keys().cloned().collect(),
            })?;
        mount_drive(mount, device)?;
        if mount.writable {
            // The mission cache image is created empty by the host, so its root
            // directory belongs to whoever ran `mke2fs`. The task runs
            // unprivileged (D24) and would otherwise find it unwritable. One
            // chown of the mount point is enough: everything below it is created
            // by the task itself.
            std::os::unix::fs::chown(&mount.target, Some(user.uid), Some(user.gid))
                .map_err(|source| GuestError::io("taking ownership of a writable mount", source))?;
        }
    }
    Ok(())
}

/// Flushes and unmounts the writable drives.
///
/// This is what makes the mission cache image readable by the next run: an ext4
/// image detached while dirty is an image whose last writes are not there. The
/// host never touches a read-write drive of a running guest, so this is the only
/// place that consistency can be established.
pub fn unmount_writable(mounts: &[DriveMount]) -> Result<()> {
    rustix::fs::sync();
    let mut first_error = None;
    for mount in mounts.iter().filter(|mount| mount.writable).rev() {
        if let Err(source) =
            rustix::mount::unmount(&mount.target, rustix::mount::UnmountFlags::empty())
        {
            first_error.get_or_insert(GuestError::Unmount {
                target: mount.target.clone(),
                source,
            });
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// The pseudo-filesystems the task's toolchain expects.
///
/// `/proc` in particular is not optional: rustc and cargo read it, and a build
/// in a guest without it fails in ways that look like a toolchain bug.
pub fn mount_pseudo() -> Result<()> {
    let pseudo: [(&str, &str, &str, MountFlags); 5] = [
        (
            "proc",
            "/proc",
            "proc",
            MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC,
        ),
        (
            "sysfs",
            "/sys",
            "sysfs",
            MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC,
        ),
        ("devtmpfs", "/dev", "devtmpfs", MountFlags::NOSUID),
        (
            "tmpfs",
            "/tmp",
            "tmpfs",
            MountFlags::NOSUID | MountFlags::NODEV,
        ),
        (
            "tmpfs",
            "/run",
            "tmpfs",
            MountFlags::NOSUID | MountFlags::NODEV,
        ),
    ];

    for (source, target, filesystem, flags) in pseudo {
        std::fs::create_dir_all(target)
            .map_err(|error| GuestError::io("creating a pseudo-filesystem mount point", error))?;
        match rustix::mount::mount(source, target, filesystem, flags, None) {
            Ok(()) => {}
            // The kernel mounts devtmpfs itself when CONFIG_DEVTMPFS_MOUNT is
            // set, and mounting an already-mounted /dev is success, not a
            // failure to recover from.
            Err(rustix::io::Errno::BUSY) => {}
            Err(source) => {
                return Err(GuestError::PseudoMount {
                    target: PathBuf::from(target),
                    filesystem,
                    source,
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]
    use super::*;

    /// A file that looks like an ext4 device carrying `label`.
    ///
    /// The offsets are repeated from the shared parser deliberately: what this
    /// module is responsible for is reading the right bytes off a device, and a
    /// test that borrowed the parser's own arithmetic could not tell a seek bug
    /// from a parse bug.
    fn device_with_label(label: &str) -> (tempfile::TempDir, PathBuf) {
        const MAGIC_AT: usize = 1024 + 0x38;
        const LABEL_AT: usize = 1024 + 0x78;
        let mut bytes = vec![0_u8; 2048];
        bytes[MAGIC_AT..MAGIC_AT + 2].copy_from_slice(&0xEF53_u16.to_le_bytes());
        bytes[LABEL_AT..LABEL_AT + label.len()].copy_from_slice(label.as_bytes());

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vdb");
        std::fs::write(&path, &bytes).unwrap();
        (dir, path)
    }

    #[test]
    fn a_label_is_read_from_the_device() {
        let (_dir, path) = device_with_label("clyde-work");
        assert_eq!(ext_label(&path).as_deref(), Some("clyde-work"));
    }

    #[test]
    fn a_device_without_an_ext_superblock_has_no_label() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vda");
        std::fs::write(&path, vec![0_u8; 2048]).unwrap();
        assert_eq!(
            ext_label(&path),
            None,
            "the erofs root has no ext superblock and must be skipped, not refused"
        );
    }

    #[test]
    fn a_device_too_short_to_hold_a_superblock_is_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vdc");
        std::fs::write(&path, [0_u8; 8]).unwrap();
        assert_eq!(ext_label(&path), None);
    }

    #[test]
    fn a_missing_device_is_not_a_panic() {
        assert_eq!(ext_label(Path::new("/nonexistent/vdz")), None);
    }
}
