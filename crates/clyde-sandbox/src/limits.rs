//! Resource limit application.
//!
//! Limits are applied by wrapping the sandbox command rather than by setting
//! them in a `pre_exec` hook. That keeps the whole mechanism at the argv level:
//! it contains no `unsafe`, and the exact command Clyde runs is a value that can
//! be asserted in a test and printed in an audit record.
//!
//! Cgroup v2 limits are **mandatory** for T2 and above. On a host without
//! delegation, build and test tasks are refused — there is no rlimits-only
//! fallback, because rlimits are a weaker bound rather than an equivalent one
//! (`RLIMIT_NPROC` is per-user, `RLIMIT_AS` is per-process) and a flag that
//! weakens a stated boundary tends to end up set (D22).

use std::path::PathBuf;

use clyde_core::classification::{ResourceLimits, TrustClass};

use crate::error::{Result, SandboxError};

/// Host programs used to apply limits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LimitTools {
    /// `systemd-run`, for cgroup v2 scopes via user delegation.
    pub systemd_run: Option<PathBuf>,
    /// `prlimit` from util-linux, for rlimits.
    pub prlimit: Option<PathBuf>,
    /// Whether cgroup delegation is actually available on this host.
    pub cgroup_delegation: bool,
}

/// Wraps `inner` with the limit-applying prefix.
///
/// Returns the complete argv to execute. For T2 and above the cgroup scope is
/// required, so a host without delegation produces an error rather than a
/// weaker command.
pub fn wrap_with_limits(
    id: &str,
    trust_class: TrustClass,
    limits: &ResourceLimits,
    tools: &LimitTools,
    inner: Vec<String>,
) -> Result<Vec<String>> {
    let mut argv = Vec::new();

    if trust_class.requires_cgroup_limits() {
        if !tools.cgroup_delegation {
            return Err(SandboxError::CgroupDelegationUnavailable {
                detail: "cgroup v2 delegation is not available on this host".to_owned(),
            });
        }
        let Some(systemd_run) = tools.systemd_run.as_ref() else {
            return Err(SandboxError::CgroupDelegationUnavailable {
                detail: "systemd-run was not found, so no delegated scope can be created"
                    .to_owned(),
            });
        };
        argv.push(systemd_run.to_string_lossy().to_string());
        argv.extend(scope_arguments(id, limits));
        argv.push("--".to_owned());
    }

    if let Some(prlimit) = tools.prlimit.as_ref() {
        argv.push(prlimit.to_string_lossy().to_string());
        argv.extend(rlimit_arguments(limits));
        argv.push("--".to_owned());
    } else if !trust_class.requires_cgroup_limits() {
        // The workspace environment may run on rlimits and a timeout, but if
        // even `prlimit` is missing the daemon's wall-clock timeout is the only
        // bound left. That is a genuine weakening and must be visible.
        tracing::warn!(
            sandbox = id,
            "prlimit is unavailable; the workspace environment runs with only a wall-clock bound"
        );
    } else {
        return Err(SandboxError::ProgramNotFound {
            program: PathBuf::from("prlimit"),
        });
    }

    argv.extend(inner);
    Ok(argv)
}

/// `systemd-run --user --scope` arguments for a cgroup v2 scope.
fn scope_arguments(id: &str, limits: &ResourceLimits) -> Vec<String> {
    vec![
        "--user".to_owned(),
        "--scope".to_owned(),
        "--quiet".to_owned(),
        "--collect".to_owned(),
        format!("--unit=clyde-{id}"),
        "--property".to_owned(),
        format!("MemoryMax={}", limits.max_memory_bytes),
        "--property".to_owned(),
        format!("MemorySwapMax=0"),
        "--property".to_owned(),
        format!("CPUQuota={}%", limits.max_cpu_percent),
        "--property".to_owned(),
        format!("TasksMax={}", limits.max_tasks),
    ]
}

/// `prlimit` arguments.
///
/// `--nproc` is deliberately omitted: `RLIMIT_NPROC` is per-user, so setting it
/// here would bound the developer's whole login session rather than the sandbox,
/// and would not bound the sandbox at all if other processes were already
/// running. Process count is bounded by `TasksMax` in the cgroup instead.
fn rlimit_arguments(limits: &ResourceLimits) -> Vec<String> {
    vec![
        format!(
            "--nofile={}:{}",
            limits.max_open_files, limits.max_open_files
        ),
        format!("--core=0:0"),
        format!(
            "--cpu={}:{}",
            limits.max_wall_clock.as_secs(),
            limits.max_wall_clock.as_secs()
        ),
    ]
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
            max_wall_clock: HumanDuration::parse("30m").unwrap(),
            max_memory_bytes: 8 << 30,
            max_cpu_percent: 400,
            max_tasks: 512,
            max_open_files: 4096,
        }
    }

    fn tools(delegation: bool) -> LimitTools {
        LimitTools {
            systemd_run: Some(PathBuf::from("/usr/bin/systemd-run")),
            prlimit: Some(PathBuf::from("/usr/bin/prlimit")),
            cgroup_delegation: delegation,
        }
    }

    #[test]
    fn a_build_task_gets_a_cgroup_scope_and_rlimits() {
        let argv = wrap_with_limits(
            "sb-1",
            TrustClass::T2,
            &limits(),
            &tools(true),
            vec!["/bin/bwrap".to_owned(), "true".to_owned()],
        )
        .expect("delegation available");
        let rendered = argv.join(" ");
        assert!(rendered.starts_with("/usr/bin/systemd-run --user --scope"));
        assert!(rendered.contains("--unit=clyde-sb-1"));
        assert!(rendered.contains("MemoryMax=8589934592"));
        assert!(rendered.contains("CPUQuota=400%"));
        assert!(rendered.contains("TasksMax=512"));
        assert!(rendered.contains("/usr/bin/prlimit"));
        assert!(rendered.ends_with("/bin/bwrap true"));
    }

    #[test]
    fn a_build_task_is_refused_without_delegation() {
        let error = wrap_with_limits(
            "sb-1",
            TrustClass::T2,
            &limits(),
            &tools(false),
            vec!["/bin/bwrap".to_owned()],
        )
        .expect_err("T2 requires cgroup limits");
        assert!(matches!(
            error,
            SandboxError::CgroupDelegationUnavailable { .. }
        ));
        // And there is no argument or flag that produces a weaker command.
        let error = wrap_with_limits(
            "sb-1",
            TrustClass::T3,
            &limits(),
            &LimitTools {
                systemd_run: None,
                prlimit: Some(PathBuf::from("/usr/bin/prlimit")),
                cgroup_delegation: true,
            },
            vec!["/bin/bwrap".to_owned()],
        )
        .expect_err("no systemd-run means no scope");
        assert!(matches!(
            error,
            SandboxError::CgroupDelegationUnavailable { .. }
        ));
    }

    #[test]
    fn the_workspace_environment_runs_on_rlimits_alone() {
        let argv = wrap_with_limits(
            "sb-ws",
            TrustClass::T1,
            &limits(),
            &tools(false),
            vec!["/bin/bwrap".to_owned()],
        )
        .expect("T0/T1 may run without cgroups");
        let rendered = argv.join(" ");
        assert!(!rendered.contains("systemd-run"));
        assert!(rendered.contains("prlimit"));
    }

    #[test]
    fn memory_swap_is_pinned_to_zero() {
        // Without this, MemoryMax alone lets a hostile build script push the
        // host into swap thrash rather than being killed.
        let argv = wrap_with_limits(
            "sb-1",
            TrustClass::T2,
            &limits(),
            &tools(true),
            vec!["/bin/true".to_owned()],
        )
        .unwrap();
        assert!(argv.iter().any(|arg| arg == "MemorySwapMax=0"));
    }

    #[test]
    fn nproc_is_not_set_because_it_is_per_user() {
        let argv = wrap_with_limits(
            "sb-1",
            TrustClass::T2,
            &limits(),
            &tools(true),
            vec!["/bin/true".to_owned()],
        )
        .unwrap();
        assert!(
            !argv.iter().any(|arg| arg.starts_with("--nproc")),
            "RLIMIT_NPROC is per-user and would bound the login session, not the sandbox"
        );
        assert!(argv.iter().any(|arg| arg.starts_with("--nofile=")));
    }
}
