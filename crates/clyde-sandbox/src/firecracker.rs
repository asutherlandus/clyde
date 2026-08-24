//! The Firecracker backend (D9, Phase 2b).
//!
//! Same `SandboxSpec` contract as the namespace backend, so runtime-root
//! identity and task policy are unchanged across backends. Two properties are
//! structural rather than configured:
//!
//! - **The guest has no network device at all.** Egress, where a profile permits
//!   it, is vsock-bridged to the host proxy. There is no tap device, no bridge,
//!   and no host firewall rule, so networking needs no privilege on either
//!   backend.
//! - **Read-only means read-only.** A drive whose spec says read-only is
//!   attached with `is_read_only: true`; the backend has no path that relaxes
//!   it.

use std::path::PathBuf;
use std::process::Stdio;

use chrono::Utc;
use clyde_core::classification::{BackendKind, IsolationLevel};
use serde::Serialize;

use crate::backend::{
    BoxFuture, ExitStatus, SandboxBackend, SandboxHandle, terminate_child, wait_with_deadline,
};
use crate::error::{Result, SandboxError};
use crate::spec::{MountMode, SandboxSpec};

/// Host paths and images the backend needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirecrackerConfig {
    pub firecracker: PathBuf,
    /// Uncompressed guest kernel image.
    pub kernel: PathBuf,
    /// Directory holding rootfs images built from the runtime-root closures, one
    /// per runtime root kind (D6), so runtime-root identity is stable across
    /// backends.
    pub rootfs_dir: PathBuf,
    /// Directory for API sockets and generated configuration.
    pub runtime_dir: PathBuf,
    /// Host-side vsock socket for the egress bridge.
    pub vsock_dir: PathBuf,
}

/// The Firecracker backend.
#[derive(Debug, Clone)]
pub struct FirecrackerBackend {
    config: FirecrackerConfig,
}

/// Firecracker's boot source configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct BootSource {
    kernel_image_path: String,
    boot_args: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Drive {
    drive_id: String,
    path_on_host: String,
    is_root_device: bool,
    is_read_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct MachineConfig {
    vcpu_count: u8,
    mem_size_mib: u64,
    smt: bool,
    track_dirty_pages: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Vsock {
    guest_cid: u32,
    uds_path: String,
}

/// The generated VM configuration.
///
/// There is deliberately no `network-interfaces` field: the type cannot express
/// a guest network device, so no code path can add one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VmConfig {
    #[serde(rename = "boot-source")]
    boot_source: BootSource,
    drives: Vec<Drive>,
    #[serde(rename = "machine-config")]
    machine_config: MachineConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    vsock: Option<Vsock>,
}

impl VmConfig {
    /// Whether any drive is writable, for the read-only assertions.
    pub fn writable_drives(&self) -> Vec<&str> {
        self.drives
            .iter()
            .filter(|drive| !drive.is_read_only)
            .map(|drive| drive.drive_id.as_str())
            .collect()
    }

    pub fn has_vsock(&self) -> bool {
        self.vsock.is_some()
    }

    pub fn vcpu_count(&self) -> u8 {
        self.machine_config.vcpu_count
    }

    pub fn mem_size_mib(&self) -> u64 {
        self.machine_config.mem_size_mib
    }
}

/// The guest CID Clyde assigns. Host-side is always CID 2.
const GUEST_CID: u32 = 3;

impl FirecrackerBackend {
    pub fn new(config: FirecrackerConfig) -> Self {
        Self { config }
    }

    /// The rootfs image for a spec's runtime root.
    fn rootfs_for(&self, spec: &SandboxSpec) -> PathBuf {
        self.config
            .rootfs_dir
            .join(format!("{}.img", spec.runtime_root_kind.name()))
    }

    /// Builds the VM configuration for a spec.
    ///
    /// Resource limits come from VM configuration rather than cgroups: memory
    /// and vCPU allocation bound the workload by construction, which is why
    /// Phase 2b moots the cgroup-delegation refusal.
    pub fn vm_config(&self, spec: &SandboxSpec) -> Result<VmConfig> {
        spec.validate()?;
        let mut drives = vec![Drive {
            drive_id: "rootfs".to_owned(),
            path_on_host: self.rootfs_for(spec).to_string_lossy().to_string(),
            is_root_device: true,
            is_read_only: true,
        }];
        for (index, mount) in spec.mounts.iter().enumerate() {
            let Some(source) = mount.source.as_ref() else {
                continue;
            };
            match mount.mode {
                MountMode::ReadOnly | MountMode::ReadWrite => drives.push(Drive {
                    drive_id: format!("d{index}-{}", mount.purpose_name()),
                    path_on_host: source.to_string_lossy().to_string(),
                    is_root_device: false,
                    is_read_only: matches!(mount.mode, MountMode::ReadOnly),
                }),
                // Sockets reach the guest over vsock, not as a block device, and
                // tmpfs has no host source.
                MountMode::Socket | MountMode::Tmpfs { .. } => {}
            }
        }

        let mem_size_mib = spec.limits.max_memory_bytes / (1024 * 1024);
        // CPU percent maps to whole vCPUs, rounded up, so a 400% quota becomes
        // four vCPUs rather than a fraction the guest cannot express.
        let vcpu_count =
            u8::try_from(spec.limits.max_cpu_percent.div_ceil(100).max(1)).unwrap_or(1);

        Ok(VmConfig {
            boot_source: BootSource {
                kernel_image_path: self.config.kernel.to_string_lossy().to_string(),
                boot_args: "console=ttyS0 reboot=k panic=1 pci=off i8042.noaux init=/init"
                    .to_owned(),
            },
            drives,
            machine_config: MachineConfig {
                vcpu_count,
                mem_size_mib: mem_size_mib.max(128),
                smt: false,
                track_dirty_pages: false,
            },
            // A vsock device exists only when the profile permits egress. Under
            // profile `none` the guest has no channel of any kind to the host.
            vsock: (!spec.egress.is_none()).then(|| Vsock {
                guest_cid: GUEST_CID,
                uds_path: self
                    .config
                    .vsock_dir
                    .join(format!("{}.vsock", spec.id))
                    .to_string_lossy()
                    .to_string(),
            }),
        })
    }
}

impl crate::spec::Mount {
    /// Short name for a drive identifier.
    fn purpose_name(&self) -> &'static str {
        match self.purpose {
            crate::spec::MountPurpose::Snapshot => "snapshot",
            crate::spec::MountPurpose::MissionCache => "cache",
            crate::spec::MountPurpose::DependencyBundle => "deps",
            crate::spec::MountPurpose::RuntimeRoot => "runtime",
            _ => "aux",
        }
    }
}

impl SandboxBackend for FirecrackerBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Firecracker
    }

    fn isolation_level(&self) -> IsolationLevel {
        IsolationLevel::MicroVm
    }

    fn preflight(&self, spec: &SandboxSpec) -> Result<()> {
        spec.validate()?;
        if !self.config.firecracker.is_file() {
            return Err(SandboxError::ProgramNotFound {
                program: self.config.firecracker.clone(),
            });
        }
        if !std::path::Path::new("/dev/kvm").exists() {
            return Err(SandboxError::IsolationUnavailable {
                required: IsolationLevel::MicroVm,
                detail: "/dev/kvm is absent".to_owned(),
            });
        }
        if !self.config.kernel.is_file() {
            return Err(SandboxError::IsolationUnavailable {
                required: IsolationLevel::MicroVm,
                detail: format!("guest kernel {:?} is absent", self.config.kernel),
            });
        }
        let rootfs = self.rootfs_for(spec);
        if !rootfs.is_file() {
            return Err(SandboxError::RuntimeRoot {
                root: rootfs,
                detail: "no rootfs image was built for this runtime root".to_owned(),
            });
        }
        Ok(())
    }

    fn start<'a>(&'a self, spec: SandboxSpec) -> BoxFuture<'a, Result<SandboxHandle>> {
        Box::pin(async move {
            self.preflight(&spec)?;
            let config = self.vm_config(&spec)?;
            std::fs::create_dir_all(&self.config.runtime_dir)
                .map_err(|error| SandboxError::io("creating the firecracker runtime dir", error))?;
            let config_path = self.config.runtime_dir.join(format!("{}.json", spec.id));
            let encoded = serde_json::to_vec_pretty(&config).map_err(|error| {
                SandboxError::io(
                    "encoding the VM configuration",
                    std::io::Error::other(error),
                )
            })?;
            std::fs::write(&config_path, encoded)
                .map_err(|error| SandboxError::io("writing the VM configuration", error))?;

            let api_socket = self
                .config
                .runtime_dir
                .join(format!("{}.api.sock", spec.id));
            let _ = std::fs::remove_file(&api_socket);

            let mut command = tokio::process::Command::new(&self.config.firecracker);
            command
                .arg("--api-sock")
                .arg(&api_socket)
                .arg("--config-file")
                .arg(&config_path)
                .arg("--no-api");
            command.env_clear();
            command.kill_on_drop(true);
            command.stdin(Stdio::null());
            command.stdout(match spec.stdout_path.as_ref() {
                Some(path) => Stdio::from(
                    std::fs::File::create(path)
                        .map_err(|error| SandboxError::io("creating the guest log", error))?,
                ),
                None => Stdio::null(),
            });
            command.stderr(match spec.stderr_path.as_ref() {
                Some(path) => Stdio::from(
                    std::fs::File::create(path)
                        .map_err(|error| SandboxError::io("creating the guest log", error))?,
                ),
                None => Stdio::null(),
            });

            let started_at = Utc::now();
            let deadline = started_at
                + chrono::Duration::from_std(spec.limits.max_wall_clock.as_duration())
                    .unwrap_or_else(|_| chrono::Duration::hours(1));
            let child = command.spawn().map_err(|error| SandboxError::Spawn {
                id: spec.id.clone(),
                source: error,
            })?;
            tracing::info!(
                sandbox = spec.id,
                vcpus = config.vcpu_count(),
                mem_mib = config.mem_size_mib(),
                vsock = config.has_vsock(),
                "microVM started"
            );
            Ok(SandboxHandle::new(
                spec.id,
                BackendKind::Firecracker,
                started_at,
                deadline,
                child,
            ))
        })
    }

    fn wait<'a>(&'a self, handle: &'a SandboxHandle) -> BoxFuture<'a, Result<ExitStatus>> {
        Box::pin(
            async move { wait_with_deadline(&handle.id, handle.deadline, handle.child()).await },
        )
    }

    fn terminate<'a>(&'a self, handle: &'a SandboxHandle) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { terminate_child(handle.child()).await })
    }
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
    use std::collections::BTreeMap;

    use clyde_core::HumanDuration;
    use clyde_core::classification::{EgressProfile, ResourceLimits, TrustClass};
    use clyde_core::task::RuntimeRootKind;

    use crate::spec::{Mount, MountPurpose, ScratchPolicy};

    fn backend() -> FirecrackerBackend {
        FirecrackerBackend::new(FirecrackerConfig {
            firecracker: PathBuf::from("/usr/bin/firecracker"),
            kernel: PathBuf::from("/var/lib/clyde/vm/vmlinux"),
            rootfs_dir: PathBuf::from("/var/lib/clyde/vm/rootfs"),
            runtime_dir: PathBuf::from("/run/clyde/vm"),
            vsock_dir: PathBuf::from("/run/clyde/vsock"),
        })
    }

    fn spec(egress: EgressProfile) -> SandboxSpec {
        let mut mounts = vec![
            Mount {
                source: Some(PathBuf::from("/var/lib/clyde/snapshots/s1.img")),
                target: PathBuf::from("/work"),
                mode: MountMode::ReadOnly,
                purpose: MountPurpose::Snapshot,
            },
            Mount {
                source: Some(PathBuf::from("/var/lib/clyde/missions/m1/cache.img")),
                target: PathBuf::from("/cache"),
                mode: MountMode::ReadWrite,
                purpose: MountPurpose::MissionCache,
            },
            Mount {
                source: Some(PathBuf::from("/var/lib/clyde/deps/bundle.img")),
                target: PathBuf::from("/deps"),
                mode: MountMode::ReadOnly,
                purpose: MountPurpose::DependencyBundle,
            },
        ];
        if !egress.is_none() {
            mounts.push(Mount {
                source: Some(PathBuf::from("/run/clyde/egress.sock")),
                target: PathBuf::from("/run/clyde/egress.sock"),
                mode: MountMode::Socket,
                purpose: MountPurpose::EgressSocket,
            });
        }
        SandboxSpec {
            id: "sb-vm".to_owned(),
            runtime_root_kind: RuntimeRootKind::Fetch,
            runtime_root: PathBuf::from("/nix/store/fetch-root"),
            runtime_root_closure: vec![PathBuf::from("/nix/store/fetch-root")],
            mounts,
            egress,
            limits: ResourceLimits {
                max_wall_clock: HumanDuration::parse("20m").unwrap(),
                max_memory_bytes: 4 << 30,
                max_cpu_percent: 200,
                max_tasks: 256,
                max_open_files: 4096,
            },
            trust_class: TrustClass::T3,
            min_isolation: IsolationLevel::MicroVm,
            env: BTreeMap::new(),
            argv: vec!["/bin/cargo".to_owned(), "fetch".to_owned()],
            cwd: PathBuf::from("/work"),
            scratch: ScratchPolicy::default(),
            stdout_path: None,
            stderr_path: None,
        }
    }

    #[test]
    fn the_guest_has_no_network_device_in_any_configuration() {
        for egress in [EgressProfile::None, EgressProfile::RustRegistry] {
            let config = backend().vm_config(&spec(egress)).unwrap();
            let json = serde_json::to_string(&config).unwrap();
            assert!(
                !json.contains("network-interfaces") && !json.contains("iface_id"),
                "the guest must have no network device at all: {json}"
            );
        }
    }

    #[test]
    fn vsock_exists_only_when_the_profile_permits_egress() {
        assert!(
            !backend()
                .vm_config(&spec(EgressProfile::None))
                .unwrap()
                .has_vsock()
        );
        assert!(
            backend()
                .vm_config(&spec(EgressProfile::RustRegistry))
                .unwrap()
                .has_vsock()
        );
    }

    #[test]
    fn read_only_mounts_become_read_only_drives() {
        let config = backend().vm_config(&spec(EgressProfile::None)).unwrap();
        let writable = config.writable_drives();
        assert_eq!(
            writable.len(),
            1,
            "only the mission cache is writable: {writable:?}"
        );
        assert!(writable[0].contains("cache"));
        assert!(
            config
                .drives
                .iter()
                .any(|drive| drive.drive_id.contains("snapshot") && drive.is_read_only),
            "snapshots are always read-only"
        );
        assert!(
            config
                .drives
                .iter()
                .any(|drive| drive.drive_id.contains("deps") && drive.is_read_only),
            "the dependency bundle is read-only wherever it is mounted"
        );
    }

    #[test]
    fn the_rootfs_is_the_root_device_and_read_only() {
        let config = backend().vm_config(&spec(EgressProfile::None)).unwrap();
        let root = config
            .drives
            .iter()
            .find(|drive| drive.is_root_device)
            .expect("a root device");
        assert!(root.is_read_only);
        assert!(root.path_on_host.ends_with("fetch.img"));
    }

    #[test]
    fn limits_become_vm_sizing() {
        let config = backend().vm_config(&spec(EgressProfile::None)).unwrap();
        assert_eq!(config.mem_size_mib(), 4096);
        assert_eq!(config.vcpu_count(), 2, "200% CPU quota is two vCPUs");
    }

    #[test]
    fn tiny_limits_still_produce_a_bootable_machine() {
        let mut spec = spec(EgressProfile::None);
        spec.limits.max_memory_bytes = 1 << 20;
        spec.limits.max_cpu_percent = 10;
        let config = backend().vm_config(&spec).unwrap();
        assert!(config.mem_size_mib() >= 128);
        assert_eq!(config.vcpu_count(), 1);
    }

    #[test]
    fn preflight_refuses_without_kvm_or_images() {
        // This host has neither, which is the case the refusal exists for.
        let error = backend()
            .preflight(&spec(EgressProfile::RustRegistry))
            .expect_err("a host without firecracker must refuse");
        assert!(matches!(
            error,
            SandboxError::ProgramNotFound { .. } | SandboxError::IsolationUnavailable { .. }
        ));
    }

    #[test]
    fn the_backend_reports_microvm_isolation() {
        assert_eq!(backend().isolation_level(), IsolationLevel::MicroVm);
        assert_eq!(backend().kind(), BackendKind::Firecracker);
    }
}
