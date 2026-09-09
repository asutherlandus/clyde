//! The Firecracker backend (D9, D24).
//!
//! The same [`SandboxSpec`] contract as the namespace backend, so runtime-root
//! identity and task policy are unchanged across backends. What differs is that
//! **the file surface is block devices only**: Firecracker's device model has no
//! virtio-fs, no 9p, and no filesystem passthrough of any kind, so every mount
//! with a host source becomes an image ([`crate::images`]).
//!
//! Three properties are structural rather than configured:
//!
//! - **The guest has no network device.** [`VmConfig`] cannot express one, so no
//!   code path can add one. Egress, where a profile permits it, is a vsock
//!   bridge rather than a NIC.
//! - **Every VM has a vsock device, and an egress *listener* only sometimes.**
//!   Logs have nowhere else to go, so the device is unconditional; what varies
//!   is whether the host binds anything on the egress port ([D7 amendment]).
//! - **Read-only means read-only.** A drive whose spec says read-only is
//!   attached with `is_read_only: true`, and there is no path that relaxes it.
//!
//! [D7 amendment]: ../../../docs/builder/decisions.md

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use chrono::Utc;
use clyde_core::classification::{BackendKind, IsolationLevel};
use clyde_guest_api::{
    DriveMount, EgressBridge, Filesystem, Job, PROTOCOL_VERSION, Report, TaskUser, port,
};
use serde::Serialize;

use crate::backend::{
    BoxFuture, ExitStatus, SandboxBackend, SandboxHandle, terminate_child, wait_with_deadline,
};
use crate::error::{Result, SandboxError};
use crate::guest_channel::{ChannelPaths, GuestChannel, GuestOutcome, LogTargets, ServedRun};
use crate::images::{self, BuiltImage};
use crate::spec::{MountPurpose, SandboxSpec};

/// Where the mission cache is mounted in the guest.
///
/// One image rather than one per directory: the cache is the expensive surface
/// and it is never rebuilt per run (D24). `CARGO_TARGET_DIR` and `CARGO_HOME`
/// live inside it, and cargo creates both.
const GUEST_CACHE: &str = "/cache";

/// The mission cache image's name inside the mission cache directory.
///
/// Inside, so that mission closeout deleting the cache directory deletes the
/// largest file Clyde creates along with it (D3).
const CACHE_IMAGE_NAME: &str = "vm-cache.img";

/// Host paths and images the backend needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirecrackerConfig {
    pub firecracker: PathBuf,
    /// `mke2fs`, from `e2fsprogs`. Builds an image from a directory
    /// unprivileged — no loop mount and no root.
    pub mke2fs: PathBuf,
    /// Uncompressed guest kernel image.
    pub kernel: PathBuf,
    /// Directory holding the erofs root images built from the runtime-root
    /// closures, one per runtime root kind (D6, R12).
    pub rootfs_dir: PathBuf,
    /// Directory for API sockets, generated configuration, and per-run images.
    pub runtime_dir: PathBuf,
    /// Host-side directory for the vsock sockets.
    pub vsock_dir: PathBuf,
}

/// The Firecracker backend.
#[derive(Debug, Clone)]
pub struct FirecrackerBackend {
    config: FirecrackerConfig,
    /// Runs in flight, so `wait` can collect what `start` set up.
    runs: Arc<tokio::sync::Mutex<HashMap<String, ServedRun>>>,
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
    /// `Unsafe` for the mission cache: it is disposable by design, so paying
    /// host `fsync` for it buys nothing (D24).
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_type: Option<&'static str>,
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
    /// Present in every configuration. Logs have nowhere else to go: the 8250
    /// UART is the only console and it stalls under build output ([D7
    /// amendment]).
    ///
    /// [D7 amendment]: ../../../docs/builder/decisions.md
    vsock: Vsock,
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

    pub fn drive_count(&self) -> usize {
        self.drives.len()
    }

    pub fn vcpu_count(&self) -> u8 {
        self.machine_config.vcpu_count
    }

    pub fn mem_size_mib(&self) -> u64 {
        self.machine_config.mem_size_mib
    }

    pub fn boot_args(&self) -> &str {
        &self.boot_source.boot_args
    }
}

/// How many drives one VM may carry.
///
/// Firecracker discovers virtio devices from the kernel command line on x86_64
/// and the count is bounded by the legacy IRQ range available for MMIO. This
/// budget is deliberately below any plausible ceiling and **has not been
/// confirmed against a deployed Firecracker**; the point is that exceeding it is
/// a named refusal rather than a boot that fails with no drives.
const MAX_DRIVES: usize = 8;

/// The guest CID Clyde assigns. Host-side is always CID 2.
const GUEST_CID: u32 = clyde_guest_api::GUEST_CID;

/// Everything one run needs, computed before anything is spawned.
///
/// A value rather than a sequence of side effects, so a test can assert on the
/// configuration and the job a spec produces without a kernel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedRun {
    pub vm_config: VmConfig,
    pub job: Job,
    pub images: Vec<BuiltImage>,
    pub channel: ChannelPaths,
}

impl FirecrackerBackend {
    pub fn new(config: FirecrackerConfig) -> Self {
        Self {
            config,
            runs: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    /// The erofs root image for a spec's runtime root.
    fn rootfs_for(&self, spec: &SandboxSpec) -> PathBuf {
        self.config
            .rootfs_dir
            .join(format!("{}.img", spec.runtime_root_kind.name()))
    }

    /// Where a per-run image for this sandbox lives.
    fn run_image(&self, spec: &SandboxSpec, name: &str) -> PathBuf {
        self.config
            .runtime_dir
            .join(format!("{}.{name}.img", spec.id))
    }

    /// Builds every image the spec's mount table needs.
    ///
    /// The mission cache is created once and reused; the source snapshot is
    /// rebuilt every run, which is the trade that keeps the inner loop viable
    /// (D24). Nothing here reads a running guest's image: images are built
    /// before the VM starts and only the guest writes to them afterwards.
    fn build_images(&self, spec: &SandboxSpec) -> Result<Vec<BuiltImage>> {
        let mut built = Vec::new();
        let mut cache_done = false;

        for (index, mount) in spec.mounts.iter().enumerate() {
            let Some(source) = mount.source.as_ref() else {
                continue;
            };
            match mount.purpose {
                MountPurpose::Snapshot => {
                    let image = self.run_image(spec, "work");
                    let label = images::label_for(MountPurpose::Snapshot, 0);
                    images::build_source_image(&self.config.mke2fs, &image, &label, source)?;
                    built.push(BuiltImage {
                        path: image,
                        label,
                        target: mount.target.clone(),
                        writable: false,
                        purpose: mount.purpose,
                    });
                }
                MountPurpose::MissionCache => {
                    // Every mission-cache mount is a directory inside one cache
                    // root; the guest gets that root as a single image.
                    if cache_done {
                        continue;
                    }
                    cache_done = true;
                    let cache_root = source.parent().unwrap_or(source);
                    let image = cache_root.join(CACHE_IMAGE_NAME);
                    let label = images::label_for(MountPurpose::MissionCache, 0);
                    images::ensure_cache_image(
                        &self.config.mke2fs,
                        &image,
                        &label,
                        cache_bytes(spec),
                    )?;
                    built.push(BuiltImage {
                        path: image,
                        label,
                        target: PathBuf::from(GUEST_CACHE),
                        writable: true,
                        purpose: mount.purpose,
                    });
                }
                MountPurpose::DependencyBundle => {
                    let image = self.run_image(spec, "deps");
                    let label = images::label_for(MountPurpose::DependencyBundle, 0);
                    images::build_source_image(&self.config.mke2fs, &image, &label, source)?;
                    built.push(BuiltImage {
                        path: image,
                        label,
                        target: mount.target.clone(),
                        writable: false,
                        purpose: mount.purpose,
                    });
                }
                // Sockets reach the guest over vsock, not as a block device;
                // tmpfs has no host source; and the runtime root arrives as the
                // erofs root device rather than as a mount.
                MountPurpose::EgressSocket
                | MountPurpose::RuntimeRoot
                | MountPurpose::Scratch
                | MountPurpose::SystemPseudo => {}
                other => {
                    // A mount purpose with a host source that this backend has
                    // no representation for is a refusal, not something to drop
                    // quietly: dropping it would run the task with less than
                    // the spec described.
                    return Err(SandboxError::Unsupported {
                        backend: "firecracker",
                        requirement: format!(
                            "mount {index} ({other:?}) has a host source and no block-device representation"
                        ),
                    });
                }
            }
        }
        Ok(built)
    }

    /// Builds the VM configuration and the job for a spec, creating the images
    /// it needs.
    pub fn prepare(&self, spec: &SandboxSpec) -> Result<PreparedRun> {
        spec.validate()?;
        let built = self.build_images(spec)?;
        self.plan(spec, built)
    }

    /// The configuration and job a spec plus its images produce.
    ///
    /// Pure: no image is built and nothing is spawned, so every property that
    /// matters — no network device, read-only drives, the guest's mount table —
    /// is assertable over a value on a host with no KVM at all.
    pub fn plan(&self, spec: &SandboxSpec, built: Vec<BuiltImage>) -> Result<PreparedRun> {
        let mut drives = vec![Drive {
            drive_id: "rootfs".to_owned(),
            path_on_host: self.rootfs_for(spec).to_string_lossy().to_string(),
            is_root_device: true,
            is_read_only: true,
            cache_type: None,
        }];
        for (index, image) in built.iter().enumerate() {
            drives.push(Drive {
                drive_id: format!("d{index}-{}", image.label),
                path_on_host: image.path.to_string_lossy().to_string(),
                is_root_device: false,
                is_read_only: !image.writable,
                cache_type: image
                    .writable
                    .then_some("Unsafe")
                    .filter(|_| matches!(image.purpose, MountPurpose::MissionCache)),
            });
        }

        if drives.len() > MAX_DRIVES {
            // Budgeted rather than discovered by a boot failure: Firecracker's
            // virtio device count is bounded by the legacy IRQ range available
            // for MMIO, and a VM over that ceiling fails at boot with nothing
            // that names the mount table as the cause.
            return Err(SandboxError::Unsupported {
                backend: "firecracker",
                requirement: format!(
                    "{} drives exceeds the {MAX_DRIVES}-drive budget for virtio-mmio devices",
                    drives.len()
                ),
            });
        }

        let mem_size_mib = spec.limits.max_memory_bytes / (1024 * 1024);
        // CPU percent maps to whole vCPUs, rounded up, so a 400% quota becomes
        // four vCPUs rather than a fraction the guest cannot express.
        let vcpu_count =
            u8::try_from(spec.limits.max_cpu_percent.div_ceil(100).max(1)).unwrap_or(1);

        let channel = ChannelPaths::new(&self.config.vsock_dir, &spec.id);

        let vm_config = VmConfig {
            boot_source: BootSource {
                kernel_image_path: self.config.kernel.to_string_lossy().to_string(),
                // `root=/dev/vda` is the erofs runtime root; `init=/init` is the
                // guest init; `reboot=k` is how the guest stops the VM, since
                // Firecracker's i8042 is a reset controller and a halt would
                // leave the VM running with nothing in it.
                boot_args:
                    "console=ttyS0 reboot=k panic=1 pci=off i8042.noaux root=/dev/vda ro rootfstype=erofs init=/init"
                        .to_owned(),
            },
            drives,
            machine_config: MachineConfig {
                vcpu_count,
                mem_size_mib: mem_size_mib.max(128),
                smt: false,
                track_dirty_pages: false,
            },
            vsock: Vsock {
                guest_cid: GUEST_CID,
                uds_path: channel.uds.to_string_lossy().to_string(),
            },
        };

        Ok(PreparedRun {
            job: job_for(spec, &built),
            vm_config,
            images: built,
            channel,
        })
    }
}

/// The mission cache image's size.
///
/// Created at the mission's cache budget so the budget is enforced by the
/// filesystem's size rather than by accounting (D24). The spec carries no cache
/// budget of its own, so the memory limit is used as a proportional stand-in
/// until the mission's `max_cache_bytes` is threaded through.
fn cache_bytes(spec: &SandboxSpec) -> u64 {
    const MINIMUM: u64 = 8 << 30;
    spec.limits.max_memory_bytes.saturating_mul(4).max(MINIMUM)
}

/// The job a spec and its images produce.
fn job_for(spec: &SandboxSpec, images: &[BuiltImage]) -> Job {
    let mounts = images
        .iter()
        .map(|image| DriveMount {
            label: image.label.clone(),
            target: image.target.clone(),
            filesystem: Filesystem::Ext4,
            writable: image.writable,
        })
        .collect();

    Job {
        version: PROTOCOL_VERSION,
        id: spec.id.clone(),
        argv: spec.argv.clone(),
        env: spec.env.clone().into_iter().collect(),
        cwd: spec.cwd.clone(),
        mounts,
        // The task runs as the uid that built the images, so the files it finds
        // are owned by the identity it runs under, and never as guest root
        // (D24).
        user: TaskUser {
            uid: rustix::process::getuid().as_raw(),
            gid: rustix::process::getgid().as_raw(),
        },
        egress: (!spec.egress.is_none()).then_some(EgressBridge {
            listen_port: 8118,
            host_port: port::EGRESS,
        }),
        timeout_seconds: spec.limits.max_wall_clock.as_duration().as_secs(),
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
        if !self.config.mke2fs.is_file() {
            return Err(SandboxError::ProgramNotFound {
                program: self.config.mke2fs.clone(),
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
                detail: "no guest image was built for this runtime root".to_owned(),
            });
        }
        if !spec.egress.is_none() {
            // Refuse rather than run a task with an egress profile it cannot
            // get: the guest half of the bridge lands with dependency
            // resolution. A task that silently ran without its egress would
            // fail in ways that look like a network fault.
            return Err(SandboxError::Unsupported {
                backend: "firecracker",
                requirement: format!(
                    "egress profile {} needs the vsock egress bridge, which is not built yet",
                    spec.egress
                ),
            });
        }
        Ok(())
    }

    fn start<'a>(&'a self, spec: SandboxSpec) -> BoxFuture<'a, Result<SandboxHandle>> {
        Box::pin(async move {
            self.preflight(&spec)?;
            std::fs::create_dir_all(&self.config.runtime_dir)
                .map_err(|error| SandboxError::io("creating the firecracker runtime dir", error))?;

            let prepared = self.prepare(&spec)?;

            // The channel is bound before the VM starts: a guest that connects
            // to a port nobody is listening on gets a refusal, and the order
            // here is what makes that a host-side guarantee rather than a race.
            let channel = GuestChannel::bind(prepared.channel.clone())?;

            let config_path = self.config.runtime_dir.join(format!("{}.json", spec.id));
            let encoded = serde_json::to_vec_pretty(&prepared.vm_config).map_err(|error| {
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
            // `--no-api` and a config file: this VM's shape is fixed before it
            // boots, and an API socket would be a second way to change it.
            command
                .arg("--no-api")
                .arg("--config-file")
                .arg(&config_path);
            command.env_clear();
            command.kill_on_drop(true);
            command.stdin(Stdio::null());
            // The console carries early boot diagnostics only; task output
            // arrives over vsock (R11). It goes to a file rather than a pipe
            // because nothing here reads a pipe, and an 8250 console whose
            // reader never drains it stalls the guest — the exact failure the
            // console is supposed to help diagnose.
            let console_path = self
                .config
                .runtime_dir
                .join(format!("{}.console.log", spec.id));
            let console = std::fs::File::create(&console_path)
                .map_err(|error| SandboxError::io("creating the guest console log", error))?;
            let console_err = console
                .try_clone()
                .map_err(|error| SandboxError::io("duplicating the guest console log", error))?;
            command.stdout(Stdio::from(console));
            command.stderr(Stdio::from(console_err));

            let started_at = Utc::now();
            let deadline = started_at
                + chrono::Duration::from_std(spec.limits.max_wall_clock.as_duration())
                    .unwrap_or_else(|_| chrono::Duration::hours(1));

            let child = command.spawn().map_err(|error| SandboxError::Spawn {
                id: spec.id.clone(),
                source: error,
            })?;

            let (outcome, tasks) = channel.serve(
                prepared.job,
                LogTargets {
                    stdout: spec.stdout_path.clone(),
                    stderr: spec.stderr_path.clone(),
                },
            );
            self.runs.lock().await.insert(
                spec.id.clone(),
                ServedRun {
                    outcome,
                    tasks,
                    paths: prepared.channel,
                },
            );

            tracing::info!(
                sandbox = spec.id,
                vcpus = prepared.vm_config.vcpu_count(),
                mem_mib = prepared.vm_config.mem_size_mib(),
                drives = prepared.vm_config.drive_count(),
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

    /// Waits for the VM, and reports the *guest's* account of the task.
    ///
    /// The distinction this function exists to preserve: a task that ran and
    /// failed produces an exit status, and a VM that died produces an error. A
    /// backend that reported the VM's own exit code as the task's would turn
    /// every sandbox failure into a project failure.
    fn wait<'a>(&'a self, handle: &'a SandboxHandle) -> BoxFuture<'a, Result<ExitStatus>> {
        Box::pin(async move {
            let vm = wait_with_deadline(&handle.id, handle.deadline, handle.child()).await;
            let run = self.runs.lock().await.remove(&handle.id);

            let Some(run) = run else {
                // No served run means `start` never registered one, which is a
                // bug rather than a guest outcome.
                return Err(SandboxError::GuestChannel {
                    detail: format!("sandbox {} has no guest channel", handle.id),
                });
            };
            for task in &run.tasks {
                task.abort();
            }
            GuestChannel::cleanup(&run.paths);

            match run.outcome.await {
                Ok(GuestOutcome::Reported(Report::Exited { exit })) => Ok(ExitStatus {
                    code: exit.code,
                    timed_out: false,
                    signal: exit.signal,
                }),
                Ok(GuestOutcome::Reported(Report::Failed { stage, detail })) => {
                    Err(SandboxError::GuestChannel {
                        detail: format!("the guest failed at {}: {detail}", stage.as_str()),
                    })
                }
                Ok(GuestOutcome::Silent { detail }) => match vm {
                    // A VM the deadline killed is a wall-clock failure, which
                    // the task layer already knows how to render.
                    Ok(status) if status.timed_out => Ok(status),
                    Ok(_) | Err(_) => Err(SandboxError::GuestChannel {
                        detail: format!("the VM stopped without reporting: {detail}"),
                    }),
                },
                Err(_) => Err(SandboxError::GuestChannel {
                    detail: "the guest channel was dropped before the run finished".to_owned(),
                }),
            }
        })
    }

    fn terminate<'a>(&'a self, handle: &'a SandboxHandle) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let outcome = terminate_child(handle.child()).await;
            if let Some(run) = self.runs.lock().await.remove(&handle.id) {
                for task in &run.tasks {
                    task.abort();
                }
                GuestChannel::cleanup(&run.paths);
            }
            outcome
        })
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

    use crate::spec::{Mount, MountMode, ScratchPolicy};

    fn backend() -> FirecrackerBackend {
        FirecrackerBackend::new(FirecrackerConfig {
            firecracker: PathBuf::from("/usr/bin/firecracker"),
            mke2fs: PathBuf::from("/usr/sbin/mke2fs"),
            kernel: PathBuf::from("/var/lib/clyde/vm/vmlinux"),
            rootfs_dir: PathBuf::from("/var/lib/clyde/vm/rootfs"),
            runtime_dir: PathBuf::from("/run/clyde/vm"),
            vsock_dir: PathBuf::from("/run/clyde/vsock"),
        })
    }

    /// The images a `rust.check` run produces, without building any.
    fn images() -> Vec<BuiltImage> {
        vec![
            BuiltImage {
                path: PathBuf::from("/run/clyde/vm/sb-vm.work.img"),
                label: "clyde-work".to_owned(),
                target: PathBuf::from("/work"),
                writable: false,
                purpose: MountPurpose::Snapshot,
            },
            BuiltImage {
                path: PathBuf::from("/var/lib/clyde/missions/m1/vm-cache.img"),
                label: "clyde-cache".to_owned(),
                target: PathBuf::from(GUEST_CACHE),
                writable: true,
                purpose: MountPurpose::MissionCache,
            },
            BuiltImage {
                path: PathBuf::from("/run/clyde/vm/sb-vm.deps.img"),
                label: "clyde-deps".to_owned(),
                target: PathBuf::from("/deps"),
                writable: false,
                purpose: MountPurpose::DependencyBundle,
            },
        ]
    }

    fn spec(egress: EgressProfile) -> SandboxSpec {
        let mounts = vec![
            Mount {
                source: Some(PathBuf::from("/var/lib/clyde/snapshots/s1")),
                target: PathBuf::from("/work"),
                mode: MountMode::ReadOnly,
                purpose: MountPurpose::Snapshot,
            },
            Mount {
                source: Some(PathBuf::from("/var/lib/clyde/missions/m1/cargo-target")),
                target: PathBuf::from("/cache/target"),
                mode: MountMode::ReadWrite,
                purpose: MountPurpose::MissionCache,
            },
        ];
        SandboxSpec {
            id: "sb-vm".to_owned(),
            runtime_root_kind: RuntimeRootKind::Rust,
            runtime_root: PathBuf::from("/nix/store/rust-root"),
            runtime_root_closure: vec![PathBuf::from("/nix/store/rust-root")],
            mounts,
            egress,
            limits: ResourceLimits {
                max_wall_clock: HumanDuration::parse("20m").unwrap(),
                max_memory_bytes: 4 << 30,
                max_cpu_percent: 200,
                max_tasks: 256,
                max_open_files: 4096,
            },
            trust_class: TrustClass::T2,
            min_isolation: IsolationLevel::MicroVm,
            env: BTreeMap::from([
                ("CARGO_TARGET_DIR".to_owned(), "/cache/target".to_owned()),
                ("CARGO_HOME".to_owned(), "/cache/cargo-home".to_owned()),
            ]),
            argv: vec![
                "/nix/store/rust-root/bin/cargo".to_owned(),
                "check".to_owned(),
            ],
            cwd: PathBuf::from("/work"),
            scratch: ScratchPolicy::default(),
            stdout_path: None,
            stderr_path: None,
        }
    }

    fn plan(egress: EgressProfile) -> PreparedRun {
        backend().plan(&spec(egress), images()).unwrap()
    }

    #[test]
    fn the_guest_has_no_network_device_in_any_configuration() {
        for egress in [EgressProfile::None, EgressProfile::RustRegistry] {
            let json = serde_json::to_string(&plan(egress).vm_config).unwrap();
            assert!(
                !json.contains("network-interfaces") && !json.contains("iface_id"),
                "the guest must have no network device at all: {json}"
            );
        }
    }

    #[test]
    fn every_vm_has_a_vsock_device_whatever_the_egress_profile() {
        // Supersedes the pre-D24 rule that a `none` profile guest had no
        // channel at all: logs have nowhere else to go, so the device is
        // unconditional and what varies is the host listener (D7 amendment).
        for egress in [EgressProfile::None, EgressProfile::RustRegistry] {
            let config = plan(egress).vm_config;
            assert_eq!(config.vsock.guest_cid, clyde_guest_api::GUEST_CID);
            assert!(config.vsock.uds_path.ends_with("sb-vm.vsock"));
        }
    }

    #[test]
    fn only_the_mission_cache_is_writable() {
        let config = plan(EgressProfile::None).vm_config;
        let writable = config.writable_drives();
        assert_eq!(writable.len(), 1, "writable drives: {writable:?}");
        assert!(writable[0].contains("cache"));
    }

    #[test]
    fn the_snapshot_and_dependency_drives_are_read_only() {
        let config = plan(EgressProfile::None).vm_config;
        for label in ["clyde-work", "clyde-deps"] {
            let drive = config
                .drives
                .iter()
                .find(|drive| drive.drive_id.contains(label))
                .expect("the drive exists");
            assert!(drive.is_read_only, "{label} must be attached read-only");
        }
    }

    #[test]
    fn the_rootfs_is_the_root_device_and_read_only() {
        let config = plan(EgressProfile::None).vm_config;
        let root = config
            .drives
            .iter()
            .find(|drive| drive.is_root_device)
            .expect("a root device");
        assert!(root.is_read_only);
        assert!(root.path_on_host.ends_with("rust.img"));
    }

    #[test]
    fn the_boot_arguments_name_the_erofs_root_and_the_init() {
        let config = plan(EgressProfile::None).vm_config;
        let args = config.boot_args();
        assert!(args.contains("root=/dev/vda"), "{args}");
        assert!(args.contains("rootfstype=erofs"), "{args}");
        assert!(args.contains("init=/init"), "{args}");
        // The guest stops the VM with a reset, which is what Firecracker's
        // i8042 answers to.
        assert!(args.contains("reboot=k"), "{args}");
    }

    #[test]
    fn the_mission_cache_is_the_only_drive_with_an_unsafe_cache_type() {
        let config = plan(EgressProfile::None).vm_config;
        let unsafe_drives: Vec<&str> = config
            .drives
            .iter()
            .filter(|drive| drive.cache_type == Some("Unsafe"))
            .map(|drive| drive.drive_id.as_str())
            .collect();
        assert_eq!(unsafe_drives.len(), 1);
        assert!(unsafe_drives[0].contains("cache"));
    }

    #[test]
    fn the_job_carries_the_argv_environment_and_working_directory() {
        let job = plan(EgressProfile::None).job;
        assert_eq!(job.version, PROTOCOL_VERSION);
        assert_eq!(job.argv, spec(EgressProfile::None).argv);
        assert_eq!(job.cwd, PathBuf::from("/work"));
        assert_eq!(
            job.env.get("CARGO_TARGET_DIR").map(String::as_str),
            Some("/cache/target")
        );
    }

    #[test]
    fn the_job_addresses_drives_by_label_and_never_by_device_name() {
        let job = plan(EgressProfile::None).job;
        let labels: Vec<&str> = job
            .mounts
            .iter()
            .map(|mount| mount.label.as_str())
            .collect();
        assert_eq!(labels, vec!["clyde-work", "clyde-cache", "clyde-deps"]);
        assert!(
            !job.mounts
                .iter()
                .any(|mount| mount.target.to_string_lossy().contains("/dev/vd")),
            "device names are positional and must never appear in the contract"
        );
    }

    #[test]
    fn the_task_does_not_run_as_guest_root() {
        let job = plan(EgressProfile::None).job;
        assert_ne!(job.user.uid, 0, "the task drops out of guest root (D24)");
    }

    #[test]
    fn the_job_names_no_egress_bridge_under_a_none_profile() {
        assert!(plan(EgressProfile::None).job.egress.is_none());
        assert!(plan(EgressProfile::RustRegistry).job.egress.is_some());
    }

    #[test]
    fn limits_become_vm_sizing() {
        let config = plan(EgressProfile::None).vm_config;
        assert_eq!(config.mem_size_mib(), 4096);
        assert_eq!(config.vcpu_count(), 2, "200% CPU quota is two vCPUs");
    }

    #[test]
    fn tiny_limits_still_produce_a_bootable_machine() {
        let mut spec = spec(EgressProfile::None);
        spec.limits.max_memory_bytes = 1 << 20;
        spec.limits.max_cpu_percent = 10;
        let config = backend().plan(&spec, images()).unwrap().vm_config;
        assert!(config.mem_size_mib() >= 128);
        assert_eq!(config.vcpu_count(), 1);
    }

    #[test]
    fn a_mount_table_over_the_drive_budget_is_refused_rather_than_booted() {
        let mut many = images();
        for index in 0..MAX_DRIVES {
            many.push(BuiltImage {
                path: PathBuf::from(format!("/run/clyde/vm/extra{index}.img")),
                label: format!("clyde-x{index}"),
                target: PathBuf::from(format!("/extra{index}")),
                writable: false,
                purpose: MountPurpose::DependencyBundle,
            });
        }
        let error = backend()
            .plan(&spec(EgressProfile::None), many)
            .expect_err("over the budget");
        assert!(matches!(error, SandboxError::Unsupported { .. }));
    }

    #[test]
    fn preflight_refuses_without_kvm_or_images() {
        // This host has neither, which is the case the refusal exists for.
        let error = backend()
            .preflight(&spec(EgressProfile::None))
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

    #[test]
    fn the_cache_image_lives_inside_the_mission_cache_directory() {
        // So that mission closeout deleting the cache directory deletes the
        // image with it (D3), rather than leaving the largest file behind.
        let image = &images()[1];
        assert!(
            image.path.starts_with("/var/lib/clyde/missions/m1"),
            "{:?} must be inside the mission cache directory",
            image.path
        );
        assert!(image.path.ends_with(CACHE_IMAGE_NAME));
    }
}
