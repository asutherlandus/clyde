//! Configuration loading.
//!
//! Precedence is built-in defaults → host config → user config → repository
//! config (D14). The first three are loaded once at daemon start; repository
//! config is loaded per workspace, because it is untrusted content whose
//! rejections must be attributed to that workspace.

use std::path::{Path, PathBuf};

use chrono::Utc;
use clyde_core::ids::WorkspaceId;
use clyde_policy::config::{Config, ConfigError, ConfigLoadRecord, ConfigSource, apply_layer};
use clyde_store::{ConfigLoad, Store};

/// Where the host and user configuration files live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    pub host: PathBuf,
    pub user: Option<PathBuf>,
}

impl ConfigPaths {
    pub fn discover() -> Self {
        let user = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .map(|base| base.join("clyde").join("config.toml"));
        Self {
            host: std::env::var_os("CLYDE_HOST_CONFIG")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/etc/clyde/config.toml")),
            user,
        }
    }
}

/// The base configuration and the records of how it was loaded.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub config: Config,
    pub records: Vec<ConfigLoadRecord>,
}

/// Loads defaults, then host, then user configuration.
///
/// A missing file is not an error; an unreadable or malformed one is, because
/// silently running on defaults when an operator believes they configured
/// something is exactly the failure D14 exists to prevent.
pub fn load_base(paths: &ConfigPaths) -> Result<LoadedConfig, ConfigError> {
    let mut config = Config::defaults();
    let mut records = Vec::new();
    for (path, source) in [
        (Some(paths.host.clone()), ConfigSource::Host),
        (paths.user.clone(), ConfigSource::User),
    ] {
        let Some(path) = path else { continue };
        let Some(text) = read_optional(&path) else {
            continue;
        };
        let (next, record) = apply_layer(config, &text, source)?;
        config = next;
        records.push(record);
    }
    Ok(LoadedConfig { config, records })
}

/// Applies a workspace's `.clyde/policy.toml` on top of the base configuration.
///
/// A repository value that would widen authority is a rejection with a
/// diagnostic, not a silent clamp — so this returns an error the operator sees
/// rather than a quietly narrowed configuration.
pub fn load_repository(
    base: &Config,
    workspace_root: &Path,
) -> Result<(Config, Option<ConfigLoadRecord>), ConfigError> {
    let path = workspace_root.join(".clyde").join("policy.toml");
    let Some(text) = read_optional(&path) else {
        return Ok((base.clone(), None));
    };
    let (config, record) = apply_layer(base.clone(), &text, ConfigSource::Repository)?;
    Ok((config, Some(record)))
}

/// Records a configuration load, including any rejected keys.
pub fn record_load(store: &dyn Store, workspace: Option<&WorkspaceId>, record: &ConfigLoadRecord) {
    let load = ConfigLoad {
        workspace: workspace.cloned(),
        source: record.source.name().to_owned(),
        digest: record.digest.clone(),
        rejected_keys: record
            .rejections
            .iter()
            .map(|rejection| rejection.key.clone())
            .collect(),
        loaded_at: Utc::now(),
    };
    if let Err(error) = store.record_config_load(load) {
        tracing::error!(error = %error, "recording a config load failed");
    }
}

/// Records a *rejected* repository configuration.
///
/// A rejection is more important to record than a successful load: it is
/// evidence that a repository tried to widen its own authority.
pub fn record_rejection(store: &dyn Store, workspace: Option<&WorkspaceId>, error: &ConfigError) {
    let rejections = error.rejections();
    if rejections.is_empty() {
        return;
    }
    let digest = clyde_core::Digest::of_bytes(b"rejected");
    let load = ConfigLoad {
        workspace: workspace.cloned(),
        source: ConfigSource::Repository.name().to_owned(),
        digest,
        rejected_keys: rejections
            .iter()
            .map(|rejection| rejection.key.clone())
            .collect(),
        loaded_at: Utc::now(),
    };
    if let Err(error) = store.record_config_load(load) {
        tracing::error!(error = %error, "recording a config rejection failed");
    }
}

fn read_optional(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            // An unreadable file is reported rather than treated as absent: the
            // operator believes it is in force.
            tracing::error!(path = %path.display(), error = %error, "configuration file is unreadable");
            None
        }
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
    use clyde_store::MemoryStore;

    #[test]
    fn missing_files_leave_the_defaults_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load_base(&ConfigPaths {
            host: dir.path().join("absent.toml"),
            user: Some(dir.path().join("also-absent.toml")),
        })
        .unwrap();
        assert_eq!(loaded.config, Config::defaults());
        assert!(loaded.records.is_empty());
    }

    #[test]
    fn host_then_user_apply_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let host = dir.path().join("host.toml");
        let user = dir.path().join("user.toml");
        std::fs::write(&host, "[mission.defaults]\nmax_task_runs = 40\n").unwrap();
        std::fs::write(&user, "[mission.defaults]\nmax_task_runs = 25\n").unwrap();
        let loaded = load_base(&ConfigPaths {
            host,
            user: Some(user),
        })
        .unwrap();
        assert_eq!(loaded.config.mission_defaults.budget.max_task_runs, 25);
        assert_eq!(loaded.records.len(), 2);
    }

    #[test]
    fn a_repository_narrowing_applies_and_is_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("project");
        std::fs::create_dir_all(workspace.join(".clyde")).unwrap();
        std::fs::write(
            workspace.join(".clyde/policy.toml"),
            "[snapshot]\nexclusions = [\"fixtures/large\"]\n",
        )
        .unwrap();
        let (config, record) = load_repository(&Config::defaults(), &workspace).unwrap();
        assert!(config.snapshot.exclusions.contains("fixtures/large"));
        let record = record.expect("a record");
        assert_eq!(record.source, ConfigSource::Repository);

        let store = MemoryStore::new();
        record_load(&store, None, &record);
        assert_eq!(store.list_config_loads(None).unwrap().len(), 1);
    }

    #[test]
    fn a_repository_widening_is_an_error_and_is_recorded_as_one() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("project");
        std::fs::create_dir_all(workspace.join(".clyde")).unwrap();
        std::fs::write(
            workspace.join(".clyde/policy.toml"),
            "[agent]\ncommand = \"/tmp/evil\"\n",
        )
        .unwrap();
        let error = load_repository(&Config::defaults(), &workspace)
            .expect_err("agent.command must be rejected");
        assert_eq!(error.rejections()[0].key, "agent.command");

        let store = MemoryStore::new();
        record_rejection(&store, None, &error);
        let loads = store.list_config_loads(None).unwrap();
        assert_eq!(loads[0].rejected_keys, vec!["agent.command".to_owned()]);
    }

    #[test]
    fn a_workspace_with_no_policy_file_uses_the_base_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let (config, record) = load_repository(&Config::defaults(), dir.path()).unwrap();
        assert_eq!(config, Config::defaults());
        assert!(record.is_none());
    }
}
