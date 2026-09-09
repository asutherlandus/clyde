//! The bubblewrap backend (D5, Phase 2a deliverable 2).
//!
//! Bubblewrap is knowingly a weaker boundary than the design calls for on
//! untrusted execution. It is acceptable only because Phase 2b follows and
//! nothing network-bearing ships on it: `rust.resolve-deps` demands microVM
//! isolation, and `preflight` here refuses anything that does.
//!
//! The command construction is a pure function so that "what exactly does Clyde
//! run" is a value a test can assert on and an audit record can carry, rather
//! than something assembled inline at spawn time.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use chrono::Utc;
use clyde_core::classification::{BackendKind, IsolationLevel, TrustClass};

use crate::backend::{
    BoxFuture, ExitStatus, SandboxBackend, SandboxHandle, terminate_child, wait_with_deadline,
};
use crate::error::{Result, SandboxError};
use crate::limits::{LimitTools, wrap_with_limits};
use crate::seccomp::{FilterArch, SeccompFilter};
use crate::spec::{Mount, MountMode, MountPurpose, SandboxSpec};

/// Where the seccomp filter is handed to `bwrap`.
///
/// `bwrap --seccomp FD` reads the filter from an inherited descriptor. Passing
/// it as the child's stdin keeps this in safe Rust: `Stdio::from(File)` places
/// the file on descriptor 0 with no `pre_exec` hook and no fd juggling. The
/// sandboxed process has no use for stdin.
const SECCOMP_FD: u8 = 0;

/// The bubblewrap backend.
#[derive(Debug, Clone)]
pub struct BubblewrapBackend {
    bwrap: PathBuf,
    tools: LimitTools,
    /// Directory for per-sandbox scratch files such as the seccomp filter.
    runtime_dir: PathBuf,
}

impl BubblewrapBackend {
    pub fn new(bwrap: PathBuf, tools: LimitTools, runtime_dir: PathBuf) -> Self {
        Self {
            bwrap,
            tools,
            runtime_dir,
        }
    }

    /// The full argv, including the limit wrapper.
    pub fn command_for(&self, spec: &SandboxSpec, seccomp: bool) -> Result<Vec<String>> {
        spec.validate()?;
        self.preflight(spec)?;
        let inner = self.bwrap_argv(spec, seccomp);
        wrap_with_limits(&spec.id, spec.trust_class, &spec.limits, &self.tools, inner)
    }

    /// The `bwrap` invocation itself, without the limit wrapper.
    fn bwrap_argv(&self, spec: &SandboxSpec, seccomp: bool) -> Vec<String> {
        let mut argv = vec![self.bwrap.to_string_lossy().to_string()];

        // Namespace isolation. `--unshare-net` is unconditional: every sandbox,
        // for every profile including the ones with egress, gets a loopback-only
        // network namespace. Reachability comes from the bound proxy socket, not
        // from the network namespace (network egress model).
        argv.extend(
            [
                "--unshare-user",
                "--unshare-pid",
                "--unshare-ipc",
                "--unshare-uts",
                "--unshare-cgroup-try",
                "--unshare-net",
                "--die-with-parent",
                "--new-session",
                "--clearenv",
            ]
            .into_iter()
            .map(str::to_owned),
        );

        // A stable, non-root identity inside the sandbox. Mapping to uid 0 would
        // make in-sandbox tooling believe it is privileged.
        argv.extend(
            ["--uid", "1000", "--gid", "1000"]
                .into_iter()
                .map(str::to_owned),
        );
        argv.extend(
            ["--hostname", "clyde-sandbox"]
                .into_iter()
                .map(str::to_owned),
        );

        // Pseudo-filesystems.
        argv.extend(["--proc", "/proc"].into_iter().map(str::to_owned));
        argv.extend(["--dev", "/dev"].into_iter().map(str::to_owned));

        // The runtime root closure. Every store path is bound at its own path so
        // that interpreter and library references inside the closure resolve.
        for path in &spec.runtime_root_closure {
            argv.push("--ro-bind".to_owned());
            argv.push(path.to_string_lossy().to_string());
            argv.push(path.to_string_lossy().to_string());
        }

        for mount in &spec.mounts {
            argv.extend(mount_arguments(mount));
        }

        argv.extend([
            "--tmpfs".to_owned(),
            "/tmp".to_owned(),
            "--tmpfs".to_owned(),
            "/scratch".to_owned(),
        ]);

        for (key, value) in &spec.env {
            argv.push("--setenv".to_owned());
            argv.push(key.clone());
            argv.push(value.clone());
        }

        argv.push("--chdir".to_owned());
        argv.push(spec.cwd.to_string_lossy().to_string());

        if seccomp {
            argv.push("--seccomp".to_owned());
            argv.push(SECCOMP_FD.to_string());
        }

        argv.push("--".to_owned());
        argv.extend(spec.argv.iter().cloned());
        argv
    }

    /// Writes the seccomp filter to a file whose descriptor becomes the child's
    /// stdin.
    fn seccomp_file(&self, spec: &SandboxSpec) -> Result<Option<std::fs::File>> {
        let Some(arch) = FilterArch::host() else {
            return Ok(None);
        };
        std::fs::create_dir_all(&self.runtime_dir)
            .map_err(|error| SandboxError::io("creating the sandbox runtime directory", error))?;
        let path = self.runtime_dir.join(format!("{}.seccomp", spec.id));
        let filter = SeccompFilter::build(arch);
        std::fs::write(&path, filter.to_bytes())
            .map_err(|error| SandboxError::io("writing the seccomp filter", error))?;
        let file = std::fs::File::open(&path)
            .map_err(|error| SandboxError::io("opening the seccomp filter", error))?;
        // The filter has been read into the descriptor; the file itself need not
        // outlive the spawn.
        let _ = std::fs::remove_file(&path);
        Ok(Some(file))
    }
}

/// The `bwrap` arguments for one mount.
fn mount_arguments(mount: &Mount) -> Vec<String> {
    let target = mount.target.to_string_lossy().to_string();
    match (&mount.source, mount.mode) {
        (_, MountMode::Tmpfs { size_bytes }) => vec![
            "--size".to_owned(),
            size_bytes.to_string(),
            "--tmpfs".to_owned(),
            target,
        ],
        (Some(source), MountMode::ReadOnly) => vec![
            "--ro-bind".to_owned(),
            source.to_string_lossy().to_string(),
            target,
        ],
        (Some(source), MountMode::ReadWrite | MountMode::Socket) => vec![
            "--bind".to_owned(),
            source.to_string_lossy().to_string(),
            target,
        ],
        // A non-tmpfs mount with no source is rejected by `SandboxSpec::validate`
        // before this point; producing nothing keeps the function total.
        (None, _) => Vec::new(),
    }
}

impl SandboxBackend for BubblewrapBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Bubblewrap
    }

    fn isolation_level(&self) -> IsolationLevel {
        IsolationLevel::NamespaceSandbox
    }

    fn preflight(&self, spec: &SandboxSpec) -> Result<()> {
        spec.validate()?;
        if spec.min_isolation > self.isolation_level() {
            return Err(SandboxError::Unsupported {
                backend: "bubblewrap",
                requirement: format!(
                    "{} isolation; this backend provides {}",
                    spec.min_isolation,
                    self.isolation_level()
                ),
            });
        }
        // Boundary-shaped refusals are reported before host-tooling problems, so
        // "this task cannot run at this boundary" is not masked by "a binary is
        // missing" on a host that has both problems.
        if spec.trust_class.requires_cgroup_limits() && !self.tools.cgroup_delegation {
            return Err(SandboxError::CgroupDelegationUnavailable {
                detail: format!(
                    "{} execution requires cgroup v2 limits, which this host cannot delegate",
                    spec.trust_class
                ),
            });
        }
        // Untrusted execution without a seccomp filter is a weaker boundary than
        // the design states, so it is refused rather than run.
        if spec.trust_class >= TrustClass::T2 && FilterArch::host().is_none() {
            return Err(SandboxError::Unsupported {
                backend: "bubblewrap",
                requirement: "a seccomp filter for this architecture".to_owned(),
            });
        }
        for path in &spec.runtime_root_closure {
            if !path.exists() {
                return Err(SandboxError::RuntimeRoot {
                    root: path.clone(),
                    detail: "closure path does not exist on this host".to_owned(),
                });
            }
        }
        if !self.bwrap.is_file() {
            return Err(SandboxError::ProgramNotFound {
                program: self.bwrap.clone(),
            });
        }
        Ok(())
    }

    fn start<'a>(&'a self, spec: SandboxSpec) -> BoxFuture<'a, Result<SandboxHandle>> {
        Box::pin(async move {
            self.preflight(&spec)?;
            let seccomp = self.seccomp_file(&spec)?;
            let argv = self.command_for(&spec, seccomp.is_some())?;
            let Some((program, arguments)) = argv.split_first() else {
                return Err(SandboxError::Spec(crate::spec::SpecViolation::EmptyArgv));
            };

            let mut command = tokio::process::Command::new(program);
            command.args(arguments);
            command.kill_on_drop(true);
            // The daemon's own environment must not leak in; `--clearenv` covers
            // the sandbox, and this covers the wrapper processes.
            command.env_clear();
            // Except the two variables `systemd-run --user` needs to find the
            // per-user manager. Cleared, it cannot resolve a bus address and
            // exits 1 before `bwrap` is ever reached, which surfaces as a task
            // that failed in milliseconds with no diagnostic to classify. These
            // reach the wrapper only: `--clearenv` still applies to the payload,
            // so nothing here is visible to project code.
            for (key, value) in
                session_bus_environment(spec.trust_class, |key| std::env::var(key).ok())
            {
                command.env(key, value);
            }
            command.stdin(match seccomp {
                Some(file) => Stdio::from(file),
                None => Stdio::null(),
            });
            command.stdout(open_log(spec.stdout_path.as_deref())?);
            command.stderr(open_log(spec.stderr_path.as_deref())?);

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
                trust_class = %spec.trust_class,
                egress = spec.egress.name(),
                "sandbox started"
            );
            Ok(SandboxHandle::new(
                spec.id,
                BackendKind::Bubblewrap,
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

/// The session-bus variables the `systemd-run --user` wrapper needs.
///
/// Only for a trust class that gets a cgroup scope — the workspace environment
/// runs on rlimits alone and needs no bus. `DBUS_SESSION_BUS_ADDRESS` wins where
/// it is set; otherwise libsystemd derives `$XDG_RUNTIME_DIR/bus`, so passing
/// the runtime directory is enough. Both are read from the daemon's own
/// environment, which is where the session it belongs to is recorded.
/// `lookup` is the environment to read from, injected so this is testable
/// without mutating the process environment.
fn session_bus_environment(
    trust_class: TrustClass,
    lookup: impl Fn(&str) -> Option<String>,
) -> Vec<(&'static str, String)> {
    if !trust_class.requires_cgroup_limits() {
        return Vec::new();
    }
    ["XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS"]
        .into_iter()
        .filter_map(|key| lookup(key).map(|value| (key, value)))
        .collect()
}

/// Opens a log destination, or discards output when no path is given.
fn open_log(path: Option<&Path>) -> Result<Stdio> {
    match path {
        None => Ok(Stdio::null()),
        Some(path) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| SandboxError::io("creating the log directory", error))?;
            }
            let file = std::fs::File::create(path)
                .map_err(|error| SandboxError::io("creating the log file", error))?;
            Ok(Stdio::from(file))
        }
    }
}

/// Whether a spec would place a credential-shaped host path inside the sandbox.
///
/// Used by the Phase 2a security tests, which assert that no build sandbox can
/// reach the host home, `~/.ssh`, `~/.gnupg`, a browser profile, or a container
/// runtime socket.
pub fn credential_shaped_mounts(spec: &SandboxSpec) -> Vec<PathBuf> {
    const FORBIDDEN: [&str; 8] = [
        "/root",
        "/var/run/docker.sock",
        "/run/docker.sock",
        "/run/podman",
        "/run/user",
        "/etc/shadow",
        "/etc/sudoers",
        "/proc/1",
    ];
    let home = std::env::var_os("HOME").map(PathBuf::from);
    spec.mounts
        .iter()
        .filter_map(|mount| mount.source.as_ref())
        .filter(|source| {
            let string = source.to_string_lossy();
            let named = FORBIDDEN
                .into_iter()
                .any(|forbidden| string.starts_with(forbidden));
            let dotfile = [".ssh", ".gnupg", ".aws", ".config/gcloud", ".docker"]
                .into_iter()
                .any(|dir| match &home {
                    Some(home) => source.starts_with(home.join(dir)),
                    None => false,
                });
            named || dotfile
        })
        .cloned()
        .collect()
}

/// Whether a mount table exposes the workspace outside its snapshot.
///
/// A build sandbox reads project source only through a snapshot mount; a live
/// workspace bind in a T2 sandbox would defeat the immutability property.
pub fn live_workspace_mounts(spec: &SandboxSpec) -> Vec<&Mount> {
    spec.mounts
        .iter()
        .filter(|mount| {
            matches!(
                mount.purpose,
                MountPurpose::EditScope | MountPurpose::ReadScope
            )
        })
        .collect()
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
    use clyde_core::classification::{EgressProfile, ResourceLimits};
    use clyde_core::task::RuntimeRootKind;

    use crate::spec::ScratchPolicy;

    fn backend(delegation: bool) -> BubblewrapBackend {
        BubblewrapBackend::new(
            PathBuf::from("/nix/store/bwrap/bin/bwrap"),
            LimitTools {
                systemd_run: Some(PathBuf::from("/usr/bin/systemd-run")),
                prlimit: Some(PathBuf::from("/usr/bin/prlimit")),
                cgroup_delegation: delegation,
            },
            std::env::temp_dir().join("clyde-sandbox-test"),
        )
    }

    fn spec(trust_class: TrustClass, egress: EgressProfile) -> SandboxSpec {
        let mut mounts = vec![Mount {
            source: Some(PathBuf::from("/var/lib/clyde/snapshots/s1")),
            target: PathBuf::from("/work"),
            mode: MountMode::ReadOnly,
            purpose: MountPurpose::Snapshot,
        }];
        if !egress.is_none() {
            mounts.push(Mount {
                source: Some(PathBuf::from("/run/clyde/egress.sock")),
                target: PathBuf::from("/run/clyde/egress.sock"),
                mode: MountMode::Socket,
                purpose: MountPurpose::EgressSocket,
            });
        }
        SandboxSpec {
            id: "sb-1".to_owned(),
            runtime_root_kind: RuntimeRootKind::Rust,
            runtime_root: PathBuf::from("/nix/store/rust-root"),
            runtime_root_closure: vec![
                PathBuf::from("/nix/store/rust-root"),
                PathBuf::from("/nix/store/glibc"),
            ],
            mounts,
            egress,
            limits: ResourceLimits {
                max_wall_clock: HumanDuration::parse("30m").unwrap(),
                max_memory_bytes: 8 << 30,
                max_cpu_percent: 400,
                max_tasks: 512,
                max_open_files: 4096,
            },
            trust_class,
            min_isolation: IsolationLevel::NamespaceSandbox,
            env: BTreeMap::from([("CARGO_NET_OFFLINE".to_owned(), "true".to_owned())]),
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

    fn argv(backend: &BubblewrapBackend, spec: &SandboxSpec) -> Vec<String> {
        backend.bwrap_argv(spec, true)
    }

    #[test]
    fn every_sandbox_unshares_the_network_even_with_an_egress_profile() {
        // Reachability comes from the bound proxy socket, never from the network
        // namespace, so both cases must unshare.
        for egress in [EgressProfile::None, EgressProfile::RustRegistry] {
            let spec = spec(TrustClass::T2, egress);
            let rendered = argv(&backend(true), &spec).join(" ");
            assert!(
                rendered.contains("--unshare-net"),
                "the sandbox must have a loopback-only netns"
            );
        }
    }

    #[test]
    fn the_environment_is_cleared_and_repopulated_explicitly() {
        let spec = spec(TrustClass::T2, EgressProfile::None);
        let rendered = argv(&backend(true), &spec);
        assert!(rendered.iter().any(|arg| arg == "--clearenv"));
        let index = rendered.iter().position(|arg| arg == "--setenv").unwrap();
        assert_eq!(rendered[index + 1], "CARGO_NET_OFFLINE");
        assert_eq!(rendered[index + 2], "true");
    }

    #[test]
    fn a_scoped_task_keeps_the_session_bus_variables_the_wrapper_needs() {
        let session = |key: &str| match key {
            "XDG_RUNTIME_DIR" => Some("/run/user/1000".to_owned()),
            _ => None,
        };
        let passed = session_bus_environment(TrustClass::T2, session);
        assert!(
            passed
                .iter()
                .any(|(key, value)| *key == "XDG_RUNTIME_DIR" && value == "/run/user/1000"),
            "systemd-run --user cannot resolve a bus address without it: {passed:?}"
        );
        assert!(
            session_bus_environment(TrustClass::T1, session).is_empty(),
            "the workspace environment gets no cgroup scope and needs no bus"
        );
    }

    #[test]
    fn the_whole_runtime_root_closure_is_bound_read_only() {
        let spec = spec(TrustClass::T2, EgressProfile::None);
        let rendered = argv(&backend(true), &spec).join(" ");
        // Binding only the root would give a sandbox whose binaries cannot find
        // their libraries.
        assert!(rendered.contains("--ro-bind /nix/store/rust-root /nix/store/rust-root"));
        assert!(rendered.contains("--ro-bind /nix/store/glibc /nix/store/glibc"));
    }

    #[test]
    fn snapshots_are_bound_read_only_and_caches_read_write() {
        let mut spec = spec(TrustClass::T2, EgressProfile::None);
        spec.mounts.push(Mount {
            source: Some(PathBuf::from("/var/lib/clyde/missions/m1/cargo-target")),
            target: PathBuf::from("/cache/target"),
            mode: MountMode::ReadWrite,
            purpose: MountPurpose::MissionCache,
        });
        let rendered = argv(&backend(true), &spec).join(" ");
        assert!(rendered.contains("--ro-bind /var/lib/clyde/snapshots/s1 /work"));
        assert!(rendered.contains("--bind /var/lib/clyde/missions/m1/cargo-target /cache/target"));
    }

    #[test]
    fn the_seccomp_filter_is_referenced_only_when_present() {
        let spec = spec(TrustClass::T2, EgressProfile::None);
        let with = backend(true).bwrap_argv(&spec, true).join(" ");
        assert!(with.contains("--seccomp 0"));
        let without = backend(true).bwrap_argv(&spec, false).join(" ");
        assert!(!without.contains("--seccomp"));
    }

    #[test]
    fn the_command_is_wrapped_in_a_cgroup_scope_for_build_tasks() {
        let spec = spec(TrustClass::T2, EgressProfile::None);
        // `command_for` also runs preflight, which needs the binaries to exist;
        // the wrapper composition is what is asserted here.
        let inner = backend(true).bwrap_argv(&spec, true);
        let wrapped = wrap_with_limits(
            &spec.id,
            spec.trust_class,
            &spec.limits,
            &LimitTools {
                systemd_run: Some(PathBuf::from("/usr/bin/systemd-run")),
                prlimit: Some(PathBuf::from("/usr/bin/prlimit")),
                cgroup_delegation: true,
            },
            inner,
        )
        .unwrap();
        assert!(wrapped[0].ends_with("systemd-run"));
        assert!(wrapped.iter().any(|arg| arg.ends_with("bwrap")));
    }

    #[test]
    fn preflight_refuses_a_microvm_only_spec() {
        let mut spec = spec(TrustClass::T3, EgressProfile::RustRegistry);
        spec.min_isolation = IsolationLevel::MicroVm;
        let error = backend(true)
            .preflight(&spec)
            .expect_err("a namespace sandbox must not run a microVM-only task");
        assert!(matches!(error, SandboxError::Unsupported { .. }));
    }

    #[test]
    fn preflight_refuses_a_build_task_without_delegation() {
        let spec = spec(TrustClass::T2, EgressProfile::None);
        let error = backend(false)
            .preflight(&spec)
            .expect_err("T2 without delegation must be refused");
        assert!(matches!(
            error,
            SandboxError::CgroupDelegationUnavailable { .. }
        ));
    }

    #[test]
    fn preflight_reports_a_missing_runtime_root_closure_path() {
        let mut spec = spec(TrustClass::T1, EgressProfile::None);
        spec.runtime_root_closure = vec![PathBuf::from("/nix/store/definitely-absent")];
        let error = backend(true).preflight(&spec).expect_err("missing closure");
        assert!(matches!(error, SandboxError::RuntimeRoot { .. }));
    }

    #[test]
    fn credential_shaped_mounts_are_detected() {
        let mut spec = spec(TrustClass::T2, EgressProfile::None);
        assert!(credential_shaped_mounts(&spec).is_empty());
        spec.mounts.push(Mount {
            source: Some(PathBuf::from("/run/docker.sock")),
            target: PathBuf::from("/run/docker.sock"),
            mode: MountMode::Socket,
            purpose: MountPurpose::ActorSocket,
        });
        assert_eq!(credential_shaped_mounts(&spec).len(), 1);
    }

    #[test]
    fn a_build_spec_has_no_live_workspace_mounts() {
        let spec = spec(TrustClass::T2, EgressProfile::None);
        assert!(
            live_workspace_mounts(&spec).is_empty(),
            "project code executes only against snapshots, never the live tree"
        );
    }
}
