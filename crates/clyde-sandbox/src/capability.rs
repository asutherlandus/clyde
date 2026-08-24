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

use std::path::{Path, PathBuf};

use clyde_core::classification::IsolationLevel;

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
    pub kvm: Capability,
    pub bubblewrap: Capability,
    pub firecracker: Capability,
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
    pub fn can_run_build(&self) -> bool {
        self.can_run_workspace() && self.cgroup_delegation.is_available()
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
    pub nix: Option<PathBuf>,
    /// Directory used for the hardlink probe; the daemon's state directory.
    pub state_dir: Option<PathBuf>,
}

/// Probes every host prerequisite.
///
/// Pure inspection: nothing is created that outlives the call, and no probe
/// requires privilege.
pub fn probe(paths: &ProbePaths) -> HostReport {
    HostReport {
        user_namespaces: probe_user_namespaces(),
        cgroup_v2: probe_cgroup_v2(),
        cgroup_delegation: probe_cgroup_delegation(),
        kvm: probe_kvm(),
        bubblewrap: probe_program(
            "bubblewrap",
            paths.bwrap.as_deref(),
            "bwrap",
            "add `bubblewrap` to the flake devShell, or set sandbox.bwrap in host configuration",
        ),
        firecracker: probe_program(
            "firecracker",
            paths.firecracker.as_deref(),
            "firecracker",
            "install firecracker and set sandbox.firecracker in host configuration; without it, microVM tasks are refused",
        ),
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
fn probe_user_namespaces() -> Capability {
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
        return Capability::Blocked {
            detail: "kernel.apparmor_restrict_unprivileged_userns=1 blocks unprivileged user namespaces for binaries without a permitting AppArmor profile, which includes a nix-store bwrap".to_owned(),
            remedy: "install an AppArmor profile granting `userns create` for the nix-store bwrap path (narrowest, survives reboot); or `sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`, which re-enables unprivileged userns for everything on the host".to_owned(),
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
fn probe_cgroup_delegation() -> Capability {
    if !Path::new("/sys/fs/cgroup/cgroup.controllers").exists() {
        return Capability::Unavailable {
            detail: "no cgroup v2 hierarchy, so delegation cannot exist".to_owned(),
            remedy: "use a host with cgroup v2; build tasks are refused without it".to_owned(),
        };
    }
    let user_slice = std::fs::read_to_string("/proc/self/cgroup")
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("0::").map(str::to_owned))
        });
    let Some(path) = user_slice else {
        return Capability::Unavailable {
            detail: "this process is not in a cgroup v2 cgroup".to_owned(),
            remedy: "run under a systemd user session so a delegated slice exists".to_owned(),
        };
    };
    let delegated = Path::new("/sys/fs/cgroup").join(path.trim_start_matches('/'));
    let subtree = delegated.join("cgroup.subtree_control");
    // Writability of the delegated cgroup's subtree_control is the operational
    // question: without it, no scope with limits can be created.
    let writable = std::fs::OpenOptions::new()
        .append(true)
        .open(&subtree)
        .is_ok();
    if writable {
        Capability::Available {
            detail: format!("cgroup v2 delegated at {}", delegated.display()),
        }
    } else {
        Capability::Blocked {
            detail: format!(
                "cgroup v2 is present but {} is not writable, so no delegated scope can be created",
                subtree.display()
            ),
            remedy: "run under a systemd user session with Delegate=yes (`systemctl --user status`), or use `systemd-run --user --scope`; build tasks are refused without delegation (D22)".to_owned(),
        }
    }
}

fn probe_kvm() -> Capability {
    let kvm = Path::new("/dev/kvm");
    if !kvm.exists() {
        return Capability::Unavailable {
            detail: "/dev/kvm is absent, so the microVM backend cannot run".to_owned(),
            remedy: "enable hardware virtualisation, or accept that dependency resolution is refused on this host".to_owned(),
        };
    }
    match std::fs::OpenOptions::new().read(true).write(true).open(kvm) {
        Ok(_) => Capability::Available {
            detail: "/dev/kvm is accessible".to_owned(),
        },
        Err(error) => Capability::Blocked {
            detail: format!("/dev/kvm exists but is not accessible: {error}"),
            remedy: "add your user to the `kvm` group and re-login".to_owned(),
        },
    }
}

fn probe_program(
    name: &'static str,
    configured: Option<&Path>,
    program: &str,
    remedy: &str,
) -> Capability {
    match resolve_program(configured, program) {
        Some(path) => Capability::Available {
            detail: format!("{name} at {}", path.display()),
        },
        None => Capability::Unavailable {
            detail: format!("{name} was not found on PATH or in configuration"),
            remedy: remedy.to_owned(),
        },
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
            remedy: "snapshots will fall back to copying, which is slower but correct".to_owned(),
        },
    };
    let _ = std::fs::remove_file(&source);
    let _ = std::fs::remove_file(&link);
    capability
}

fn read_sysctl(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
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
            kvm,
            bubblewrap: available(),
            firecracker,
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
}
