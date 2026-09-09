//! Host capability probing.
//!
//! `clyde doctor` is what makes bring-up diagnosable rather than mysterious, and
//! it is only as good as these probes. Two distinctions matter more than the
//! rest, because the remedies are entirely different and the raw kernel errors
//! do not say which case you are in:
//!
//! - "user namespaces unavailable" versus "blocked by AppArmor"
//! - "no cgroup v2" versus "cgroup v2 present but not delegated" versus
//!   "delegated but missing controllers"

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clyde_core::posture::PostureObservation;

use clyde_core::classification::IsolationLevel;
use clyde_core::task::RuntimeRootKind;

use crate::runtime_root::RuntimeRoot;

/// The state of one host prerequisite.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Capability {
    /// Present and usable.
    Available { detail: String },
    /// Present but unusable as configured, with the remedy.
    Blocked { detail: String, remedy: String },
    /// Not present at all.
    Unavailable { detail: String, remedy: String },
}

impl Capability {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available { .. })
    }

    pub fn detail(&self) -> &str {
        match self {
            Self::Available { detail }
            | Self::Blocked { detail, .. }
            | Self::Unavailable { detail, .. } => detail,
        }
    }

    pub fn remedy(&self) -> Option<&str> {
        match self {
            Self::Available { .. } => None,
            Self::Blocked { remedy, .. } | Self::Unavailable { remedy, .. } => Some(remedy),
        }
    }
}

/// Everything `clyde doctor` reports.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HostReport {
    pub user_namespaces: Capability,
    pub cgroup_v2: Capability,
    pub cgroup_delegation: Capability,
    /// Whether the `systemd-run --user` wrapper can reach the per-user manager.
    ///
    /// Separate from `cgroup_delegation`, which asks whether a delegated subtree
    /// is writable. Both must hold: a writable subtree with no reachable bus
    /// still means every build task dies before `bwrap` starts.
    pub session_bus: Capability,
    /// Whether each configured runtime root's programs resolve once bound.
    pub runtime_roots: Capability,
    pub kvm: Capability,
    pub bubblewrap: Capability,
    pub firecracker: Capability,
    /// `mke2fs`, which the microVM backend needs to build every guest image.
    pub mke2fs: Capability,
    /// Whether the guest kernel and root images the flake builds are in place.
    ///
    /// Reported separately from `firecracker` because they fail differently: a
    /// missing binary is an install step, and a missing image is a `nix build`
    /// plus a symlink.
    pub guest_images: Capability,
    pub nix: Capability,
    pub hardlinks: Capability,
    /// Known limitations, stated plainly so they are discovered before a
    /// confusing build failure.
    pub known_limitations: Vec<String>,
}

impl HostReport {
    /// Whether the host can run the workspace environment (T0/T1).
    ///
    /// The workspace environment may run on rlimits and a timeout, since it
    /// hosts a semi-trusted agent rather than hostile dependency code (D22).
    pub fn can_run_workspace(&self) -> bool {
        self.user_namespaces.is_available() && self.bubblewrap.is_available()
    }

    /// Whether the host can run build tasks (T2 and above).
    ///
    /// Cgroup v2 delegation is mandatory; without it build tasks are refused,
    /// with no opt-in and no fallback (D22).
    ///
    /// The session bus counts: the wrapper chain starts with `systemd-run --user`,
    /// so a host with a delegated subtree it cannot reach still refuses every
    /// build task, and reporting `ok` here would be a lie the first run exposes.
    pub fn can_run_build(&self) -> bool {
        self.can_run_workspace()
            && self.cgroup_delegation.is_available()
            && self.session_bus.is_available()
    }

    /// The unsatisfied workspace prerequisites, each named.
    pub fn workspace_blockers(&self) -> Vec<(&'static str, &Capability)> {
        [
            ("user namespaces", &self.user_namespaces),
            ("bubblewrap", &self.bubblewrap),
        ]
        .into_iter()
        .filter(|(_, capability)| !capability.is_available())
        .collect()
    }

    /// The unsatisfied build prerequisites, each named.
    ///
    /// Build inherits every workspace prerequisite, so a blocked user namespace
    /// refuses build tasks on its own. Naming the failing one matters: a report
    /// that always blames delegation sends the operator to the D22 remedy for a
    /// host whose delegation is fine.
    pub fn build_blockers(&self) -> Vec<(&'static str, &Capability)> {
        let mut blockers = self.workspace_blockers();
        if !self.cgroup_delegation.is_available() {
            blockers.push(("cgroup delegation", &self.cgroup_delegation));
        }
        if !self.session_bus.is_available() {
            blockers.push(("session bus", &self.session_bus));
        }
        blockers
    }

    /// Whether the host can run microVM tasks (Phase 2b).
    pub fn can_run_microvm(&self) -> bool {
        self.kvm.is_available() && self.firecracker.is_available()
    }

    /// The strongest isolation level available, for admission checks.
    pub fn strongest_isolation(&self) -> Option<IsolationLevel> {
        if self.can_run_microvm() {
            Some(IsolationLevel::MicroVm)
        } else if self.can_run_workspace() {
            Some(IsolationLevel::NamespaceSandbox)
        } else {
            None
        }
    }

    /// The capability view the policy layer consumes.
    pub fn policy_view(&self) -> clyde_policy::HostCapabilities {
        clyde_policy::HostCapabilities {
            strongest_isolation: self.strongest_isolation(),
            cgroup_delegation: self.cgroup_delegation.is_available(),
        }
    }
}

/// Where to look for the host programs Clyde executes.
#[derive(Debug, Clone, Default)]
pub struct ProbePaths {
    pub bwrap: Option<PathBuf>,
    pub firecracker: Option<PathBuf>,
    pub mke2fs: Option<PathBuf>,
    pub nix: Option<PathBuf>,
    /// The guest image directory, normally `<state-dir>/vm`.
    pub guest_vm_dir: Option<PathBuf>,
    /// Directory used for the hardlink probe; the daemon's state directory.
    pub state_dir: Option<PathBuf>,
    /// The configured runtime roots, so the probe can check that what a task
    /// would exec actually resolves inside the closure the sandbox binds.
    pub runtime_roots: BTreeMap<RuntimeRootKind, PathBuf>,
    /// The `runtimeRootManifests` directory, which is what supplies the closure.
    pub runtime_root_manifests: Option<PathBuf>,
}

/// Probes every host prerequisite.
///
/// Pure inspection: nothing is created that outlives the call, and no probe
/// requires privilege.
pub fn probe(paths: &ProbePaths) -> HostReport {
    let enclosure = detect_enclosure();
    // Resolved once: the userns verdict is about this exact path, because
    // AppArmor attaches policy by executable path.
    let bwrap = resolve_program(paths.bwrap.as_deref(), "bwrap");
    HostReport {
        user_namespaces: probe_user_namespaces(&enclosure, bwrap.as_deref()),
        cgroup_v2: probe_cgroup_v2(),
        cgroup_delegation: classify_cgroup_delegation(&observe_cgroups(enclosure.clone())),
        session_bus: probe_session_bus(&enclosure),
        runtime_roots: probe_runtime_roots(
            &paths.runtime_roots,
            paths.runtime_root_manifests.as_deref(),
        ),
        kvm: classify_kvm(&observe_kvm(enclosure)),
        bubblewrap: classify_resolved(
            "bubblewrap",
            bwrap.as_deref(),
            "add `bubblewrap` to the flake devShell, or set sandbox.bwrap in host configuration",
        ),
        firecracker: probe_program(
            "firecracker",
            paths.firecracker.as_deref(),
            "firecracker",
            "install firecracker and set sandbox.firecracker in host configuration; without it, microVM tasks are refused",
        ),
        mke2fs: probe_program(
            "mke2fs",
            paths.mke2fs.as_deref(),
            "mke2fs",
            "add `e2fsprogs` to the flake devShell, or set sandbox.mke2fs in host configuration; the microVM backend builds every guest image with it",
        ),
        guest_images: probe_guest_images(paths.guest_vm_dir.as_deref()),
        nix: probe_program(
            "nix",
            paths.nix.as_deref(),
            "nix",
            "install nix with flakes enabled; runtime roots are nix closures (D6)",
        ),
        hardlinks: probe_hardlinks(paths.state_dir.as_deref()),
        known_limitations: known_limitations(),
    }
}

/// What encloses this process, as far as the probes can tell.
///
/// This never changes a verdict, only the remedy — which is the whole point.
/// The same "no delegated cgroup" observation means "configure your login
/// session" on a host and "the container was not given this" inside a
/// container, and only one of those two remedies can possibly work. A doctor
/// that names the wrong one sends someone to edit a systemd unit that is not
/// there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Enclosure {
    /// A container. Remedies lie at the container boundary, not inside it.
    Container { evidence: String },
    /// No service manager, but no positive container evidence either — a
    /// non-systemd distribution, or a bare chroot.
    NoServiceManager { evidence: String },
    /// An ordinary host running a service manager.
    Host,
}

impl Enclosure {
    /// Whether remedies have to be applied from outside this environment.
    pub fn is_container(&self) -> bool {
        matches!(self, Self::Container { .. })
    }

    /// The evidence, for inclusion in a diagnostic.
    pub fn evidence(&self) -> Option<&str> {
        match self {
            Self::Container { evidence } | Self::NoServiceManager { evidence } => {
                Some(evidence.as_str())
            }
            Self::Host => None,
        }
    }
}

fn detect_enclosure() -> Enclosure {
    if Path::new("/.dockerenv").exists() {
        return Enclosure::Container {
            evidence: "/.dockerenv is present".to_owned(),
        };
    }
    if let Ok(text) = std::fs::read_to_string("/proc/1/cgroup") {
        for marker in ["/docker/", "/lxc/", "kubepods", "/podman"] {
            if text.contains(marker) {
                return Enclosure::Container {
                    evidence: format!("PID 1's cgroup path contains `{marker}`"),
                };
            }
        }
    }
    let init = std::fs::read_to_string("/proc/1/comm")
        .ok()
        .map(|text| text.trim().to_owned());
    if Path::new("/run/systemd/system").exists() {
        return Enclosure::Host;
    }
    match init {
        // A container init: not proof of a container by itself, but nothing
        // else runs these as PID 1.
        Some(name) if matches!(name.as_str(), "tini" | "docker-init" | "dumb-init") => {
            Enclosure::Container {
                evidence: format!("PID 1 is `{name}` and /run/systemd/system is absent"),
            }
        }
        Some(name) if name != "systemd" => Enclosure::NoServiceManager {
            evidence: format!("PID 1 is `{name}` and /run/systemd/system is absent"),
        },
        _ => Enclosure::Host,
    }
}

/// Limitations worth stating before someone hits a confusing failure.
pub fn known_limitations() -> Vec<String> {
    vec![
        "git metadata is unavailable to build tasks, so vergen-style crates and build scripts shelling out to git will fail (D21)".to_owned(),
        "build and test tasks require cgroup v2 delegation and are refused without it (D22)".to_owned(),
        "dependency resolution requires the microVM backend, so it is refused on a host without KVM (D9)".to_owned(),
        "private registries and private git dependencies are not supported and are refused rather than half-supported".to_owned(),
    ]
}

/// The Ubuntu 24.04 case: `bwrap` exists and user namespaces are enabled in the
/// kernel, but AppArmor blocks unprivileged userns for binaries without a
/// permitting profile — which includes anything in the nix store.
///
/// The sysctl alone cannot answer this. A profile permitting one binary leaves
/// the sysctl at `1`, because the restriction stays on host-wide and the
/// profile is an exception to it, so inferring a verdict from the sysctl
/// reports the recommended remedy as still-unapplied forever. When a `bwrap` is
/// known, the question is settled by running it.
fn probe_user_namespaces(enclosure: &Enclosure, bwrap: Option<&Path>) -> Capability {
    let max = read_sysctl("/proc/sys/user/max_user_namespaces")
        .and_then(|text| text.trim().parse::<u64>().ok());
    if max == Some(0) {
        return Capability::Unavailable {
            detail: "user.max_user_namespaces is 0".to_owned(),
            remedy: "sysctl -w user.max_user_namespaces=15000".to_owned(),
        };
    }
    let apparmor_restricted = read_sysctl("/proc/sys/kernel/apparmor_restrict_unprivileged_userns")
        .map(|text| text.trim() == "1")
        .unwrap_or(false);
    if apparmor_restricted {
        // Only this binary's verdict matters: it is the one Clyde will execute,
        // and AppArmor attaches policy by executable path.
        if let Some(path) = bwrap {
            match bwrap_can_unshare(path) {
                Some(true) => {
                    return Capability::Available {
                        detail: format!(
                            "{} can create a user namespace under kernel.apparmor_restrict_unprivileged_userns=1, so a profile permits it",
                            path.display()
                        ),
                    };
                }
                Some(false) => {
                    return Capability::Blocked {
                        detail: format!(
                            "kernel.apparmor_restrict_unprivileged_userns=1 and {} could not create a user namespace, so no AppArmor profile permits it",
                            path.display()
                        ),
                        remedy: apparmor_remedy(enclosure),
                    };
                }
                // Could not be run at all: fall through to the sysctl reading,
                // which is a weaker claim and says so.
                None => {}
            }
        }
        return Capability::Blocked {
            detail: "kernel.apparmor_restrict_unprivileged_userns=1 blocks unprivileged user namespaces for binaries without a permitting AppArmor profile, which includes a nix-store bwrap; no bwrap could be run to confirm whether one permits it here".to_owned(),
            remedy: apparmor_remedy(enclosure),
        };
    }
    match max {
        Some(max) => Capability::Available {
            detail: format!("unprivileged user namespaces permitted (max {max})"),
        },
        None => Capability::Unavailable {
            detail: "/proc/sys/user/max_user_namespaces is not readable, so user namespace support cannot be confirmed".to_owned(),
            remedy: "run on a Linux host with user namespace support; Clyde is Linux-first".to_owned(),
        },
    }
}

fn apparmor_remedy(enclosure: &Enclosure) -> String {
    // The sysctl and the policy both belong to the host kernel, so inside a
    // container neither the profile nor the sysctl can be changed from here —
    // worth saying, because both remedies look actionable.
    let mut remedy = "install an AppArmor profile granting `userns create` for the nix-store bwrap path (narrowest, survives reboot); or `sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`, which re-enables unprivileged userns for everything on the host".to_owned();
    if enclosure.is_container() {
        remedy.push_str(
            ". Both apply to the host kernel, not to this container, so neither can be done from in here",
        );
    }
    remedy
}

/// Runs the real thing: the smallest namespace `bwrap` can be asked for.
///
/// `None` when the answer is not the host's — the binary could not be spawned,
/// or there is no payload to hand it — so the caller does not read a broken
/// probe as a denial.
fn bwrap_can_unshare(bwrap: &Path) -> Option<bool> {
    let payload = resolve_program(None, "true").or_else(|| {
        let fallback = PathBuf::from("/bin/true");
        fallback.is_file().then_some(fallback)
    })?;
    let status = std::process::Command::new(bwrap)
        .args([
            "--unshare-user",
            "--uid",
            "0",
            "--gid",
            "0",
            "--ro-bind",
            "/",
            "/",
        ])
        .arg(&payload)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    Some(status.success())
}

fn probe_cgroup_v2() -> Capability {
    let controllers = Path::new("/sys/fs/cgroup/cgroup.controllers");
    if !controllers.exists() {
        return Capability::Unavailable {
            detail: "/sys/fs/cgroup/cgroup.controllers is absent, so this host is not running the cgroup v2 unified hierarchy".to_owned(),
            remedy: "boot with systemd.unified_cgroup_hierarchy=1, or use a host with cgroup v2".to_owned(),
        };
    }
    match std::fs::read_to_string(controllers) {
        Ok(text) => {
            let available: Vec<&str> = text.split_whitespace().collect();
            let required = ["memory", "pids", "cpu"];
            let missing: Vec<&str> = required
                .into_iter()
                .filter(|controller| !available.contains(controller))
                .collect();
            if missing.is_empty() {
                Capability::Available {
                    detail: format!("cgroup v2 with controllers: {}", available.join(", ")),
                }
            } else {
                Capability::Blocked {
                    detail: format!(
                        "cgroup v2 is present but missing controllers: {}",
                        missing.join(", ")
                    ),
                    remedy: "enable the missing controllers in the parent cgroup's subtree_control"
                        .to_owned(),
                }
            }
        }
        Err(error) => Capability::Unavailable {
            detail: format!("cgroup v2 controllers could not be read: {error}"),
            remedy: "check that /sys/fs/cgroup is mounted".to_owned(),
        },
    }
}

/// Delegation is what lets an unprivileged daemon create a scope with limits.
///
/// Reported as a hard failure for build capability rather than a warning, since
/// build tasks are refused without it (D22).
///
/// Split into an observation and a classification so the cases that cannot be
/// reproduced on the machine running the tests — a read-only cgroupfs, a
/// container with no service manager — are still covered by them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CgroupObservation {
    /// Whether a cgroup v2 unified hierarchy exists at all.
    pub hierarchy_present: bool,
    /// This process's own cgroup path, from `/proc/self/cgroup`.
    pub own_path: Option<String>,
    /// Whether `/sys/fs/cgroup` is mounted read-only.
    pub read_only_mount: bool,
    /// Whether this process can write its own cgroup's `subtree_control`.
    pub subtree_control_writable: bool,
    pub enclosure: Enclosure,
}

fn observe_cgroups(enclosure: Enclosure) -> CgroupObservation {
    let own_path = std::fs::read_to_string("/proc/self/cgroup")
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("0::").map(str::to_owned))
        });
    let subtree_control_writable = own_path
        .as_deref()
        .map(|path| {
            let subtree = Path::new("/sys/fs/cgroup")
                .join(path.trim_start_matches('/'))
                .join("cgroup.subtree_control");
            std::fs::OpenOptions::new()
                .append(true)
                .open(subtree)
                .is_ok()
        })
        .unwrap_or(false);
    CgroupObservation {
        hierarchy_present: Path::new("/sys/fs/cgroup/cgroup.controllers").exists(),
        own_path,
        read_only_mount: cgroupfs_is_read_only(),
        subtree_control_writable,
        enclosure,
    }
}

/// Whether `/sys/fs/cgroup` carries the `ro` mount option.
///
/// A read-only cgroupfs is a different failure from an unwritable
/// `subtree_control`: no cgroup can be created anywhere in the hierarchy, so
/// no amount of session configuration helps.
fn cgroupfs_is_read_only() -> bool {
    let Ok(mounts) = std::fs::read_to_string("/proc/self/mounts") else {
        return false;
    };
    mounts.lines().any(|line| {
        let mut fields = line.split_whitespace();
        let (Some(_source), Some(target), Some(kind), Some(options)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return false;
        };
        target == "/sys/fs/cgroup"
            && kind == "cgroup2"
            && options.split(',').any(|option| option == "ro")
    })
}

pub fn classify_cgroup_delegation(observed: &CgroupObservation) -> Capability {
    if !observed.hierarchy_present {
        return Capability::Unavailable {
            detail: "no cgroup v2 hierarchy, so delegation cannot exist".to_owned(),
            remedy: "use a host with cgroup v2; build tasks are refused without it".to_owned(),
        };
    }
    let Some(path) = observed.own_path.as_deref() else {
        return Capability::Unavailable {
            detail: "this process is not in a cgroup v2 cgroup".to_owned(),
            remedy: "run under a systemd user session so a delegated slice exists".to_owned(),
        };
    };
    let delegated = Path::new("/sys/fs/cgroup").join(path.trim_start_matches('/'));
    if observed.subtree_control_writable {
        return Capability::Available {
            detail: format!("cgroup v2 delegated at {}", delegated.display()),
        };
    }
    // A read-only cgroupfs is the stronger and more specific finding, so it is
    // reported ahead of the delegation question: the session remedy cannot work
    // when nothing in the hierarchy can be written at all.
    if observed.read_only_mount {
        let remedy = match &observed.enclosure {
            Enclosure::Container { .. } => {
                "this is the container boundary, and nothing inside the container can lift it: run clyded on the host, or give the container a writable cgroup namespace (`--cgroupns=private` with a read-write /sys/fs/cgroup, or an init that delegates). Build tasks are refused without delegation (D22)"
            }
            _ => {
                "remount /sys/fs/cgroup read-write, then run under a service manager that delegates a subtree. Build tasks are refused without delegation (D22)"
            }
        };
        return Capability::Blocked {
            detail: format!(
                "/sys/fs/cgroup is mounted read-only, so no cgroup can be created anywhere in the hierarchy{}",
                observed
                    .enclosure
                    .evidence()
                    .map(|evidence| format!(" ({evidence})"))
                    .unwrap_or_default()
            ),
            remedy: remedy.to_owned(),
        };
    }
    let remedy = match &observed.enclosure {
        Enclosure::Container { .. } => {
            "the container was not given a delegated cgroup subtree: run clyded on the host, or start the container with an init that delegates one. Build tasks are refused without delegation (D22)"
        }
        Enclosure::NoServiceManager { .. } => {
            "no service manager is running to delegate a subtree; start one, or run clyded where systemd manages the session. Build tasks are refused without delegation (D22)"
        }
        Enclosure::Host => {
            "run under a systemd user session with Delegate=yes (`systemctl show user@$(id -u).service -p Delegate`); a non-login session needs `loginctl enable-linger $USER`. Build tasks are refused without delegation (D22)"
        }
    };
    Capability::Blocked {
        detail: format!(
            "cgroup v2 is present but {}/cgroup.subtree_control is not writable, so no delegated scope can be created",
            delegated.display()
        ),
        remedy: remedy.to_owned(),
    }
}

/// What the CPU reports about hardware virtualisation.
///
/// The distinction that matters: `/dev/kvm` missing on a CPU that advertises
/// `vmx` or `svm` means the device was not exposed to this environment, not
/// that the hardware cannot do it. Those have opposite remedies, and telling
/// someone to "enable hardware virtualisation" that is already enabled is the
/// kind of dead end `clyde doctor` exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuVirtualisation {
    /// Intel VT-x.
    Vmx,
    /// AMD-V.
    Svm,
    /// A flags line was found and carried neither.
    Absent,
    /// No flags line to read; a non-x86 architecture, or no `/proc/cpuinfo`.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KvmObservation {
    pub device_present: bool,
    /// The error from opening the device, if it exists but would not open.
    pub open_error: Option<String>,
    pub cpu: CpuVirtualisation,
    pub enclosure: Enclosure,
}

fn observe_kvm(enclosure: Enclosure) -> KvmObservation {
    let kvm = Path::new("/dev/kvm");
    let device_present = kvm.exists();
    let open_error = device_present
        .then(|| {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(kvm)
                .err()
                .map(|error| error.to_string())
        })
        .flatten();
    KvmObservation {
        device_present,
        open_error,
        cpu: read_cpu_virtualisation(),
        enclosure,
    }
}

fn read_cpu_virtualisation() -> CpuVirtualisation {
    let Ok(text) = std::fs::read_to_string("/proc/cpuinfo") else {
        return CpuVirtualisation::Unknown;
    };
    let mut saw_flags = false;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        // `flags` on x86, `Features` on some arm64 kernels.
        if !matches!(key.trim(), "flags" | "Features") {
            continue;
        }
        saw_flags = true;
        for flag in value.split_whitespace() {
            match flag {
                "vmx" => return CpuVirtualisation::Vmx,
                "svm" => return CpuVirtualisation::Svm,
                _ => {}
            }
        }
    }
    if saw_flags {
        CpuVirtualisation::Absent
    } else {
        CpuVirtualisation::Unknown
    }
}

pub fn classify_kvm(observed: &KvmObservation) -> Capability {
    if !observed.device_present {
        return match observed.cpu {
            CpuVirtualisation::Vmx | CpuVirtualisation::Svm => {
                let extension = if observed.cpu == CpuVirtualisation::Vmx {
                    "vmx"
                } else {
                    "svm"
                };
                let remedy = match &observed.enclosure {
                    Enclosure::Container { .. } => "expose the device to this environment (`--device=/dev/kvm`), or run clyded on the host",
                    _ => "load the kvm module (`modprobe kvm_intel` or `modprobe kvm_amd`) and add your user to the `kvm` group; if this is itself a virtual machine, enable nested virtualisation on its hypervisor",
                };
                Capability::Unavailable {
                    detail: format!(
                        "/dev/kvm is absent, but this CPU reports `{extension}`, so hardware virtualisation is supported and the device is simply not present here{}",
                        observed
                            .enclosure
                            .evidence()
                            .map(|evidence| format!(" ({evidence})"))
                            .unwrap_or_default()
                    ),
                    remedy: remedy.to_owned(),
                }
            }
            CpuVirtualisation::Absent => Capability::Unavailable {
                detail: "/dev/kvm is absent and this CPU reports neither `vmx` nor `svm`, so hardware virtualisation is unavailable".to_owned(),
                remedy: "enable VT-x or AMD-V in firmware if the hardware supports it; otherwise dependency resolution is refused on this host, which is the intended behaviour rather than a downgrade (D9)".to_owned(),
            },
            CpuVirtualisation::Unknown => Capability::Unavailable {
                detail: "/dev/kvm is absent and CPU virtualisation support could not be determined from /proc/cpuinfo".to_owned(),
                remedy: "check that the hardware supports virtualisation and that the kvm module is loaded; without it, dependency resolution is refused (D9)".to_owned(),
            },
        };
    }
    match &observed.open_error {
        None => Capability::Available {
            detail: "/dev/kvm is accessible".to_owned(),
        },
        Some(error) => Capability::Blocked {
            detail: format!("/dev/kvm exists but is not accessible: {error}"),
            remedy: "add your user to the `kvm` group and re-login".to_owned(),
        },
    }
}

/// Whether `systemd-run --user` could reach the per-user manager.
///
/// A build task's wrapper chain starts with `systemd-run --user --scope`, and
/// the sandbox spawn clears the environment down to what that needs. libsystemd
/// resolves the user bus from `DBUS_SESSION_BUS_ADDRESS`, else from
/// `$XDG_RUNTIME_DIR/bus`; with neither it fails `Failed to connect to bus: No
/// medium found` and exits before `bwrap` runs. Inspection only — the variables
/// are read from this process, which is the environment that gets forwarded.
fn probe_session_bus(enclosure: &Enclosure) -> Capability {
    if let Some(address) = std::env::var_os("DBUS_SESSION_BUS_ADDRESS") {
        return Capability::Available {
            detail: format!(
                "user bus at {} (DBUS_SESSION_BUS_ADDRESS)",
                address.to_string_lossy()
            ),
        };
    }
    let remedy = if enclosure.is_container() {
        "this environment has no user session bus, and nothing inside it can create one: \
         run clyded on the host, or under a systemd user manager"
            .to_owned()
    } else {
        "start clyded inside a login session — `systemctl --user` or a terminal — rather than \
         as a system unit or under sudo, and `loginctl enable-linger $USER` so the manager \
         survives logout"
            .to_owned()
    };
    match std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        Some(dir) if dir.join("bus").exists() => Capability::Available {
            detail: format!("user bus at {}", dir.join("bus").display()),
        },
        Some(dir) => Capability::Blocked {
            detail: format!(
                "XDG_RUNTIME_DIR is {} but {} does not exist, so systemd-run --user has no bus \
                 to reach and every build task fails before bwrap starts",
                dir.display(),
                dir.join("bus").display()
            ),
            remedy,
        },
        None => Capability::Unavailable {
            detail: "neither DBUS_SESSION_BUS_ADDRESS nor XDG_RUNTIME_DIR is set, so \
                     systemd-run --user cannot resolve the per-user manager and every build \
                     task fails before bwrap starts"
                .to_owned(),
            remedy,
        },
    }
}

/// Whether each configured runtime root's programs survive being bound.
///
/// A runtime root is a `buildEnv`: a tree of symlinks into other store paths. A
/// program in it resolves on the host whatever the closure says, and resolves
/// *inside the sandbox* only if what it points at is bound too. When the closure
/// is just the root — which is what a missing manifest directory yields — every
/// binary dangles and the only diagnostic is an `execvp` ENOENT naming the
/// program rather than the configuration.
fn probe_runtime_roots(
    configured: &BTreeMap<RuntimeRootKind, PathBuf>,
    manifests: Option<&Path>,
) -> Capability {
    if configured.is_empty() {
        return Capability::Unavailable {
            detail: "no runtime roots are configured".to_owned(),
            remedy: "set sandbox.runtime_root_workspace, runtime_root_rust, and \
                     runtime_root_fetch in host configuration; a task executes against exactly \
                     one runtime root (D6)"
                .to_owned(),
        };
    }
    let remedy = "build the closure manifests and name them as \
                  sandbox.runtime_root_manifests: `nix build .#runtimeRootManifests`. Without \
                  them a root's closure is taken to be the root alone"
        .to_owned();

    let mut summaries = Vec::new();
    for (kind, path) in configured {
        let root = match RuntimeRoot::read(*kind, path, manifests) {
            Ok(root) => root,
            Err(error) => {
                return Capability::Blocked {
                    detail: format!(
                        "the {} runtime root at {}: {error}",
                        kind.name(),
                        path.display()
                    ),
                    remedy: format!(
                        "check sandbox.runtime_root_{} in host configuration, and that the store \
                         path still exists — add a GC root so nix-collect-garbage cannot remove it",
                        kind.name()
                    ),
                };
            }
        };
        let dangling: Vec<&String> = root
            .binaries
            .iter()
            .filter(|name| {
                root.program(name)
                    .is_none_or(|program| !root.resolves_inside_sandbox(&program))
            })
            .collect();
        if let Some(first) = dangling.first() {
            return Capability::Blocked {
                detail: format!(
                    "{} of {} programs in the {} runtime root would not resolve inside the \
                     sandbox, because they point outside the {} store path(s) its closure binds \
                     — {} is the first",
                    dangling.len(),
                    root.binaries.len(),
                    kind.name(),
                    root.closure.len(),
                    first
                ),
                remedy,
            };
        }
        summaries.push(format!(
            "{}: {} programs, {} store paths",
            kind.name(),
            root.binaries.len(),
            root.closure.len()
        ));
    }
    Capability::Available {
        detail: format!("closures bound — {}", summaries.join("; ")),
    }
}

fn probe_program(
    name: &'static str,
    configured: Option<&Path>,
    program: &str,
    remedy: &str,
) -> Capability {
    classify_resolved(
        name,
        resolve_program(configured, program).as_deref(),
        remedy,
    )
}

fn classify_resolved(name: &'static str, path: Option<&Path>, remedy: &str) -> Capability {
    match path {
        Some(path) => Capability::Available {
            detail: format!("{name} at {}", path.display()),
        },
        None => Capability::Unavailable {
            detail: format!("{name} was not found on PATH or in configuration"),
            remedy: remedy.to_owned(),
        },
    }
}

/// The project build programs whose presence on `PATH` is a bypass (D26).
///
/// A driver that can reach any of these can build project code without going
/// through a typed task, which is precisely what makes the pipeline advisory.
const TOOLCHAIN_PROGRAMS: [&str; 3] = ["cargo", "rustc", "rustup"];

/// Observes what posture is derived from.
///
/// Observation only: the classification is [`clyde_core::posture::derive`], a
/// pure function, so the cases worth diagnosing — a host with no toolchain in
/// particular — are testable on a machine that has one.
pub fn observe_posture(hosts_the_actor: bool) -> PostureObservation {
    PostureObservation {
        toolchain_on_path: TOOLCHAIN_PROGRAMS
            .into_iter()
            .filter_map(|program| {
                resolve_program(None, program)
                    .map(|path| format!("{program} at {}", path.display()))
            })
            .collect(),
        hosts_the_actor,
    }
}

/// Resolves a program by configured path first, then by `PATH`.
///
/// The configured path wins so a host can pin exactly which binary runs, which
/// matters for `bwrap` where the AppArmor profile is path-specific.
pub fn resolve_program(configured: Option<&Path>, program: &str) -> Option<PathBuf> {
    if let Some(path) = configured {
        return path.is_file().then(|| path.to_path_buf());
    }
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// Snapshots materialise by hardlink where the filesystem allows it, which is
/// what makes the read-only bind load-bearing rather than stylistic.
fn probe_hardlinks(state_dir: Option<&Path>) -> Capability {
    let dir = state_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    if std::fs::create_dir_all(&dir).is_err() {
        return Capability::Unavailable {
            detail: format!("{} is not writable", dir.display()),
            remedy: "point the state directory at a writable filesystem".to_owned(),
        };
    }
    let source = dir.join(format!(".clyde-hardlink-probe-{}", std::process::id()));
    let link = dir.join(format!(".clyde-hardlink-probe-{}-link", std::process::id()));
    let _ = std::fs::remove_file(&source);
    let _ = std::fs::remove_file(&link);
    let result =
        std::fs::write(&source, b"probe").and_then(|()| std::fs::hard_link(&source, &link));
    let capability = match result {
        Ok(()) => Capability::Available {
            detail: format!("hardlinks supported on {}", dir.display()),
        },
        Err(error) => Capability::Blocked {
            detail: format!("hardlinks are unavailable on {}: {error}", dir.display()),
            remedy: "snapshots fall back to copying, which costs the tree's bytes per run but stays correct and stays incremental: the copy carries the blob's mtime, so a build tool still sees unchanged files as unchanged (D27)".to_owned(),
        },
    };
    let _ = std::fs::remove_file(&source);
    let _ = std::fs::remove_file(&link);
    capability
}

fn read_sysctl(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Whether the guest kernel and root images are where the backend expects them.
///
/// A separate probe from `firecracker` because the remedy is different in kind:
/// a missing binary is an install step, a missing image is a `nix build` and a
/// symlink. Reporting both as "microVM unavailable" would send an operator to
/// the wrong one (R10).
fn probe_guest_images(vm_dir: Option<&Path>) -> Capability {
    let Some(directory) = vm_dir else {
        return Capability::Unavailable {
            detail: "no guest image directory was configured".to_owned(),
            remedy: GUEST_IMAGE_REMEDY.to_owned(),
        };
    };

    let kernel = directory.join("vmlinux");
    if !kernel.is_file() {
        return Capability::Unavailable {
            detail: format!("no guest kernel at {}", kernel.display()),
            remedy: GUEST_IMAGE_REMEDY.to_owned(),
        };
    }

    let rootfs = directory.join("rootfs");
    let missing: Vec<&str> = ["workspace", "rust", "fetch"]
        .into_iter()
        .filter(|kind| !rootfs.join(format!("{kind}.img")).is_file())
        .collect();
    if missing.is_empty() {
        Capability::Available {
            detail: format!("guest kernel and root images under {}", directory.display()),
        }
    } else {
        Capability::Unavailable {
            detail: format!(
                "guest kernel present, but no root image for: {}",
                missing.join(", ")
            ),
            remedy: GUEST_IMAGE_REMEDY.to_owned(),
        }
    }
}

const GUEST_IMAGE_REMEDY: &str = "build them from the flake and link them into the state directory: `nix build .#guestVm` then `ln -s \"$(readlink -f result)\" <state-dir>/vm`";

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]
    use super::*;

    fn available() -> Capability {
        Capability::Available {
            detail: "ok".to_owned(),
        }
    }

    fn unavailable() -> Capability {
        Capability::Unavailable {
            detail: "missing".to_owned(),
            remedy: "install it".to_owned(),
        }
    }

    fn report(
        userns: Capability,
        delegation: Capability,
        kvm: Capability,
        firecracker: Capability,
    ) -> HostReport {
        HostReport {
            user_namespaces: userns,
            cgroup_v2: available(),
            cgroup_delegation: delegation,
            session_bus: available(),
            runtime_roots: available(),
            kvm,
            bubblewrap: available(),
            firecracker,
            mke2fs: available(),
            guest_images: available(),
            nix: available(),
            hardlinks: available(),
            known_limitations: Vec::new(),
        }
    }

    #[test]
    fn build_capability_requires_cgroup_delegation() {
        let report = report(available(), unavailable(), available(), available());
        assert!(
            report.can_run_workspace(),
            "the workspace environment may still run on rlimits and a timeout"
        );
        assert!(
            !report.can_run_build(),
            "build tasks are refused without delegation, with no fallback (D22)"
        );
        assert!(!report.policy_view().cgroup_delegation);
    }

    #[test]
    fn a_blocked_prerequisite_is_named_rather_than_blamed_on_delegation() {
        // The regression: build inherits the workspace prerequisites, so a
        // report that always names delegation tells an operator whose
        // delegation is fine to go and fix delegation, citing D22 for a
        // refusal D22 had no part in.
        let report = report(unavailable(), available(), available(), available());
        assert!(!report.can_run_build());
        let named: Vec<_> = report
            .build_blockers()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(named, ["user namespaces"], "{named:?}");
    }

    #[test]
    fn delegation_is_named_when_it_is_the_one_that_failed() {
        let report = report(available(), unavailable(), available(), available());
        let named: Vec<_> = report
            .build_blockers()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(named, ["cgroup delegation"], "{named:?}");
    }

    #[test]
    fn a_host_that_can_build_has_no_blockers() {
        let report = report(available(), available(), available(), available());
        assert!(report.build_blockers().is_empty());
    }

    #[test]
    fn microvm_capability_requires_both_kvm_and_firecracker() {
        assert!(!report(available(), available(), unavailable(), available()).can_run_microvm());
        assert!(!report(available(), available(), available(), unavailable()).can_run_microvm());
        assert!(report(available(), available(), available(), available()).can_run_microvm());
    }

    #[test]
    fn strongest_isolation_falls_back_and_then_gives_up() {
        assert_eq!(
            report(available(), available(), available(), available()).strongest_isolation(),
            Some(IsolationLevel::MicroVm)
        );
        assert_eq!(
            report(available(), available(), unavailable(), unavailable()).strongest_isolation(),
            Some(IsolationLevel::NamespaceSandbox)
        );
        assert_eq!(
            report(unavailable(), available(), unavailable(), unavailable()).strongest_isolation(),
            None,
            "with no usable sandbox, nothing runs rather than something running unconfined"
        );
    }

    #[test]
    fn blocked_and_unavailable_both_carry_a_remedy() {
        let blocked = Capability::Blocked {
            detail: "d".to_owned(),
            remedy: "r".to_owned(),
        };
        assert_eq!(blocked.remedy(), Some("r"));
        assert_eq!(unavailable().remedy(), Some("install it"));
        assert_eq!(available().remedy(), None);
    }

    #[test]
    fn probing_this_host_produces_a_complete_report() {
        // The values depend on the host; what is asserted is that every probe
        // returns something with a detail, and that a blocked capability always
        // explains the fix.
        let report = probe(&ProbePaths::default());
        for capability in [
            &report.user_namespaces,
            &report.cgroup_v2,
            &report.cgroup_delegation,
            &report.kvm,
            &report.bubblewrap,
            &report.firecracker,
            &report.nix,
            &report.hardlinks,
        ] {
            assert!(!capability.detail().is_empty());
            if !capability.is_available() {
                assert!(
                    capability.remedy().is_some_and(|remedy| !remedy.is_empty()),
                    "a failing probe must state the remedy: {capability:?}"
                );
            }
        }
        assert!(!known_limitations().is_empty());
    }

    #[test]
    fn program_resolution_prefers_configured_paths() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("pretend-bwrap");
        std::fs::write(&program, b"#!/bin/sh\n").unwrap();
        assert_eq!(
            resolve_program(Some(&program), "bwrap"),
            Some(program.clone())
        );
        let missing = dir.path().join("absent");
        assert_eq!(resolve_program(Some(&missing), "bwrap"), None);
    }

    fn cgroups(read_only: bool, writable: bool, enclosure: Enclosure) -> CgroupObservation {
        CgroupObservation {
            hierarchy_present: true,
            own_path: Some("/user.slice/user-1000.slice".to_owned()),
            read_only_mount: read_only,
            subtree_control_writable: writable,
            enclosure,
        }
    }

    fn container() -> Enclosure {
        Enclosure::Container {
            evidence: "PID 1 is `tini`".to_owned(),
        }
    }

    #[test]
    fn a_runtime_root_whose_programs_point_outside_its_closure_is_blocked() {
        // A `buildEnv` in miniature: the root is symlinks into a store path the
        // closure does not name, which is what a missing manifest directory
        // produces and what fails as `bwrap: execvp …: No such file or directory`.
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store/cargo-1.97.1/bin");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("cargo"), b"#!/bin/sh\n").unwrap();
        let root = dir.path().join("roots/rust");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::os::unix::fs::symlink(store.join("cargo"), root.join("bin/cargo")).unwrap();

        let configured = [(RuntimeRootKind::Rust, root.clone())]
            .into_iter()
            .collect();
        let capability = probe_runtime_roots(&configured, None);
        let detail = capability.detail().to_owned();
        assert!(
            matches!(capability, Capability::Blocked { .. }),
            "a dangling program must block rather than read as available: {detail}"
        );
        assert!(
            detail.contains("cargo"),
            "the failing program is named: {detail}"
        );
        assert!(
            capability
                .remedy()
                .is_some_and(|remedy| remedy.contains("runtime_root_manifests")),
            "the remedy must name the key that fixes it"
        );

        // With a manifest naming the store path, the same root is fine.
        let manifests = dir.path().join("manifests/rust");
        std::fs::create_dir_all(&manifests).unwrap();
        std::fs::write(
            manifests.join("store-paths"),
            format!(
                "{}\n{}",
                root.display(),
                dir.path().join("store/cargo-1.97.1").display()
            ),
        )
        .unwrap();
        assert!(
            probe_runtime_roots(&configured, Some(&dir.path().join("manifests"))).is_available(),
            "naming the closure is what makes the root usable"
        );
    }

    #[test]
    fn no_configured_runtime_roots_is_unavailable_rather_than_silently_fine() {
        let capability = probe_runtime_roots(&BTreeMap::new(), None);
        assert!(!capability.is_available());
        assert!(
            capability
                .remedy()
                .is_some_and(|r| r.contains("runtime_root_rust"))
        );
    }

    #[test]
    fn build_capability_requires_a_reachable_session_bus() {
        // A delegated subtree the wrapper cannot reach is still no build host:
        // `systemd-run --user` fails before bwrap starts.
        let mut host = report(available(), available(), available(), available());
        host.session_bus = Capability::Unavailable {
            detail: "no bus".to_owned(),
            remedy: "start clyded in a login session".to_owned(),
        };
        assert!(
            !host.can_run_build(),
            "reporting ok here would be a lie the first run exposes"
        );
        assert!(
            host.build_blockers()
                .iter()
                .any(|(label, _)| *label == "session bus"),
            "the blocker must be named, not folded into delegation"
        );
        assert!(
            host.can_run_workspace(),
            "the workspace environment gets no cgroup scope and needs no bus"
        );
    }

    #[test]
    fn a_read_only_cgroupfs_is_not_reported_as_a_session_misconfiguration() {
        // The container case: the old diagnostic said "run under a systemd user
        // session with Delegate=yes", which cannot be done from inside a
        // container whose whole cgroupfs is read-only. A remedy that cannot
        // work is worse than no remedy, because it is followed.
        let capability = classify_cgroup_delegation(&cgroups(true, false, container()));
        let remedy = capability
            .remedy()
            .expect("a blocked capability explains the fix");
        assert!(
            capability.detail().contains("read-only"),
            "the specific finding must be named: {}",
            capability.detail()
        );
        assert!(
            !remedy.contains("Delegate=yes"),
            "the session remedy cannot work here: {remedy}"
        );
        assert!(
            remedy.contains("on the host"),
            "the remedy has to point outside the container: {remedy}"
        );
    }

    #[test]
    fn an_unwritable_subtree_on_a_host_still_names_the_session_remedy() {
        let capability = classify_cgroup_delegation(&cgroups(false, false, Enclosure::Host));
        let remedy = capability.remedy().unwrap();
        assert!(remedy.contains("Delegate"), "{remedy}");
        assert!(
            remedy.contains("enable-linger"),
            "a non-login session is the other half of this case: {remedy}"
        );
    }

    #[test]
    fn the_same_observation_gives_different_remedies_by_enclosure() {
        // The verdict is identical; only the remedy differs. That is the whole
        // point of detecting the enclosure, and it is why detection cannot be
        // allowed to change what the host may run.
        let host = classify_cgroup_delegation(&cgroups(false, false, Enclosure::Host));
        let inside = classify_cgroup_delegation(&cgroups(false, false, container()));
        assert!(!host.is_available() && !inside.is_available());
        assert_eq!(host.detail(), inside.detail());
        assert_ne!(host.remedy(), inside.remedy());
    }

    #[test]
    fn delegation_available_regardless_of_enclosure() {
        assert!(classify_cgroup_delegation(&cgroups(false, true, container())).is_available());
        assert!(classify_cgroup_delegation(&cgroups(false, true, Enclosure::Host)).is_available());
    }

    fn kvm(present: bool, cpu: CpuVirtualisation, enclosure: Enclosure) -> KvmObservation {
        KvmObservation {
            device_present: present,
            open_error: None,
            cpu,
            enclosure,
        }
    }

    #[test]
    fn a_missing_device_on_a_virtualisation_capable_cpu_is_not_missing_hardware() {
        // "enable hardware virtualisation" on a CPU that already reports vmx is
        // a dead end: the device is simply not exposed here.
        for extension in [CpuVirtualisation::Vmx, CpuVirtualisation::Svm] {
            let capability = classify_kvm(&kvm(false, extension, container()));
            let detail = capability.detail();
            let remedy = capability.remedy().unwrap();
            assert!(
                detail.contains("not present here") || detail.contains("supported"),
                "{detail}"
            );
            assert!(
                !remedy.contains("firmware"),
                "the firmware remedy is wrong when the CPU already reports the extension: {remedy}"
            );
            assert!(
                remedy.contains("/dev/kvm") || remedy.contains("host"),
                "{remedy}"
            );
        }
    }

    #[test]
    fn a_missing_device_on_a_capable_cpu_on_a_host_names_the_module_and_the_group() {
        let remedy = classify_kvm(&kvm(false, CpuVirtualisation::Vmx, Enclosure::Host))
            .remedy()
            .unwrap()
            .to_owned();
        assert!(remedy.contains("modprobe"), "{remedy}");
        assert!(remedy.contains("kvm` group"), "{remedy}");
        assert!(
            remedy.contains("nested"),
            "running inside a VM is the common case for this: {remedy}"
        );
    }

    #[test]
    fn hardware_without_the_extension_says_so_and_cites_the_decision() {
        let capability = classify_kvm(&kvm(false, CpuVirtualisation::Absent, Enclosure::Host));
        assert!(
            capability.detail().contains("neither"),
            "{}",
            capability.detail()
        );
        let remedy = capability.remedy().unwrap();
        assert!(remedy.contains("D9"), "the refusal is by design: {remedy}");
    }

    #[test]
    fn an_undeterminable_cpu_does_not_claim_either_way() {
        let capability = classify_kvm(&kvm(false, CpuVirtualisation::Unknown, Enclosure::Host));
        assert!(
            capability.detail().contains("could not be determined"),
            "{}",
            capability.detail()
        );
    }

    #[test]
    fn an_inaccessible_device_is_blocked_rather_than_unavailable() {
        let observed = KvmObservation {
            device_present: true,
            open_error: Some("Permission denied (os error 13)".to_owned()),
            cpu: CpuVirtualisation::Vmx,
            enclosure: Enclosure::Host,
        };
        let capability = classify_kvm(&observed);
        assert!(matches!(capability, Capability::Blocked { .. }));
        assert!(capability.remedy().unwrap().contains("kvm` group"));
    }

    #[test]
    fn an_accessible_device_is_available() {
        assert!(classify_kvm(&kvm(true, CpuVirtualisation::Vmx, Enclosure::Host)).is_available());
    }

    #[test]
    fn the_apparmor_remedy_says_it_cannot_be_applied_from_inside_a_container() {
        // The remedy is a pure function of the enclosure, so this holds on every
        // host rather than only on one that sets the sysctl.
        let inside = apparmor_remedy(&container());
        assert!(
            inside.contains("host kernel"),
            "the profile and the sysctl both belong to the host: {inside}"
        );
        assert!(!apparmor_remedy(&Enclosure::Host).contains("host kernel"));
    }

    #[test]
    fn a_bwrap_that_cannot_be_run_is_not_read_as_a_denial() {
        // The empirical probe answers "not the host's answer" rather than
        // "denied" when it cannot run, so a missing binary never masquerades as
        // an AppArmor refusal.
        assert_eq!(bwrap_can_unshare(Path::new("/nonexistent/bwrap")), None);
    }

    #[test]
    fn a_permitting_profile_leaves_the_sysctl_set_so_the_sysctl_cannot_decide() {
        // The regression this guards: a profile permitting one binary leaves
        // kernel.apparmor_restrict_unprivileged_userns=1, so a verdict inferred
        // from the sysctl reports the applied remedy as still outstanding
        // forever. With no bwrap to run, the fallback must say the weaker claim
        // is what it is.
        let restricted = read_sysctl("/proc/sys/kernel/apparmor_restrict_unprivileged_userns")
            .map(|text| text.trim() == "1")
            .unwrap_or(false);
        if !restricted {
            return;
        }
        let unconfirmable = probe_user_namespaces(&Enclosure::Host, None);
        assert!(
            unconfirmable
                .detail()
                .contains("no bwrap could be run to confirm"),
            "an unconfirmed verdict must not read as a confirmed denial: {}",
            unconfirmable.detail()
        );
    }

    #[test]
    fn enclosure_detection_never_changes_what_the_host_may_run() {
        // A regression guard on the shape of the change: detection feeds the
        // remedy text, and the capability predicates read only the verdicts.
        let blocked = |remedy: &str| Capability::Blocked {
            detail: "d".to_owned(),
            remedy: remedy.to_owned(),
        };
        let with = |remedy: &str| report(available(), blocked(remedy), available(), available());
        assert_eq!(
            with("host remedy").can_run_build(),
            with("container remedy").can_run_build(),
            "the remedy text cannot influence admission"
        );
        assert!(!with("either").can_run_build(), "still refused (D22)");
    }
}
