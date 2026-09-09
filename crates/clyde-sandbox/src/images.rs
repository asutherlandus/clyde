//! Block images for the microVM backend.
//!
//! Firecracker's device model has **no filesystem passthrough of any kind**
//! (D24): no virtio-fs, no 9p, no shared directory. The only path for host bytes
//! into a guest is a block device, so every mount with a host source becomes an
//! image.
//!
//! The design works because the expensive surface and the per-run surface are
//! different things:
//!
//! | Surface | Lifetime | Built by |
//! |---|---|---|
//! | runtime root | immutable | the flake, as erofs (R12) |
//! | mission cache | per mission | [`ensure_cache_image`], once |
//! | source snapshot | per run | [`build_source_image`], every run |
//! | dependency bundle | content-addressed | [`build_source_image`], cached by content |
//!
//! Every image here is built with `mke2fs -d`, which populates a filesystem from
//! a directory **unprivileged** — no loop mount, no root, no `mount` syscall on
//! the host at all. The cost is proportional to the source tree rather than to
//! `target/`, which is what keeps a per-run image viable.
//!
//! One rule with no exceptions: **a drive attached read-write to a running guest
//! is never touched from the host.** ext4 is not a cluster filesystem, and the
//! guest unmounts its writable drives before the VM stops so the next run reads
//! a clean image.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Result, SandboxError};
use crate::spec::MountPurpose;

/// ext4's volume label field is 16 bytes. A longer label is silently truncated
/// at `mkfs` time, which would make the guest's label lookup fail with no
/// indication of why.
pub const MAX_LABEL_BYTES: usize = 16;

/// Headroom added to a source image over the bytes it must hold.
///
/// Filesystem metadata, directory blocks, and per-file rounding all cost space
/// that a naive sum does not see. A too-small image fails at `mke2fs` time
/// rather than at run time, but failing at all here means a build that cannot
/// start, so the margin is generous.
const SIZE_HEADROOM: f64 = 1.35;

/// Floor for any image. Below roughly this size ext4's own metadata dominates
/// and `mke2fs` starts refusing geometries.
const MIN_IMAGE_BYTES: u64 = 16 << 20;

/// Extra inodes beyond the file count, for the same reason as [`SIZE_HEADROOM`].
const INODE_HEADROOM: u64 = 512;

/// What a built image is, and where it belongs in the guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltImage {
    pub path: PathBuf,
    /// The filesystem label, which is how the guest finds this drive: guest
    /// device names are positional and `drive_id` is invisible in the guest
    /// (D24).
    pub label: String,
    pub target: PathBuf,
    pub writable: bool,
    pub purpose: MountPurpose,
}

/// The label a mount purpose carries into the guest.
///
/// Stable strings rather than generated names, so a guest-side diagnostic names
/// something a reader recognises. Kept inside [`MAX_LABEL_BYTES`].
pub fn label_for(purpose: MountPurpose, index: usize) -> String {
    let base = match purpose {
        MountPurpose::Snapshot => "clyde-work",
        MountPurpose::MissionCache => "clyde-cache",
        MountPurpose::DependencyBundle => "clyde-deps",
        MountPurpose::Scratch => "clyde-scratch",
        _ => "clyde-aux",
    };
    // Several mounts can share a purpose — the mission cache is two directories
    // today — so the index disambiguates without making the common case ugly.
    let label = if index == 0 {
        base.to_owned()
    } else {
        format!("{base}{index}")
    };
    label.chars().take(MAX_LABEL_BYTES).collect()
}

/// How much space a directory tree needs, and how many inodes.
///
/// Walks rather than shelling out to `du`: the numbers are needed as values, and
/// a parse of someone else's output is a failure mode this does not need.
pub fn measure(directory: &Path) -> Result<(u64, u64)> {
    fn walk(path: &Path, bytes: &mut u64, inodes: &mut u64) -> std::io::Result<()> {
        let metadata = std::fs::symlink_metadata(path)?;
        *inodes += 1;
        if metadata.is_dir() {
            for entry in std::fs::read_dir(path)? {
                walk(&entry?.path(), bytes, inodes)?;
            }
        } else {
            *bytes += metadata.len();
        }
        Ok(())
    }

    let mut bytes = 0;
    let mut inodes = 0;
    walk(directory, &mut bytes, &mut inodes)
        .map_err(|source| SandboxError::io(format!("measuring {directory:?}"), source))?;
    Ok((bytes, inodes))
}

/// The image size for a tree of a given measured size.
pub fn sized_for(bytes: u64) -> u64 {
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let padded = (bytes as f64 * SIZE_HEADROOM) as u64;
    padded.max(MIN_IMAGE_BYTES)
}

/// Builds `mke2fs` invocations.
///
/// A value rather than a spawn, so what Clyde runs is assertable in a test —
/// the same reason the namespace backend's argv is a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mke2fsCommand {
    pub program: PathBuf,
    pub arguments: Vec<String>,
}

impl Mke2fsCommand {
    /// An image populated from a directory.
    pub fn from_directory(
        program: &Path,
        image: &Path,
        label: &str,
        source: &Path,
        bytes: u64,
        inodes: u64,
    ) -> Self {
        Self {
            program: program.to_path_buf(),
            arguments: vec![
                "-q".to_owned(),
                "-t".to_owned(),
                "ext4".to_owned(),
                // No reserved blocks: nothing in a guest runs as root needing
                // an emergency reserve, and 5% of a source image is waste.
                "-m".to_owned(),
                "0".to_owned(),
                "-L".to_owned(),
                label.to_owned(),
                "-N".to_owned(),
                inodes.to_string(),
                "-d".to_owned(),
                source.to_string_lossy().to_string(),
                "-F".to_owned(),
                image.to_string_lossy().to_string(),
                format!("{}k", bytes / 1024),
            ],
        }
    }

    /// An empty image of a fixed size.
    ///
    /// The mission cache is created at the mission's `max_cache_bytes`, which
    /// makes the cache budget enforced by the filesystem's size rather than by
    /// accounting (D24).
    pub fn empty(program: &Path, image: &Path, label: &str, bytes: u64) -> Self {
        Self {
            program: program.to_path_buf(),
            arguments: vec![
                "-q".to_owned(),
                "-t".to_owned(),
                "ext4".to_owned(),
                "-m".to_owned(),
                "0".to_owned(),
                "-L".to_owned(),
                label.to_owned(),
                "-F".to_owned(),
                image.to_string_lossy().to_string(),
                format!("{}k", bytes / 1024),
            ],
        }
    }

    fn run(&self) -> Result<()> {
        let output = Command::new(&self.program)
            .args(&self.arguments)
            .output()
            .map_err(|source| match source.kind() {
                std::io::ErrorKind::NotFound => SandboxError::ProgramNotFound {
                    program: self.program.clone(),
                },
                _ => SandboxError::io("running mke2fs", source),
            })?;
        if output.status.success() {
            return Ok(());
        }
        Err(SandboxError::Image {
            detail: format!(
                "mke2fs exited with {}: {}",
                output
                    .status
                    .code()
                    .map_or_else(|| "a signal".to_owned(), |code| code.to_string()),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        })
    }
}

/// Builds a read-only image from a directory tree.
///
/// Used for the per-run source snapshot and for dependency bundles. The tree is
/// read, never modified: the content store is shared by hardlink and writing
/// through one would corrupt it.
pub fn build_source_image(mke2fs: &Path, image: &Path, label: &str, source: &Path) -> Result<()> {
    if let Some(parent) = image.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|source| SandboxError::io("creating the image directory", source))?;
    }
    // An image left over from a previous run of the same sandbox id would be
    // extended rather than replaced, quietly mixing two runs' contents.
    if image.exists() {
        std::fs::remove_file(image)
            .map_err(|source| SandboxError::io("replacing a stale image", source))?;
    }

    let (bytes, inodes) = measure(source)?;
    Mke2fsCommand::from_directory(
        mke2fs,
        image,
        label,
        source,
        sized_for(bytes),
        inodes + INODE_HEADROOM,
    )
    .run()
}

/// Creates the mission cache image if it does not exist yet.
///
/// Never rebuilt per run — that is what keeps the inner loop viable (D24) — and
/// never read by the host. It lives inside the mission's cache directory so that
/// mission closeout deleting that directory deletes the image with it, rather
/// than leaving the largest file Clyde creates behind (D3).
pub fn ensure_cache_image(mke2fs: &Path, image: &Path, label: &str, bytes: u64) -> Result<()> {
    if image.exists() {
        return Ok(());
    }
    if let Some(parent) = image.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|source| SandboxError::io("creating the cache image directory", source))?;
    }
    Mke2fsCommand::empty(mke2fs, image, label, bytes.max(MIN_IMAGE_BYTES)).run()
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

    #[test]
    fn labels_stay_inside_the_ext4_field() {
        for purpose in [
            MountPurpose::Snapshot,
            MountPurpose::MissionCache,
            MountPurpose::DependencyBundle,
            MountPurpose::Scratch,
            MountPurpose::RuntimeRoot,
        ] {
            for index in 0..4 {
                let label = label_for(purpose, index);
                assert!(
                    label.len() <= MAX_LABEL_BYTES,
                    "{label} would be truncated by mke2fs, and the guest would never find it"
                );
            }
        }
    }

    #[test]
    fn labels_distinguish_mounts_that_share_a_purpose() {
        assert_ne!(
            label_for(MountPurpose::MissionCache, 0),
            label_for(MountPurpose::MissionCache, 1),
            "two cache drives must not claim the same identity"
        );
    }

    #[test]
    fn a_tiny_tree_still_gets_a_usable_image() {
        assert_eq!(sized_for(0), MIN_IMAGE_BYTES);
        assert_eq!(sized_for(1024), MIN_IMAGE_BYTES);
    }

    #[test]
    fn a_large_tree_gets_headroom_over_its_contents() {
        let bytes = 4 << 30;
        assert!(
            sized_for(bytes) > bytes,
            "an image sized exactly to its contents has no room for metadata"
        );
    }

    #[test]
    fn measuring_counts_bytes_and_inodes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), vec![0_u8; 4096]).unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), vec![0_u8; 512]).unwrap();
        let (bytes, inodes) = measure(dir.path()).unwrap();
        assert_eq!(bytes, 4096 + 512);
        // root, src, and the two files
        assert_eq!(inodes, 4);
    }

    #[test]
    fn the_populate_command_names_the_label_and_the_source() {
        let command = Mke2fsCommand::from_directory(
            Path::new("/usr/sbin/mke2fs"),
            Path::new("/run/clyde/vm/sb-1.work.img"),
            "clyde-work",
            Path::new("/var/lib/clyde/snapshots/s1"),
            64 << 20,
            2048,
        );
        let rendered = command.arguments.join(" ");
        assert!(rendered.contains("-L clyde-work"));
        assert!(rendered.contains("-d /var/lib/clyde/snapshots/s1"));
        assert!(rendered.contains("-t ext4"));
        assert!(
            rendered.contains("65536k"),
            "the size is passed in blocks mke2fs understands: {rendered}"
        );
    }

    #[test]
    fn the_empty_command_populates_nothing() {
        let command = Mke2fsCommand::empty(
            Path::new("/usr/sbin/mke2fs"),
            Path::new("/var/lib/clyde/missions/m1/cache.img"),
            "clyde-cache",
            1 << 30,
        );
        assert!(
            !command.arguments.iter().any(|argument| argument == "-d"),
            "the mission cache starts empty and is filled by the guest"
        );
    }
}

/// Tests that build real images, and are skipped where `mke2fs` is absent.
///
/// The suite asserts over constructed values everywhere else; these exist
/// because the one thing a value cannot tell you is whether `mke2fs` accepts the
/// arguments, and that is exactly what the first real run would otherwise
/// discover.
#[cfg(test)]
mod image_tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]
    use std::io::{Read as _, Seek as _, SeekFrom};

    use clyde_guest_api::ext4;

    use super::*;

    /// `mke2fs` from the flake devShell, or the host's, or nothing.
    fn mke2fs() -> Option<PathBuf> {
        if let Ok(path) = std::env::var("CLYDE_MKE2FS") {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Some(path);
            }
        }
        ["/usr/sbin/mke2fs", "/sbin/mke2fs", "/usr/bin/mke2fs"]
            .into_iter()
            .map(PathBuf::from)
            .find(|candidate| candidate.is_file())
    }

    /// The label as the *guest* would read it: off the image's superblock,
    /// through the shared parser.
    fn label_on_disk(image: &Path) -> Option<String> {
        let mut file = std::fs::File::open(image).ok()?;
        file.seek(SeekFrom::Start(ext4::SUPERBLOCK_OFFSET)).ok()?;
        let mut superblock = [0_u8; ext4::SUPERBLOCK_BYTES];
        file.read_exact(&mut superblock).ok()?;
        ext4::label_from_superblock(&superblock)
    }

    #[test]
    fn a_source_image_carries_the_label_the_guest_will_look_for() {
        let Some(mke2fs) = mke2fs() else {
            eprintln!("skipped: no mke2fs on this host");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("tree");
        std::fs::create_dir_all(source.join("src")).unwrap();
        std::fs::write(source.join("Cargo.toml"), b"[package]\nname='x'\n").unwrap();
        std::fs::write(source.join("src/main.rs"), b"fn main() {}\n").unwrap();

        let image = dir.path().join("work.img");
        let label = label_for(MountPurpose::Snapshot, 0);
        build_source_image(&mke2fs, &image, &label, &source).expect("the image builds");

        assert!(image.is_file());
        assert_eq!(
            label_on_disk(&image).as_deref(),
            Some(label.as_str()),
            "the guest finds its drives by label, so the host must write the one the job names"
        );
    }

    #[test]
    fn the_cache_image_is_created_once_and_left_alone_afterwards() {
        let Some(mke2fs) = mke2fs() else {
            eprintln!("skipped: no mke2fs on this host");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("cache").join("vm-cache.img");
        let label = label_for(MountPurpose::MissionCache, 0);

        ensure_cache_image(&mke2fs, &image, &label, 32 << 20).expect("the cache image builds");
        let created = std::fs::metadata(&image).unwrap().modified().unwrap();
        assert_eq!(label_on_disk(&image).as_deref(), Some(label.as_str()));

        // The second call must not rebuild it: the mission cache is what makes
        // the inner loop viable, and rebuilding it per run would discard every
        // compilation the mission has done (D24).
        ensure_cache_image(&mke2fs, &image, &label, 32 << 20).expect("the second call is a no-op");
        assert_eq!(
            std::fs::metadata(&image).unwrap().modified().unwrap(),
            created
        );
    }

    #[test]
    fn a_rebuilt_source_image_replaces_rather_than_extends() {
        let Some(mke2fs) = mke2fs() else {
            eprintln!("skipped: no mke2fs on this host");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("tree");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("a.rs"), b"// first\n").unwrap();
        let image = dir.path().join("work.img");

        build_source_image(&mke2fs, &image, "clyde-work", &source).unwrap();
        std::fs::write(source.join("b.rs"), vec![0_u8; 1 << 20]).unwrap();
        build_source_image(&mke2fs, &image, "clyde-work", &source).unwrap();

        assert!(image.is_file(), "the run image is rebuilt every run (D24)");
    }
}
