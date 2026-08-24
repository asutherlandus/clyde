//! The backend-independent sandbox specification.
//!
//! `SandboxSpec` names what a sandbox must provide: a runtime root closure, a
//! mount table with modes, an egress profile, resource limits, an environment,
//! an argv, and a scratch policy. Anything a backend cannot honour is a
//! `preflight` failure, never a silent relaxation — that rule is what keeps the
//! trait from becoming a place where boundaries quietly weaken (Phase 2a
//! deliverable 1).

use std::collections::BTreeMap;
use std::path::PathBuf;

use clyde_core::classification::{EgressProfile, IsolationLevel, ResourceLimits, TrustClass};
use clyde_core::task::RuntimeRootKind;

/// How a path is exposed inside a sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MountMode {
    /// Read-only bind. Snapshots are *always* read-only, because hardlink
    /// materialisation shares inodes with the content store.
    ReadOnly,
    /// Read-write bind. For a workspace environment this is the edit-scope
    /// enforcement: the writable set is exactly the lease's edit paths (D1).
    ReadWrite,
    /// A unix socket bound read-write. Distinguished from a file bind so the
    /// mount-table audit can show which sockets a sandbox can reach.
    Socket,
    /// An in-memory filesystem, discarded at teardown.
    Tmpfs { size_bytes: u64 },
}

impl MountMode {
    pub fn is_writable(self) -> bool {
        matches!(self, Self::ReadWrite | Self::Socket | Self::Tmpfs { .. })
    }
}

/// One entry in the mount table.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Mount {
    /// Host path. `None` for tmpfs, which has no source.
    pub source: Option<PathBuf>,
    /// Path inside the sandbox.
    pub target: PathBuf,
    pub mode: MountMode,
    /// Why this mount exists, recorded so a mount table is reviewable.
    pub purpose: MountPurpose,
}

/// What a mount is for.
///
/// An enum rather than free text so that the security tests can assert over the
/// *kinds* of thing a sandbox can reach — for example, that no spec the system
/// can generate carries a credential mount (Phase 4 security property).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MountPurpose {
    /// The task's execution root.
    RuntimeRoot,
    /// Lease-scoped writable project paths.
    EditScope,
    /// Lease-scoped read-only project paths.
    ReadScope,
    /// An immutable snapshot of project source.
    Snapshot,
    /// The per-mission writable build cache.
    MissionCache,
    /// The read-only dependency bundle store.
    DependencyBundle,
    /// Repository history, read-only. Never present in a build sandbox (D21).
    GitDirectory,
    /// Advisory guidance and visible policy.
    Guidance,
    /// The actor API socket.
    ActorSocket,
    /// The capability token file, mode 0400.
    SessionToken,
    /// The egress proxy bridge socket.
    EgressSocket,
    /// The Clyde CA certificate. Workspace environments only.
    CaCertificate,
    /// Scratch space.
    Scratch,
    /// A minimal `/dev`, `/proc`, or equivalent.
    SystemPseudo,
    /// The in-sandbox egress forwarder binary.
    Forwarder,
    /// The agent binary, mounted read-only from a host path named in host or
    /// user configuration (D20).
    AgentBinary,
}

impl MountPurpose {
    /// Whether a mount of this purpose would place credential material inside a
    /// sandbox.
    ///
    /// Nothing in the MVP may return `true`; the function exists so the
    /// assertion is a property of the type rather than a review habit.
    pub fn carries_credential(self) -> bool {
        false
    }
}

/// What `/tmp` and scratch look like.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ScratchPolicy {
    pub tmp_bytes: u64,
    pub scratch_bytes: u64,
}

impl Default for ScratchPolicy {
    fn default() -> Self {
        Self {
            tmp_bytes: 512 << 20,
            scratch_bytes: 1 << 30,
        }
    }
}

/// A sandbox specification.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SandboxSpec {
    /// Stable identifier, used for the cgroup scope name and in audit records.
    pub id: String,
    /// Which runtime root this sandbox executes against. Never influenced by
    /// repository configuration (D20).
    pub runtime_root_kind: RuntimeRootKind,
    /// Host path of the runtime root closure.
    pub runtime_root: PathBuf,
    /// Every store path the runtime root's closure requires, bound read-only.
    ///
    /// A nix closure references other store paths, so binding only the root
    /// would produce a sandbox whose binaries cannot find their libraries.
    pub runtime_root_closure: Vec<PathBuf>,
    pub mounts: Vec<Mount>,
    pub egress: EgressProfile,
    pub limits: ResourceLimits,
    pub trust_class: TrustClass,
    pub min_isolation: IsolationLevel,
    /// Environment variables to set. The sandbox environment is otherwise
    /// cleared, so this is the whole environment the process sees.
    pub env: BTreeMap<String, String>,
    /// argv, including the program at index 0. Resolved against the runtime
    /// root, not the host `PATH`.
    pub argv: Vec<String>,
    /// Working directory inside the sandbox.
    pub cwd: PathBuf,
    pub scratch: ScratchPolicy,
    /// Host path for the process's stdout, or `None` to discard.
    pub stdout_path: Option<PathBuf>,
    /// Host path for the process's stderr.
    pub stderr_path: Option<PathBuf>,
}

impl SandboxSpec {
    /// The writable targets inside the sandbox.
    ///
    /// This is the answer to "what could this sandbox have modified", which is
    /// the question mission review asks.
    pub fn writable_targets(&self) -> Vec<&PathBuf> {
        self.mounts
            .iter()
            .filter(|mount| mount.mode.is_writable())
            .map(|mount| &mount.target)
            .collect()
    }

    /// Whether the spec includes a mount of the given purpose.
    pub fn has_purpose(&self, purpose: MountPurpose) -> bool {
        self.mounts.iter().any(|mount| mount.purpose == purpose)
    }

    /// Whether any mount's host source lies inside `directory`.
    ///
    /// Used by the credential-absence tests, which check that no spec the system
    /// can generate reaches into the host home, `~/.ssh`, `~/.gnupg`, or a
    /// container runtime socket.
    pub fn touches_host_path(&self, directory: &std::path::Path) -> bool {
        self.mounts.iter().any(|mount| {
            mount
                .source
                .as_ref()
                .is_some_and(|source| source.starts_with(directory))
        })
    }

    /// Structural checks that hold for every backend.
    ///
    /// Backend-specific capability checks live in each backend's `preflight`;
    /// these are invariants of the spec itself.
    pub fn validate(&self) -> Result<(), SpecViolation> {
        if self.argv.is_empty() {
            return Err(SpecViolation::EmptyArgv);
        }
        if self.id.is_empty() {
            return Err(SpecViolation::EmptyId);
        }
        self.limits
            .validate()
            .map_err(|_| SpecViolation::InvalidLimits)?;
        for mount in &self.mounts {
            if !mount.target.is_absolute() {
                return Err(SpecViolation::RelativeMountTarget {
                    target: mount.target.clone(),
                });
            }
            match (&mount.source, mount.mode) {
                (None, MountMode::Tmpfs { .. }) => {}
                (None, _) => {
                    return Err(SpecViolation::MissingMountSource {
                        target: mount.target.clone(),
                    });
                }
                (Some(source), _) if !source.is_absolute() => {
                    return Err(SpecViolation::RelativeMountSource {
                        path: source.clone(),
                    });
                }
                _ => {}
            }
            if mount.purpose.carries_credential() {
                return Err(SpecViolation::CredentialMount {
                    target: mount.target.clone(),
                });
            }
        }
        // The `.git` directory is never available to a build sandbox, and no
        // configuration or pin can admit it (D21). Enforced here so it cannot be
        // reintroduced by a spec builder.
        if self.trust_class >= TrustClass::T2 && self.has_purpose(MountPurpose::GitDirectory) {
            return Err(SpecViolation::GitInBuildSandbox);
        }
        // Only the workspace environment receives the CA certificate: a sandbox
        // that does not trust the CA cannot be transparently intercepted, which
        // is what keeps the model-api carve-out from spreading (D7 amendment).
        if self.trust_class >= TrustClass::T2 && self.has_purpose(MountPurpose::CaCertificate) {
            return Err(SpecViolation::CaCertificateInBuildSandbox);
        }
        // Profile `none` is implemented by not binding the socket. There is no
        // flag that disables egress, so a socket present under `none` is a bug.
        if self.egress.is_none() && self.has_purpose(MountPurpose::EgressSocket) {
            return Err(SpecViolation::EgressSocketUnderProfileNone);
        }
        if !self.egress.is_none() && !self.has_purpose(MountPurpose::EgressSocket) {
            return Err(SpecViolation::MissingEgressSocket);
        }
        Ok(())
    }
}

/// A structural problem with a spec.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpecViolation {
    #[error("sandbox spec has an empty argv")]
    EmptyArgv,
    #[error("sandbox spec has an empty identifier")]
    EmptyId,
    #[error("sandbox spec has invalid resource limits")]
    InvalidLimits,
    #[error("mount target {target:?} is not absolute")]
    RelativeMountTarget { target: PathBuf },
    #[error("mount source {path:?} is not absolute")]
    RelativeMountSource { path: PathBuf },
    #[error("mount {target:?} has no source and is not a tmpfs")]
    MissingMountSource { target: PathBuf },
    #[error("mount {target:?} would place credential material in a sandbox")]
    CredentialMount { target: PathBuf },
    #[error(".git is never available to a build sandbox (D21)")]
    GitInBuildSandbox,
    #[error("the Clyde CA certificate must not be mounted into a build or fetch sandbox")]
    CaCertificateInBuildSandbox,
    #[error("egress profile is none, but an egress socket is bound")]
    EgressSocketUnderProfileNone,
    #[error("egress profile permits network access, but no egress socket is bound")]
    MissingEgressSocket,
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
    use clyde_core::HumanDuration;

    fn limits() -> ResourceLimits {
        ResourceLimits {
            max_wall_clock: HumanDuration::parse("10m").unwrap(),
            max_memory_bytes: 1 << 30,
            max_cpu_percent: 100,
            max_tasks: 64,
            max_open_files: 1024,
        }
    }

    fn spec(trust_class: TrustClass) -> SandboxSpec {
        SandboxSpec {
            id: "sb-test".to_owned(),
            runtime_root_kind: RuntimeRootKind::Rust,
            runtime_root: PathBuf::from("/nix/store/root"),
            runtime_root_closure: vec![PathBuf::from("/nix/store/root")],
            mounts: vec![Mount {
                source: Some(PathBuf::from("/nix/store/root")),
                target: PathBuf::from("/runtime"),
                mode: MountMode::ReadOnly,
                purpose: MountPurpose::RuntimeRoot,
            }],
            egress: EgressProfile::None,
            limits: limits(),
            trust_class,
            min_isolation: IsolationLevel::NamespaceSandbox,
            env: BTreeMap::new(),
            argv: vec!["/runtime/bin/cargo".to_owned(), "check".to_owned()],
            cwd: PathBuf::from("/work"),
            scratch: ScratchPolicy::default(),
            stdout_path: None,
            stderr_path: None,
        }
    }

    #[test]
    fn a_minimal_spec_validates() {
        assert_eq!(spec(TrustClass::T2).validate(), Ok(()));
    }

    #[test]
    fn empty_argv_and_id_are_rejected() {
        let mut spec = spec(TrustClass::T2);
        spec.argv.clear();
        assert_eq!(spec.validate(), Err(SpecViolation::EmptyArgv));
        let mut spec = self::spec(TrustClass::T2);
        spec.id.clear();
        assert_eq!(spec.validate(), Err(SpecViolation::EmptyId));
    }

    #[test]
    fn relative_paths_are_rejected() {
        let mut spec = spec(TrustClass::T2);
        spec.mounts[0].target = PathBuf::from("relative");
        assert!(matches!(
            spec.validate(),
            Err(SpecViolation::RelativeMountTarget { .. })
        ));
        let mut spec = self::spec(TrustClass::T2);
        spec.mounts[0].source = Some(PathBuf::from("relative"));
        assert!(matches!(
            spec.validate(),
            Err(SpecViolation::RelativeMountSource { .. })
        ));
    }

    #[test]
    fn git_is_never_mountable_into_a_build_sandbox() {
        let mut spec = spec(TrustClass::T2);
        spec.mounts.push(Mount {
            source: Some(PathBuf::from("/srv/project/.git")),
            target: PathBuf::from("/work/.git"),
            mode: MountMode::ReadOnly,
            purpose: MountPurpose::GitDirectory,
        });
        assert_eq!(spec.validate(), Err(SpecViolation::GitInBuildSandbox));

        // The workspace environment does receive `.git`, read-only, so an agent
        // cannot plant hooks but can read history.
        let mut workspace = self::spec(TrustClass::T1);
        workspace.mounts.push(Mount {
            source: Some(PathBuf::from("/srv/project/.git")),
            target: PathBuf::from("/work/.git"),
            mode: MountMode::ReadOnly,
            purpose: MountPurpose::GitDirectory,
        });
        assert_eq!(workspace.validate(), Ok(()));
    }

    #[test]
    fn a_build_sandbox_never_receives_the_ca_certificate() {
        let mut spec = spec(TrustClass::T2);
        spec.mounts.push(Mount {
            source: Some(PathBuf::from("/var/lib/clyde/ca/ca.pem")),
            target: PathBuf::from("/etc/clyde/ca.pem"),
            mode: MountMode::ReadOnly,
            purpose: MountPurpose::CaCertificate,
        });
        assert_eq!(
            spec.validate(),
            Err(SpecViolation::CaCertificateInBuildSandbox)
        );
    }

    #[test]
    fn profile_none_means_no_socket_and_a_profile_means_one() {
        let mut spec = spec(TrustClass::T2);
        spec.mounts.push(Mount {
            source: Some(PathBuf::from("/run/clyde/egress.sock")),
            target: PathBuf::from("/run/clyde/egress.sock"),
            mode: MountMode::Socket,
            purpose: MountPurpose::EgressSocket,
        });
        assert_eq!(
            spec.validate(),
            Err(SpecViolation::EgressSocketUnderProfileNone),
            "the absence of the socket is the absence of egress"
        );

        let mut spec = self::spec(TrustClass::T1);
        spec.egress = EgressProfile::ModelApi;
        assert_eq!(spec.validate(), Err(SpecViolation::MissingEgressSocket));
        spec.mounts.push(Mount {
            source: Some(PathBuf::from("/run/clyde/egress.sock")),
            target: PathBuf::from("/run/clyde/egress.sock"),
            mode: MountMode::Socket,
            purpose: MountPurpose::EgressSocket,
        });
        assert_eq!(spec.validate(), Ok(()));
    }

    #[test]
    fn writable_targets_reports_the_edit_surface() {
        let mut spec = spec(TrustClass::T1);
        spec.mounts.push(Mount {
            source: Some(PathBuf::from("/srv/project/crates/core")),
            target: PathBuf::from("/work/crates/core"),
            mode: MountMode::ReadWrite,
            purpose: MountPurpose::EditScope,
        });
        spec.mounts.push(Mount {
            source: Some(PathBuf::from("/srv/project/docs")),
            target: PathBuf::from("/work/docs"),
            mode: MountMode::ReadOnly,
            purpose: MountPurpose::ReadScope,
        });
        let writable = spec.writable_targets();
        assert_eq!(writable.len(), 1);
        assert_eq!(writable[0], &PathBuf::from("/work/crates/core"));
    }

    #[test]
    fn touches_host_path_finds_credential_directories() {
        let mut spec = spec(TrustClass::T1);
        assert!(!spec.touches_host_path(std::path::Path::new("/home/dev")));
        spec.mounts.push(Mount {
            source: Some(PathBuf::from("/home/dev/project/src")),
            target: PathBuf::from("/work/src"),
            mode: MountMode::ReadWrite,
            purpose: MountPurpose::EditScope,
        });
        assert!(spec.touches_host_path(std::path::Path::new("/home/dev")));
        assert!(!spec.touches_host_path(std::path::Path::new("/home/dev/.ssh")));
    }

    #[test]
    fn no_mount_purpose_carries_a_credential() {
        // Structural assertion: the MVP has no credential-bearing mount, and a
        // future one would have to change this function and fail this test.
        for purpose in [
            MountPurpose::RuntimeRoot,
            MountPurpose::EditScope,
            MountPurpose::ReadScope,
            MountPurpose::Snapshot,
            MountPurpose::MissionCache,
            MountPurpose::DependencyBundle,
            MountPurpose::GitDirectory,
            MountPurpose::Guidance,
            MountPurpose::ActorSocket,
            MountPurpose::SessionToken,
            MountPurpose::EgressSocket,
            MountPurpose::CaCertificate,
            MountPurpose::Scratch,
            MountPurpose::SystemPseudo,
            MountPurpose::Forwarder,
            MountPurpose::AgentBinary,
        ] {
            assert!(!purpose.carries_credential(), "{purpose:?}");
        }
    }
}
