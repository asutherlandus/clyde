//! SQLite-backed store.

mod migrations;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use clyde_core::Digest;
use clyde_core::actor::Actor;
use clyde_core::approval::{ApprovalDecision, ApprovalRequest, Decision};
use clyde_core::artifact::Artifact;
use clyde_core::audit::{AuditChainHead, AuditEvent, AuditEventDraft, GENESIS_HASH, verify_chain};
use clyde_core::baseline::{AccessBaseline, BaselineKey, BaselinePathEntry, BaselineProposal};
use clyde_core::broker::{BrokerOpState, BrokeredOperation};
use clyde_core::budget::{BudgetCost, BudgetUsage};
use clyde_core::decision::PolicyDecision;
use clyde_core::egress::EgressAttempt;
use clyde_core::ids::{
    ActorId, ApprovalId, ArtifactId, BrokerOpId, LeaseId, MissionId, SnapshotId, TaskRunId,
    WorkspaceId,
};
use clyde_core::lease::{Lease, LeaseState};
use clyde_core::mission::{Mission, MissionState};
use clyde_core::repo_path::RepoPath;
use clyde_core::session::{ActorSession, TokenHash};
use clyde_core::snapshot::Snapshot;
use clyde_core::task::{TaskOutcome, TaskRun, TaskRunState, TaskType};
use clyde_core::workspace::Workspace;

use crate::error::{Result, StoreError};
use crate::store::Store;
use crate::types::{
    ApprovalRecord, AuditFilter, ConfigLoad, LeaseRenewal, MissionCloseout, ResolvedSession,
};
use crate::types_bundle::BundleRecord;

/// A store backed by a SQLite database.
///
/// The connection is behind a mutex rather than pooled: Clyde's write path is a
/// single daemon performing short transactions, and serialising them removes
/// `SQLITE_BUSY` handling from every call site.
#[derive(Debug)]
pub struct SqliteStore {
    connection: Mutex<Connection>,
    path: PathBuf,
}

impl SqliteStore {
    /// Opens or creates the database and applies pending migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| StoreError::Backend(format!("creating {parent:?}: {error}")))?;
        }
        let connection = Connection::open(&path).map_err(backend)?;
        Self::configure(&connection)?;
        let store = Self {
            connection: Mutex::new(connection),
            path,
        };
        store.migrate()?;
        Ok(store)
    }

    /// Opens a private in-memory database, for tests that want SQLite semantics
    /// without a file.
    pub fn open_in_memory() -> Result<Self> {
        let connection = Connection::open_in_memory().map_err(backend)?;
        Self::configure(&connection)?;
        let store = Self {
            connection: Mutex::new(connection),
            path: PathBuf::from(":memory:"),
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn configure(connection: &Connection) -> Result<()> {
        // WAL for concurrent readers; FULL synchronous because the audit and
        // approval tables must survive a power loss, and foreign keys on so the
        // schema's referential invariants are enforced rather than assumed.
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=FULL;
                 PRAGMA foreign_keys=ON;
                 PRAGMA busy_timeout=5000;",
            )
            .map_err(backend)
    }

    fn conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| StoreError::Unavailable("sqlite connection lock is poisoned"))
    }

    fn migrate(&self) -> Result<()> {
        let mut guard = self.conn()?;
        let tx = guard.transaction().map_err(backend)?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                 version INTEGER PRIMARY KEY,
                 name    TEXT NOT NULL,
                 applied_at TEXT NOT NULL
             ) STRICT;",
        )
        .map_err(backend)?;
        let applied: u32 = tx
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |row| row.get(0),
            )
            .map_err(backend)?;
        for migration in migrations::MIGRATIONS {
            if migration.version <= applied {
                continue;
            }
            tx.execute_batch(migration.sql).map_err(|error| {
                StoreError::Backend(format!(
                    "migration {} ({}) failed: {error}",
                    migration.version, migration.name
                ))
            })?;
            tx.execute(
                "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                params![migration.version, migration.name, Utc::now().to_rfc3339()],
            )
            .map_err(backend)?;
        }
        tx.commit().map_err(backend)
    }

    /// The applied schema version.
    pub fn schema_version(&self) -> Result<u32> {
        self.conn()?
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |row| row.get(0),
            )
            .map_err(backend)
    }

    /// Test-only: removes audit rows through direct SQL, bypassing the
    /// append-only API, so truncation detection can be exercised.
    #[cfg(any(test, feature = "test-support"))]
    pub fn delete_audit_rows_for_test(&self, keep: u64) -> Result<()> {
        self.conn()?
            .execute("DELETE FROM audit_events WHERE seq > ?1", params![keep])
            .map(|_| ())
            .map_err(backend)
    }
}

fn backend(error: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(error.to_string())
}

fn encode<T: Serialize>(context: &'static str, value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|error| StoreError::encode(context, error))
}

fn decode<T: DeserializeOwned>(context: &'static str, text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|error| StoreError::decode(context, error))
}

fn optional_time(value: Option<String>) -> Option<DateTime<Utc>> {
    value
        .and_then(|text| DateTime::parse_from_rfc3339(&text).ok())
        .map(|time| time.with_timezone(&Utc))
}

/// Loads a single payload column into `T`.
///
/// Generic over the connection holder so a `MutexGuard` can be passed directly:
/// deref coercion does not reach through the expectation `?` propagates.
fn one<T: DeserializeOwned, C: std::ops::Deref<Target = Connection>>(
    connection: &C,
    context: &'static str,
    sql: &str,
    key: &str,
) -> Result<Option<T>> {
    let text: Option<String> = connection
        .query_row(sql, params![key], |row| row.get(0))
        .optional()
        .map_err(backend)?;
    match text {
        Some(text) => Ok(Some(decode(context, &text)?)),
        None => Ok(None),
    }
}

/// Loads every payload column a query returns.
fn many<T: DeserializeOwned, C: std::ops::Deref<Target = Connection>>(
    connection: &C,
    context: &'static str,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Vec<T>> {
    let mut statement = connection.prepare(sql).map_err(backend)?;
    let rows = statement
        .query_map(params, |row| row.get::<_, String>(0))
        .map_err(backend)?;
    let mut out = Vec::new();
    for row in rows {
        let text = row.map_err(backend)?;
        out.push(decode(context, &text)?);
    }
    Ok(out)
}

/// Writes the normalised rows that mirror a baseline's payload.
///
/// The payload is authoritative; these rows exist so an operator can query
/// baselines with SQL and so the documented table layout is real.
fn write_baseline_rows(tx: &Transaction<'_>, baseline: &AccessBaseline) -> Result<()> {
    let workspace = baseline.workspace.as_str();
    let task = baseline.task.name();
    let target = baseline.target.as_str();
    tx.execute(
        "DELETE FROM baseline_paths WHERE workspace = ?1 AND task = ?2 AND target = ?3",
        params![workspace, task, target],
    )
    .map_err(backend)?;
    tx.execute(
        "DELETE FROM code_exec_inventory WHERE workspace = ?1 AND task = ?2 AND target = ?3",
        params![workspace, task, target],
    )
    .map_err(backend)?;
    for entry in &baseline.paths {
        let (kind, reason) = match entry {
            BaselinePathEntry::SubtreeGrant { source, .. } => {
                ("subtree_grant", Some(format!("{source:?}")))
            }
            BaselinePathEntry::FilePin { reason, .. } => ("file_pin", Some(reason.clone())),
            BaselinePathEntry::SubtreePin { reason, .. } => ("subtree_pin", Some(reason.clone())),
        };
        tx.execute(
            "INSERT INTO baseline_paths (workspace, task, target, path, kind, reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![workspace, task, target, entry.path().as_str(), kind, reason],
        )
        .map_err(backend)?;
    }
    for entry in &baseline.inventory.entries {
        tx.execute(
            "INSERT INTO code_exec_inventory
             (workspace, task, target, crate_name, version, kind, source_blake3)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                workspace,
                task,
                target,
                entry.crate_name,
                entry.version,
                entry.kind.name(),
                entry.source_blake3.as_str()
            ],
        )
        .map_err(backend)?;
    }
    Ok(())
}

impl Store for SqliteStore {
    fn register_workspace(&self, workspace: Workspace) -> Result<()> {
        workspace.validate()?;
        let payload = encode("workspace", &workspace)?;
        let root = workspace.root.to_string_lossy().to_string();
        self.conn()?
            .execute(
                "INSERT INTO workspaces (id, root, payload) VALUES (?1, ?2, ?3)",
                params![workspace.id.as_str(), root, payload],
            )
            .map(|_| ())
            .map_err(|error| match error {
                rusqlite::Error::SqliteFailure(inner, _)
                    if inner.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    StoreError::AlreadyExists(workspace.id.to_string())
                }
                other => backend(other),
            })
    }

    fn get_workspace(&self, id: &WorkspaceId) -> Result<Workspace> {
        one(
            &self.conn()?,
            "workspace",
            "SELECT payload FROM workspaces WHERE id = ?1",
            id.as_str(),
        )?
        .ok_or_else(|| StoreError::UnknownWorkspace(id.clone()))
    }

    fn find_workspace_by_root(&self, root: &Path) -> Result<Option<Workspace>> {
        one(
            &self.conn()?,
            "workspace",
            "SELECT payload FROM workspaces WHERE root = ?1",
            &root.to_string_lossy(),
        )
    }

    fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        many(
            &self.conn()?,
            "workspace",
            "SELECT payload FROM workspaces ORDER BY id",
            &[],
        )
    }

    fn set_workspace_policy_digest(&self, id: &WorkspaceId, digest: Option<Digest>) -> Result<()> {
        let mut workspace = self.get_workspace(id)?;
        workspace.policy_digest = digest;
        let payload = encode("workspace", &workspace)?;
        self.conn()?
            .execute(
                "UPDATE workspaces SET payload = ?2 WHERE id = ?1",
                params![id.as_str(), payload],
            )
            .map(|_| ())
            .map_err(backend)
    }

    fn upsert_actor(&self, actor: Actor) -> Result<()> {
        actor.validate()?;
        let payload = encode("actor", &actor)?;
        self.conn()?
            .execute(
                "INSERT INTO actors (id, kind, payload) VALUES (?1, ?2, ?3)
                 ON CONFLICT(id) DO UPDATE SET payload = excluded.payload",
                params![actor.id.as_str(), format!("{:?}", actor.kind), payload],
            )
            .map(|_| ())
            .map_err(backend)
    }

    fn get_actor(&self, id: &ActorId) -> Result<Option<Actor>> {
        one(
            &self.conn()?,
            "actor",
            "SELECT payload FROM actors WHERE id = ?1",
            id.as_str(),
        )
    }

    fn create_mission(&self, mission: Mission) -> Result<()> {
        mission.validate()?;
        let payload = encode("mission", &mission)?;
        let mut guard = self.conn()?;
        let tx = guard.transaction().map_err(backend)?;
        let workspace_exists: bool = tx
            .query_row(
                "SELECT 1 FROM workspaces WHERE id = ?1",
                params![mission.workspace.as_str()],
                |_| Ok(true),
            )
            .optional()
            .map_err(backend)?
            .unwrap_or(false);
        if !workspace_exists {
            return Err(StoreError::UnknownWorkspace(mission.workspace.clone()));
        }
        // The partial unique index enforces D16, but the existing mission is
        // looked up first so the error can name it.
        let existing: Option<String> = tx
            .query_row(
                "SELECT id FROM missions WHERE workspace = ?1 AND is_terminal = 0",
                params![mission.workspace.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend)?;
        if let Some(existing) = existing {
            return Err(StoreError::MissionAlreadyActive {
                workspace: mission.workspace.clone(),
                existing: MissionId::parse(existing)?,
            });
        }
        tx.execute(
            "INSERT INTO missions (id, workspace, state, is_terminal, created_at, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                mission.id.as_str(),
                mission.workspace.as_str(),
                mission.state.to_string(),
                i64::from(mission.state.is_terminal()),
                mission.created_at.to_rfc3339(),
                payload
            ],
        )
        .map_err(|error| match error {
            rusqlite::Error::SqliteFailure(inner, _)
                if inner.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                StoreError::AlreadyExists(mission.id.to_string())
            }
            other => backend(other),
        })?;
        tx.commit().map_err(backend)
    }

    fn get_mission(&self, id: &MissionId) -> Result<Mission> {
        one(
            &self.conn()?,
            "mission",
            "SELECT payload FROM missions WHERE id = ?1",
            id.as_str(),
        )?
        .ok_or_else(|| StoreError::UnknownMission(id.clone()))
    }

    fn list_missions(&self, workspace: Option<&WorkspaceId>) -> Result<Vec<Mission>> {
        let guard = self.conn()?;
        match workspace {
            Some(id) => many(
                &guard,
                "mission",
                "SELECT payload FROM missions WHERE workspace = ?1 ORDER BY created_at",
                &[&id.as_str()],
            ),
            None => many(
                &guard,
                "mission",
                "SELECT payload FROM missions ORDER BY created_at",
                &[],
            ),
        }
    }

    fn active_mission(&self, workspace: &WorkspaceId) -> Result<Option<Mission>> {
        one(
            &self.conn()?,
            "mission",
            "SELECT payload FROM missions WHERE workspace = ?1 AND is_terminal = 0",
            workspace.as_str(),
        )
    }

    fn transition_mission(&self, id: &MissionId, to: MissionState) -> Result<Mission> {
        let mut mission = self.get_mission(id)?;
        mission.state = mission.state.transition(to)?;
        let payload = encode("mission", &mission)?;
        self.conn()?
            .execute(
                "UPDATE missions SET state = ?2, is_terminal = ?3, payload = ?4 WHERE id = ?1",
                params![
                    id.as_str(),
                    mission.state.to_string(),
                    i64::from(mission.state.is_terminal()),
                    payload
                ],
            )
            .map_err(backend)?;
        Ok(mission)
    }

    fn set_mission_expiry(
        &self,
        id: &MissionId,
        expiry: chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        let mut mission = self.get_mission(id)?;
        mission.expiry = expiry;
        let payload = encode("mission", &mission)?;
        self.conn()?
            .execute(
                "UPDATE missions SET payload = ?2 WHERE id = ?1",
                params![id.as_str(), payload],
            )
            .map(|_| ())
            .map_err(backend)
    }

    fn set_mission_cache_dir(&self, id: &MissionId, cache_dir: Option<PathBuf>) -> Result<()> {
        let mut mission = self.get_mission(id)?;
        mission.cache_dir = cache_dir;
        let payload = encode("mission", &mission)?;
        self.conn()?
            .execute(
                "UPDATE missions SET payload = ?2 WHERE id = ?1",
                params![id.as_str(), payload],
            )
            .map(|_| ())
            .map_err(backend)
    }

    fn close_mission(&self, closeout: MissionCloseout) -> Result<Mission> {
        let mut mission = self.get_mission(&closeout.mission)?;
        mission.state = mission.state.transition(closeout.final_state)?;
        mission.closed_at = Some(closeout.closed_at);
        let payload = encode("mission", &mission)?;

        let mut guard = self.conn()?;
        let tx = guard.transaction().map_err(backend)?;

        // Revoke leases.
        let leases: Vec<(String, String)> = {
            let mut statement = tx
                .prepare("SELECT id, payload FROM leases WHERE mission = ?1")
                .map_err(backend)?;
            let rows = statement
                .query_map(params![closeout.mission.as_str()], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(backend)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(backend)?
        };
        for (id, lease_payload) in leases {
            let mut lease: Lease = decode("lease", &lease_payload)?;
            if lease.state.is_terminal() {
                continue;
            }
            lease.state = LeaseState::Revoked;
            let updated = encode("lease", &lease)?;
            tx.execute(
                "UPDATE leases SET state = ?2, payload = ?3 WHERE id = ?1",
                params![id, lease.state.to_string(), updated],
            )
            .map_err(backend)?;
        }

        // Revoke sessions for those leases.
        tx.execute(
            "UPDATE actor_sessions SET revoked_at = ?2
             WHERE revoked_at IS NULL
               AND lease IN (SELECT id FROM leases WHERE mission = ?1)",
            params![closeout.mission.as_str(), closeout.closed_at.to_rfc3339()],
        )
        .map_err(backend)?;

        // Freeze in-flight brokered operations rather than cancelling silently.
        let ops: Vec<(String, String)> = {
            let mut statement = tx
                .prepare("SELECT id, payload FROM broker_ops WHERE mission = ?1")
                .map_err(backend)?;
            let rows = statement
                .query_map(params![closeout.mission.as_str()], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(backend)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(backend)?
        };
        for (id, op_payload) in ops {
            let mut op: BrokeredOperation = decode("broker op", &op_payload)?;
            if op.state.is_terminal() {
                continue;
            }
            op.state = op.state.transition(BrokerOpState::Frozen)?;
            op.finished_at = Some(closeout.closed_at);
            let updated = encode("broker op", &op)?;
            tx.execute(
                "UPDATE broker_ops SET state = ?2, payload = ?3 WHERE id = ?1",
                params![id, op.state.to_string(), updated],
            )
            .map_err(backend)?;
        }

        tx.execute(
            "UPDATE missions SET state = ?2, is_terminal = ?3, payload = ?4 WHERE id = ?1",
            params![
                closeout.mission.as_str(),
                mission.state.to_string(),
                i64::from(mission.state.is_terminal()),
                payload
            ],
        )
        .map_err(backend)?;
        tx.commit().map_err(backend)?;
        Ok(mission)
    }

    fn insert_lease(&self, lease: Lease) -> Result<()> {
        lease.validate()?;
        let payload = encode("lease", &lease)?;
        self.conn()?
            .execute(
                "INSERT INTO leases (id, mission, parent, actor, state, expires_at, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    lease.id.as_str(),
                    lease.mission.as_str(),
                    lease.parent.as_ref().map(LeaseId::as_str),
                    lease.actor.as_str(),
                    lease.state.to_string(),
                    lease.expires_at.to_rfc3339(),
                    payload
                ],
            )
            .map(|_| ())
            .map_err(|error| match error {
                rusqlite::Error::SqliteFailure(inner, _)
                    if inner.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    // Either the identifier collides or the mission is unknown;
                    // the mission case is the one worth naming.
                    StoreError::UnknownMission(lease.mission.clone())
                }
                other => backend(other),
            })
    }

    fn get_lease(&self, id: &LeaseId) -> Result<Lease> {
        one(
            &self.conn()?,
            "lease",
            "SELECT payload FROM leases WHERE id = ?1",
            id.as_str(),
        )?
        .ok_or_else(|| StoreError::UnknownLease(id.clone()))
    }

    fn list_leases(&self, mission: &MissionId) -> Result<Vec<Lease>> {
        many(
            &self.conn()?,
            "lease",
            "SELECT payload FROM leases WHERE mission = ?1 ORDER BY id",
            &[&mission.as_str()],
        )
    }

    fn set_lease_state(&self, id: &LeaseId, to: LeaseState) -> Result<Lease> {
        let mut lease = self.get_lease(id)?;
        lease.state = lease.state.transition(to)?;
        let payload = encode("lease", &lease)?;
        self.conn()?
            .execute(
                "UPDATE leases SET state = ?2, payload = ?3 WHERE id = ?1",
                params![id.as_str(), lease.state.to_string(), payload],
            )
            .map_err(backend)?;
        Ok(lease)
    }

    fn revoke_lease_tree(&self, id: &LeaseId, at: DateTime<Utc>) -> Result<Vec<LeaseId>> {
        let mut guard = self.conn()?;
        let tx = guard.transaction().map_err(backend)?;
        let exists: bool = tx
            .query_row(
                "SELECT 1 FROM leases WHERE id = ?1",
                params![id.as_str()],
                |_| Ok(true),
            )
            .optional()
            .map_err(backend)?
            .unwrap_or(false);
        if !exists {
            return Err(StoreError::UnknownLease(id.clone()));
        }

        // Recursive descent over the derivation tree, computed before anything
        // is written so the fan-out is applied atomically.
        let mut to_revoke = vec![id.clone()];
        let mut frontier = vec![id.clone()];
        while let Some(current) = frontier.pop() {
            let children: Vec<String> = {
                let mut statement = tx
                    .prepare("SELECT id FROM leases WHERE parent = ?1")
                    .map_err(backend)?;
                let rows = statement
                    .query_map(params![current.as_str()], |row| row.get::<_, String>(0))
                    .map_err(backend)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(backend)?
            };
            for child in children {
                let child = LeaseId::parse(child)?;
                if !to_revoke.contains(&child) {
                    to_revoke.push(child.clone());
                    frontier.push(child);
                }
            }
        }

        for lease_id in &to_revoke {
            let payload: Option<String> = tx
                .query_row(
                    "SELECT payload FROM leases WHERE id = ?1",
                    params![lease_id.as_str()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(backend)?;
            let Some(payload) = payload else { continue };
            let mut lease: Lease = decode("lease", &payload)?;
            if !lease.state.is_terminal() {
                lease.state = LeaseState::Revoked;
                let updated = encode("lease", &lease)?;
                tx.execute(
                    "UPDATE leases SET state = ?2, payload = ?3 WHERE id = ?1",
                    params![lease_id.as_str(), lease.state.to_string(), updated],
                )
                .map_err(backend)?;
            }
            tx.execute(
                "UPDATE actor_sessions SET revoked_at = ?2 WHERE lease = ?1 AND revoked_at IS NULL",
                params![lease_id.as_str(), at.to_rfc3339()],
            )
            .map_err(backend)?;
        }
        tx.commit().map_err(backend)?;
        Ok(to_revoke)
    }

    fn charge_lease(&self, id: &LeaseId, cost: &BudgetCost) -> Result<BudgetUsage> {
        let mut guard = self.conn()?;
        let tx = guard.transaction().map_err(backend)?;
        let payload: Option<String> = tx
            .query_row(
                "SELECT payload FROM leases WHERE id = ?1",
                params![id.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend)?;
        let Some(payload) = payload else {
            return Err(StoreError::UnknownLease(id.clone()));
        };
        let mut lease: Lease = decode("lease", &payload)?;
        let next = clyde_policy::charge_budget(&lease.budget, &lease.usage, cost)
            .map_err(|reason| StoreError::Backend(reason.render()))?;
        lease.usage = next;
        if clyde_policy::budget::exhausted_dimension(&lease.budget, &lease.usage).is_some()
            && lease.state == LeaseState::Active
        {
            lease.state = LeaseState::Exhausted;
        }
        let updated = encode("lease", &lease)?;
        tx.execute(
            "UPDATE leases SET state = ?2, payload = ?3 WHERE id = ?1",
            params![id.as_str(), lease.state.to_string(), updated],
        )
        .map_err(backend)?;
        tx.commit().map_err(backend)?;
        Ok(next)
    }

    fn release_parallel_slot(&self, id: &LeaseId) -> Result<BudgetUsage> {
        let mut lease = self.get_lease(id)?;
        lease.usage = lease.usage.release_parallel_subagent();
        let payload = encode("lease", &lease)?;
        self.conn()?
            .execute(
                "UPDATE leases SET payload = ?2 WHERE id = ?1",
                params![id.as_str(), payload],
            )
            .map_err(backend)?;
        Ok(lease.usage)
    }

    fn renew_lease(&self, renewal: LeaseRenewal) -> Result<Lease> {
        renewal.replacement.validate()?;
        let replacement_payload = encode("lease", &renewal.replacement)?;
        let mut guard = self.conn()?;
        let tx = guard.transaction().map_err(backend)?;
        let payload: Option<String> = tx
            .query_row(
                "SELECT payload FROM leases WHERE id = ?1",
                params![renewal.superseded.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend)?;
        let Some(payload) = payload else {
            return Err(StoreError::UnknownLease(renewal.superseded.clone()));
        };
        let mut old: Lease = decode("lease", &payload)?;
        old.state = old.state.transition(LeaseState::Superseded)?;
        let old_payload = encode("lease", &old)?;
        tx.execute(
            "UPDATE leases SET state = ?2, payload = ?3 WHERE id = ?1",
            params![
                renewal.superseded.as_str(),
                old.state.to_string(),
                old_payload
            ],
        )
        .map_err(backend)?;
        tx.execute(
            "INSERT INTO leases (id, mission, parent, actor, state, expires_at, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                renewal.replacement.id.as_str(),
                renewal.replacement.mission.as_str(),
                renewal.replacement.parent.as_ref().map(LeaseId::as_str),
                renewal.replacement.actor.as_str(),
                renewal.replacement.state.to_string(),
                renewal.replacement.expires_at.to_rfc3339(),
                replacement_payload
            ],
        )
        .map_err(|error| match error {
            rusqlite::Error::SqliteFailure(inner, _)
                if inner.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                StoreError::AlreadyExists(renewal.replacement.id.to_string())
            }
            other => backend(other),
        })?;
        tx.commit().map_err(backend)?;
        Ok(renewal.replacement)
    }

    fn bind_session(&self, session: ActorSession) -> Result<()> {
        let lease = self.get_lease(&session.lease)?;
        session.validate(lease.expires_at)?;
        let payload = encode("session", &session)?;
        self.conn()?
            .execute(
                "INSERT INTO actor_sessions
                 (token_hash, lease, actor, expires_at, revoked_at, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    session.token_hash.to_hex(),
                    session.lease.as_str(),
                    session.actor.as_str(),
                    session.expires_at.to_rfc3339(),
                    session.revoked_at.map(|at| at.to_rfc3339()),
                    payload
                ],
            )
            .map(|_| ())
            .map_err(backend)
    }

    fn resolve_token(
        &self,
        hash: &TokenHash,
        now: DateTime<Utc>,
    ) -> Result<Option<ResolvedSession>> {
        let guard = self.conn()?;
        // A single lookup by hash; unknown, expired and revoked all fall through
        // to `None` so the caller cannot distinguish them.
        let session: Option<ActorSession> = one(
            &guard,
            "session",
            "SELECT payload FROM actor_sessions WHERE token_hash = ?1",
            &hash.to_hex(),
        )?;
        let Some(session) = session else {
            return Ok(None);
        };
        // The stored payload is the authority on revocation, but the column is
        // what the revocation fan-out updates, so both are consulted.
        let revoked_at: Option<String> = guard
            .query_row(
                "SELECT revoked_at FROM actor_sessions WHERE token_hash = ?1",
                params![hash.to_hex()],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend)?
            .flatten();
        let session = ActorSession {
            revoked_at: optional_time(revoked_at).or(session.revoked_at),
            ..session
        };
        if !session.is_valid_at(now) {
            return Ok(None);
        }
        let Some(lease) = one::<Lease, _>(
            &guard,
            "lease",
            "SELECT payload FROM leases WHERE id = ?1",
            session.lease.as_str(),
        )?
        else {
            return Ok(None);
        };
        let Some(mission) = one::<Mission, _>(
            &guard,
            "mission",
            "SELECT payload FROM missions WHERE id = ?1",
            lease.mission.as_str(),
        )?
        else {
            return Ok(None);
        };
        Ok(Some(ResolvedSession {
            session,
            lease,
            mission,
        }))
    }

    fn list_sessions(&self, mission: &MissionId) -> Result<Vec<ActorSession>> {
        many(
            &self.conn()?,
            "session",
            "SELECT payload FROM actor_sessions
             WHERE lease IN (SELECT id FROM leases WHERE mission = ?1)",
            &[&mission.as_str()],
        )
    }

    fn revoke_sessions_for_lease(&self, lease: &LeaseId, at: DateTime<Utc>) -> Result<usize> {
        self.conn()?
            .execute(
                "UPDATE actor_sessions SET revoked_at = ?2 WHERE lease = ?1 AND revoked_at IS NULL",
                params![lease.as_str(), at.to_rfc3339()],
            )
            .map_err(backend)
    }

    fn set_session_sandbox(&self, lease: &LeaseId, sandbox: Option<String>) -> Result<()> {
        let mut guard = self.conn()?;
        let tx = guard.transaction().map_err(backend)?;
        let rows: Vec<(String, String)> = {
            let mut statement = tx
                .prepare("SELECT token_hash, payload FROM actor_sessions WHERE lease = ?1")
                .map_err(backend)?;
            let rows = statement
                .query_map(params![lease.as_str()], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(backend)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(backend)?
        };
        for (token_hash, payload) in rows {
            let mut session: ActorSession = decode("session", &payload)?;
            session.sandbox = sandbox.clone();
            let updated = encode("session", &session)?;
            tx.execute(
                "UPDATE actor_sessions SET payload = ?2 WHERE token_hash = ?1",
                params![token_hash, updated],
            )
            .map_err(backend)?;
        }
        tx.commit().map_err(backend)
    }

    fn insert_task_run(&self, run: TaskRun) -> Result<()> {
        run.request.validate()?;
        let lease = self.get_lease(&run.request.lease)?;
        let request_payload = encode("task request", &run.request)?;
        let run_payload = encode("task run", &run)?;
        let mut guard = self.conn()?;
        let tx = guard.transaction().map_err(backend)?;
        tx.execute(
            "INSERT INTO task_requests (id, lease, actor, task, path, requested_at, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                run.request.id.as_str(),
                run.request.lease.as_str(),
                run.request.actor.as_str(),
                run.request.task.name(),
                run.request.path.as_str(),
                run.request.requested_at.to_rfc3339(),
                request_payload
            ],
        )
        .map_err(|error| match error {
            rusqlite::Error::SqliteFailure(inner, _)
                if inner.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                StoreError::AlreadyExists(run.request.id.to_string())
            }
            other => backend(other),
        })?;
        tx.execute(
            "INSERT INTO task_runs (id, lease, mission, task, state, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                run.id.as_str(),
                run.request.lease.as_str(),
                lease.mission.as_str(),
                run.request.task.name(),
                run.state.to_string(),
                run_payload
            ],
        )
        .map_err(backend)?;
        tx.commit().map_err(backend)
    }

    fn get_task_run(&self, id: &TaskRunId) -> Result<TaskRun> {
        one(
            &self.conn()?,
            "task run",
            "SELECT payload FROM task_runs WHERE id = ?1",
            id.as_str(),
        )?
        .ok_or_else(|| StoreError::UnknownTaskRun(id.clone()))
    }

    fn list_task_runs(&self, mission: &MissionId) -> Result<Vec<TaskRun>> {
        many(
            &self.conn()?,
            "task run",
            "SELECT payload FROM task_runs WHERE mission = ?1 ORDER BY id",
            &[&mission.as_str()],
        )
    }

    fn transition_task_run(&self, id: &TaskRunId, to: TaskRunState) -> Result<TaskRun> {
        let mut run = self.get_task_run(id)?;
        run.state = run.state.transition(to)?;
        if matches!(to, TaskRunState::Running) && run.started_at.is_none() {
            run.started_at = Some(Utc::now());
        }
        let payload = encode("task run", &run)?;
        self.conn()?
            .execute(
                "UPDATE task_runs SET state = ?2, payload = ?3 WHERE id = ?1",
                params![id.as_str(), run.state.to_string(), payload],
            )
            .map_err(backend)?;
        Ok(run)
    }

    fn complete_task_run(
        &self,
        id: &TaskRunId,
        to: TaskRunState,
        outcome: TaskOutcome,
        finished_at: DateTime<Utc>,
        artifacts: Vec<ArtifactId>,
    ) -> Result<TaskRun> {
        let mut run = self.get_task_run(id)?;
        run.state = run.state.transition(to)?;
        run.outcome = Some(outcome);
        run.finished_at = Some(finished_at);
        run.artifacts.extend(artifacts);
        let payload = encode("task run", &run)?;
        self.conn()?
            .execute(
                "UPDATE task_runs SET state = ?2, payload = ?3 WHERE id = ?1",
                params![id.as_str(), run.state.to_string(), payload],
            )
            .map_err(backend)?;
        Ok(run)
    }

    fn set_task_run_snapshot(&self, id: &TaskRunId, snapshot: SnapshotId) -> Result<()> {
        let mut run = self.get_task_run(id)?;
        run.snapshot = Some(snapshot);
        let payload = encode("task run", &run)?;
        self.conn()?
            .execute(
                "UPDATE task_runs SET payload = ?2 WHERE id = ?1",
                params![id.as_str(), payload],
            )
            .map(|_| ())
            .map_err(backend)
    }

    fn set_task_run_bundle(&self, id: &TaskRunId, bundle: ArtifactId) -> Result<()> {
        let mut run = self.get_task_run(id)?;
        run.dependency_bundle = Some(bundle);
        let payload = encode("task run", &run)?;
        self.conn()?
            .execute(
                "UPDATE task_runs SET payload = ?2 WHERE id = ?1",
                params![id.as_str(), payload],
            )
            .map(|_| ())
            .map_err(backend)
    }

    fn set_task_run_backend(
        &self,
        id: &TaskRunId,
        kind: clyde_core::classification::BackendKind,
    ) -> Result<()> {
        let mut run = self.get_task_run(id)?;
        run.backend = kind;
        let payload = encode("task run", &run)?;
        self.conn()?
            .execute(
                "UPDATE task_runs SET payload = ?2 WHERE id = ?1",
                params![id.as_str(), payload],
            )
            .map(|_| ())
            .map_err(backend)
    }

    fn record_policy_decision(&self, decision: PolicyDecision) -> Result<()> {
        let payload = encode("policy decision", &decision)?;
        self.conn()?
            .execute(
                "INSERT INTO policy_decisions (id, outcome, decided_at, payload)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    decision.id.as_str(),
                    decision.outcome.name(),
                    decision.decided_at.to_rfc3339(),
                    payload
                ],
            )
            .map(|_| ())
            .map_err(backend)
    }

    fn list_policy_decisions(&self, _mission: &MissionId) -> Result<Vec<PolicyDecision>> {
        many(
            &self.conn()?,
            "policy decision",
            "SELECT payload FROM policy_decisions ORDER BY decided_at",
            &[],
        )
    }

    fn insert_snapshot(&self, snapshot: Snapshot) -> Result<()> {
        snapshot.manifest.validate()?;
        let payload = encode("snapshot", &snapshot)?;
        let mut guard = self.conn()?;
        let tx = guard.transaction().map_err(backend)?;
        tx.execute(
            "INSERT INTO snapshots (id, workspace, mission, payload) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET payload = excluded.payload",
            params![
                snapshot.id.as_str(),
                snapshot.workspace.as_str(),
                snapshot.mission.as_str(),
                payload
            ],
        )
        .map_err(backend)?;
        for entry in &snapshot.manifest.entries {
            tx.execute(
                "INSERT INTO snapshot_entries (snapshot, path, mode, size, blake3)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(snapshot, path) DO NOTHING",
                params![
                    snapshot.id.as_str(),
                    entry.path.as_str(),
                    entry.mode,
                    entry.size,
                    entry.blake3.as_str()
                ],
            )
            .map_err(backend)?;
        }
        tx.commit().map_err(backend)
    }

    fn get_snapshot(&self, id: &SnapshotId) -> Result<Snapshot> {
        one(
            &self.conn()?,
            "snapshot",
            "SELECT payload FROM snapshots WHERE id = ?1",
            id.as_str(),
        )?
        .ok_or_else(|| StoreError::UnknownSnapshot(id.clone()))
    }

    fn latest_snapshot(&self, mission: &MissionId, target: &RepoPath) -> Result<Option<Snapshot>> {
        // The target and timestamp live in the payload rather than in columns.
        // This runs once per build, against one mission's rows, so a JSON scan is
        // cheaper than the migration that would avoid it.
        let guard = self.conn()?;
        let mut statement = guard
            .prepare(
                "SELECT payload FROM snapshots
                  WHERE mission = ?1
                    AND json_extract(payload, '$.requested_path') = ?2
                  ORDER BY json_extract(payload, '$.created_at') DESC
                  LIMIT 1",
            )
            .map_err(backend)?;
        let mut rows = statement
            .query(params![mission.as_str(), target.as_str()])
            .map_err(backend)?;
        match rows.next().map_err(backend)? {
            None => Ok(None),
            Some(row) => {
                let payload: String = row.get(0).map_err(backend)?;
                Ok(Some(decode("snapshot", &payload)?))
            }
        }
    }

    fn snapshot_contains(&self, id: &SnapshotId, path: &RepoPath) -> Result<bool> {
        let guard = self.conn()?;
        let exists: bool = guard
            .query_row(
                "SELECT 1 FROM snapshots WHERE id = ?1",
                params![id.as_str()],
                |_| Ok(true),
            )
            .optional()
            .map_err(backend)?
            .unwrap_or(false);
        if !exists {
            return Err(StoreError::UnknownSnapshot(id.clone()));
        }
        Ok(guard
            .query_row(
                "SELECT 1 FROM snapshot_entries WHERE snapshot = ?1 AND path = ?2",
                params![id.as_str(), path.as_str()],
                |_| Ok(true),
            )
            .optional()
            .map_err(backend)?
            .unwrap_or(false))
    }

    fn insert_artifact(&self, artifact: Artifact) -> Result<()> {
        let payload = encode("artifact", &artifact)?;
        self.conn()?
            .execute(
                "INSERT INTO artifacts (id, mission, kind, produced_by, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET payload = excluded.payload",
                params![
                    artifact.id.as_str(),
                    artifact.mission.as_str(),
                    format!("{:?}", artifact.kind),
                    artifact.produced_by.as_ref().map(TaskRunId::as_str),
                    payload
                ],
            )
            .map(|_| ())
            .map_err(backend)
    }

    fn get_artifact(&self, id: &ArtifactId) -> Result<Artifact> {
        one(
            &self.conn()?,
            "artifact",
            "SELECT payload FROM artifacts WHERE id = ?1",
            id.as_str(),
        )?
        .ok_or_else(|| StoreError::UnknownArtifact(id.clone()))
    }

    fn list_artifacts(&self, mission: &MissionId) -> Result<Vec<Artifact>> {
        many(
            &self.conn()?,
            "artifact",
            "SELECT payload FROM artifacts WHERE mission = ?1 ORDER BY id",
            &[&mission.as_str()],
        )
    }

    fn insert_approval_request(&self, request: ApprovalRequest) -> Result<()> {
        request.validate()?;
        let payload = encode("approval request", &request)?;
        self.conn()?
            .execute(
                "INSERT INTO approval_requests
                 (id, mission, lease, request_digest, expires_at, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    request.id.as_str(),
                    request.mission.as_str(),
                    request.lease.as_str(),
                    request.request_digest.as_str(),
                    request.expires_at.to_rfc3339(),
                    payload
                ],
            )
            .map(|_| ())
            .map_err(|error| match error {
                rusqlite::Error::SqliteFailure(inner, _)
                    if inner.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    StoreError::AlreadyExists(request.id.to_string())
                }
                other => backend(other),
            })
    }

    fn get_approval(&self, id: &ApprovalId) -> Result<ApprovalRecord> {
        let guard = self.conn()?;
        let request: Option<ApprovalRequest> = one(
            &guard,
            "approval request",
            "SELECT payload FROM approval_requests WHERE id = ?1",
            id.as_str(),
        )?;
        let request = request.ok_or_else(|| StoreError::UnknownApproval(id.clone()))?;
        let decision: Option<ApprovalDecision> = one(
            &guard,
            "approval decision",
            "SELECT payload FROM approval_decisions WHERE request = ?1",
            id.as_str(),
        )?;
        Ok(ApprovalRecord { request, decision })
    }

    fn list_pending_approvals(&self, now: DateTime<Utc>) -> Result<Vec<ApprovalRequest>> {
        let requests: Vec<ApprovalRequest> = many(
            &self.conn()?,
            "approval request",
            "SELECT payload FROM approval_requests
             WHERE id NOT IN (SELECT request FROM approval_decisions)
             ORDER BY expires_at",
            &[],
        )?;
        Ok(requests
            .into_iter()
            .filter(|request| !request.is_expired_at(now))
            .collect())
    }

    fn list_approvals(&self, mission: &MissionId) -> Result<Vec<ApprovalRecord>> {
        let guard = self.conn()?;
        let requests: Vec<ApprovalRequest> = many(
            &guard,
            "approval request",
            "SELECT payload FROM approval_requests WHERE mission = ?1 ORDER BY created_at",
            &[&mission.as_str()],
        )
        .or_else(|_| {
            many(
                &guard,
                "approval request",
                "SELECT payload FROM approval_requests WHERE mission = ?1",
                &[&mission.as_str()],
            )
        })?;
        let mut records = Vec::with_capacity(requests.len());
        for request in requests {
            let decision: Option<ApprovalDecision> = one(
                &guard,
                "approval decision",
                "SELECT payload FROM approval_decisions WHERE request = ?1",
                request.id.as_str(),
            )?;
            records.push(ApprovalRecord { request, decision });
        }
        Ok(records)
    }

    fn record_approval_decision(&self, decision: ApprovalDecision) -> Result<()> {
        decision.validate()?;
        let payload = encode("approval decision", &decision)?;
        self.conn()?
            .execute(
                "INSERT INTO approval_decisions
                 (request, decision, decided_by, consumed_at, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(request) DO UPDATE SET
                   decision = excluded.decision,
                   decided_by = excluded.decided_by,
                   payload = excluded.payload",
                params![
                    decision.request.as_str(),
                    decision.decision.name(),
                    decision.decided_by.as_str(),
                    decision.consumed_at.map(|at| at.to_rfc3339()),
                    payload
                ],
            )
            .map(|_| ())
            .map_err(|error| match error {
                rusqlite::Error::SqliteFailure(inner, _)
                    if inner.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    StoreError::UnknownApproval(decision.request.clone())
                }
                other => backend(other),
            })
    }

    fn find_authorising_approval(
        &self,
        mission: &MissionId,
        digest: &Digest,
        now: DateTime<Utc>,
    ) -> Result<Option<ApprovalRecord>> {
        let guard = self.conn()?;
        let ids: Vec<String> = {
            let mut statement = guard
                .prepare(
                    "SELECT id FROM approval_requests WHERE mission = ?1 AND request_digest = ?2",
                )
                .map_err(backend)?;
            let rows = statement
                .query_map(params![mission.as_str(), digest.as_str()], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(backend)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(backend)?
        };
        for id in ids {
            let request: Option<ApprovalRequest> = one(
                &guard,
                "approval request",
                "SELECT payload FROM approval_requests WHERE id = ?1",
                &id,
            )?;
            let Some(request) = request else { continue };
            let decision: Option<ApprovalDecision> = one(
                &guard,
                "approval decision",
                "SELECT payload FROM approval_decisions WHERE request = ?1",
                &id,
            )?;
            let record = ApprovalRecord { request, decision };
            if record.authorises(digest, now) {
                return Ok(Some(record));
            }
        }
        Ok(None)
    }

    fn consume_approval(&self, id: &ApprovalId, at: DateTime<Utc>) -> Result<()> {
        let mut guard = self.conn()?;
        let tx = guard.transaction().map_err(backend)?;
        let payload: Option<String> = tx
            .query_row(
                "SELECT payload FROM approval_decisions WHERE request = ?1",
                params![id.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend)?;
        let Some(payload) = payload else {
            return Err(StoreError::ApprovalUndecided(id.clone()));
        };
        let mut decision: ApprovalDecision = decode("approval decision", &payload)?;
        match decision.decision {
            Decision::ApproveOnce => {
                if decision.consumed_at.is_some() {
                    return Err(StoreError::ApprovalAlreadyConsumed(id.clone()));
                }
                decision.consumed_at = Some(at);
                let updated = encode("approval decision", &decision)?;
                tx.execute(
                    "UPDATE approval_decisions SET consumed_at = ?2, payload = ?3
                     WHERE request = ?1 AND consumed_at IS NULL",
                    params![id.as_str(), at.to_rfc3339(), updated],
                )
                .map_err(backend)?;
                tx.commit().map_err(backend)
            }
            Decision::ApproveForMission => Ok(()),
            Decision::Deny => Err(StoreError::ApprovalUndecided(id.clone())),
        }
    }

    fn insert_broker_op(&self, op: BrokeredOperation) -> Result<()> {
        op.kind.validate()?;
        let payload = encode("broker op", &op)?;
        self.conn()?
            .execute(
                "INSERT INTO broker_ops (id, mission, lease, approval, state, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    op.id.as_str(),
                    op.mission.as_str(),
                    op.lease.as_str(),
                    op.approval.as_str(),
                    op.state.to_string(),
                    payload
                ],
            )
            .map(|_| ())
            .map_err(|error| match error {
                rusqlite::Error::SqliteFailure(inner, _)
                    if inner.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    StoreError::AlreadyExists(op.id.to_string())
                }
                other => backend(other),
            })
    }

    fn get_broker_op(&self, id: &BrokerOpId) -> Result<BrokeredOperation> {
        one(
            &self.conn()?,
            "broker op",
            "SELECT payload FROM broker_ops WHERE id = ?1",
            id.as_str(),
        )?
        .ok_or_else(|| StoreError::UnknownBrokerOp(id.clone()))
    }

    fn list_broker_ops(&self, mission: &MissionId) -> Result<Vec<BrokeredOperation>> {
        many(
            &self.conn()?,
            "broker op",
            "SELECT payload FROM broker_ops WHERE mission = ?1 ORDER BY id",
            &[&mission.as_str()],
        )
    }

    fn transition_broker_op(
        &self,
        id: &BrokerOpId,
        to: BrokerOpState,
        summary: Option<String>,
        at: DateTime<Utc>,
    ) -> Result<BrokeredOperation> {
        let mut op = self.get_broker_op(id)?;
        op.state = op.state.transition(to)?;
        if let Some(summary) = summary {
            op.result_summary = Some(summary);
        }
        if op.state.is_terminal() {
            op.finished_at = Some(at);
        }
        let payload = encode("broker op", &op)?;
        self.conn()?
            .execute(
                "UPDATE broker_ops SET state = ?2, payload = ?3 WHERE id = ?1",
                params![id.as_str(), op.state.to_string(), payload],
            )
            .map_err(backend)?;
        Ok(op)
    }

    fn freeze_broker_ops(&self, mission: &MissionId, at: DateTime<Utc>) -> Result<Vec<BrokerOpId>> {
        let ops = self.list_broker_ops(mission)?;
        let mut frozen = Vec::new();
        for op in ops {
            if op.state.is_terminal() {
                continue;
            }
            self.transition_broker_op(&op.id, BrokerOpState::Frozen, None, at)?;
            frozen.push(op.id);
        }
        Ok(frozen)
    }

    fn insert_egress_attempt(&self, attempt: EgressAttempt) -> Result<()> {
        let payload = encode("egress attempt", &attempt)?;
        self.conn()?
            .execute(
                "INSERT INTO egress_attempts (task_run, host, decision, payload)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    attempt.task_run.as_ref().map(TaskRunId::as_str),
                    attempt.host.as_str(),
                    format!("{:?}", attempt.decision),
                    payload
                ],
            )
            .map(|_| ())
            .map_err(backend)
    }

    fn list_egress_attempts(&self, task_run: &TaskRunId) -> Result<Vec<EgressAttempt>> {
        many(
            &self.conn()?,
            "egress attempt",
            "SELECT payload FROM egress_attempts WHERE task_run = ?1 ORDER BY seq",
            &[&task_run.as_str()],
        )
    }

    fn list_mission_egress_attempts(&self, mission: &MissionId) -> Result<Vec<EgressAttempt>> {
        many(
            &self.conn()?,
            "egress attempt",
            "SELECT payload FROM egress_attempts
             WHERE task_run IN (SELECT id FROM task_runs WHERE mission = ?1)
             ORDER BY seq",
            &[&mission.as_str()],
        )
    }

    fn put_baseline(&self, baseline: AccessBaseline) -> Result<()> {
        baseline.validate()?;
        let digest = baseline
            .digest()
            .map_err(|error| StoreError::Backend(error.to_string()))?;
        let payload = encode("access baseline", &baseline)?;
        let mut guard = self.conn()?;
        let tx = guard.transaction().map_err(backend)?;
        tx.execute(
            "INSERT INTO access_baselines (workspace, task, target, origin, digest, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(workspace, task, target) DO UPDATE SET
               origin = excluded.origin,
               digest = excluded.digest,
               payload = excluded.payload",
            params![
                baseline.workspace.as_str(),
                baseline.task.name(),
                baseline.target.as_str(),
                format!("{:?}", baseline.origin),
                digest.as_str(),
                payload
            ],
        )
        .map_err(backend)?;
        write_baseline_rows(&tx, &baseline)?;
        tx.commit().map_err(backend)
    }

    fn get_baseline(&self, key: &BaselineKey) -> Result<Option<AccessBaseline>> {
        let guard = self.conn()?;
        let text: Option<String> = guard
            .query_row(
                "SELECT payload FROM access_baselines
                 WHERE workspace = ?1 AND task = ?2 AND target = ?3",
                params![key.workspace.as_str(), key.task.name(), key.target.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend)?;
        match text {
            Some(text) => Ok(Some(decode("access baseline", &text)?)),
            None => Ok(None),
        }
    }

    fn list_baselines(&self, workspace: &WorkspaceId) -> Result<Vec<AccessBaseline>> {
        many(
            &self.conn()?,
            "access baseline",
            "SELECT payload FROM access_baselines WHERE workspace = ?1 ORDER BY task, target",
            &[&workspace.as_str()],
        )
    }

    fn delete_baseline(&self, key: &BaselineKey) -> Result<bool> {
        let affected = self
            .conn()?
            .execute(
                "DELETE FROM access_baselines WHERE workspace = ?1 AND task = ?2 AND target = ?3",
                params![key.workspace.as_str(), key.task.name(), key.target.as_str()],
            )
            .map_err(backend)?;
        Ok(affected > 0)
    }

    fn put_baseline_proposal(&self, proposal: BaselineProposal) -> Result<()> {
        let payload = encode("baseline proposal", &proposal)?;
        self.conn()?
            .execute(
                "INSERT INTO baseline_proposals (workspace, task, target, origin, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(workspace, task, target) DO UPDATE SET
                   origin = excluded.origin,
                   payload = excluded.payload",
                params![
                    proposal.workspace.as_str(),
                    proposal.task.name(),
                    proposal.target.as_str(),
                    format!("{:?}", proposal.origin),
                    payload
                ],
            )
            .map(|_| ())
            .map_err(backend)
    }

    fn get_baseline_proposal(&self, key: &BaselineKey) -> Result<Option<BaselineProposal>> {
        let guard = self.conn()?;
        let text: Option<String> = guard
            .query_row(
                "SELECT payload FROM baseline_proposals
                 WHERE workspace = ?1 AND task = ?2 AND target = ?3",
                params![key.workspace.as_str(), key.task.name(), key.target.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend)?;
        match text {
            Some(text) => Ok(Some(decode("baseline proposal", &text)?)),
            None => Ok(None),
        }
    }

    fn delete_baseline_proposal(&self, key: &BaselineKey) -> Result<bool> {
        let affected = self
            .conn()?
            .execute(
                "DELETE FROM baseline_proposals WHERE workspace = ?1 AND task = ?2 AND target = ?3",
                params![key.workspace.as_str(), key.task.name(), key.target.as_str()],
            )
            .map_err(backend)?;
        Ok(affected > 0)
    }

    fn append_audit(&self, draft: AuditEventDraft) -> Result<AuditEvent> {
        let mut guard = self.conn()?;
        let tx = guard.transaction().map_err(backend)?;
        let head: Option<(u64, String)> = tx
            .query_row("SELECT seq, hash FROM audit_head WHERE id = 0", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()
            .map_err(backend)?;
        let (seq, prev_hash) = match head {
            Some((seq, hash)) => (seq.saturating_add(1), hash),
            None => (1, GENESIS_HASH.to_owned()),
        };
        let event = AuditEvent::seal(draft, seq, prev_hash)
            .map_err(|error| StoreError::Backend(error.to_string()))?;
        let payload = encode("audit payload", &event.payload)?;
        tx.execute(
            "INSERT INTO audit_events
             (seq, at, kind, workspace, mission, lease, actor, task_run, snapshot,
              approval, broker_op, payload, prev_hash, hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                event.seq,
                event.at.to_rfc3339(),
                encode("audit kind", &event.kind)?,
                event.workspace.as_ref().map(WorkspaceId::as_str),
                event.mission.as_ref().map(MissionId::as_str),
                event.lease.as_ref().map(LeaseId::as_str),
                event.actor.as_ref().map(ActorId::as_str),
                event.task_run.as_ref().map(TaskRunId::as_str),
                event.snapshot.as_ref().map(SnapshotId::as_str),
                event.approval.as_ref().map(ApprovalId::as_str),
                event.broker_op.as_ref().map(BrokerOpId::as_str),
                payload,
                event.prev_hash,
                event.hash
            ],
        )
        .map_err(backend)?;
        tx.execute(
            "INSERT INTO audit_head (id, seq, hash) VALUES (0, ?1, ?2)
             ON CONFLICT(id) DO UPDATE SET seq = excluded.seq, hash = excluded.hash",
            params![event.seq, event.hash],
        )
        .map_err(backend)?;
        // Artifact references live in the payload rather than a join table: an
        // audit event's artifact list is part of its hashed content.
        tx.commit().map_err(backend)?;
        Ok(event)
    }

    fn list_audit(&self, filter: &AuditFilter) -> Result<Vec<AuditEvent>> {
        let guard = self.conn()?;
        let mut statement = guard
            .prepare(
                "SELECT seq, at, kind, workspace, mission, lease, actor, task_run, snapshot,
                        approval, broker_op, payload, prev_hash, hash
                 FROM audit_events ORDER BY seq",
            )
            .map_err(backend)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, String>(13)?,
                ))
            })
            .map_err(backend)?;
        let mut events = Vec::new();
        for row in rows {
            let (
                seq,
                at,
                kind,
                workspace,
                mission,
                lease,
                actor,
                task_run,
                snapshot,
                approval,
                broker_op,
                payload,
                prev_hash,
                hash,
            ) = row.map_err(backend)?;
            let at = DateTime::parse_from_rfc3339(&at)
                .map_err(|error| StoreError::Backend(format!("audit timestamp: {error}")))?
                .with_timezone(&Utc);
            let event = AuditEvent {
                seq,
                at,
                kind: decode("audit kind", &kind)?,
                workspace: workspace.map(WorkspaceId::parse).transpose()?,
                mission: mission.map(MissionId::parse).transpose()?,
                lease: lease.map(LeaseId::parse).transpose()?,
                actor: actor.map(ActorId::parse).transpose()?,
                task_run: task_run.map(TaskRunId::parse).transpose()?,
                snapshot: snapshot.map(SnapshotId::parse).transpose()?,
                artifacts: Vec::new(),
                approval: approval.map(ApprovalId::parse).transpose()?,
                broker_op: broker_op.map(BrokerOpId::parse).transpose()?,
                payload: decode("audit payload", &payload)?,
                prev_hash,
                hash,
            };
            if filter.matches(&event) {
                events.push(event);
            }
        }
        if let Some(limit) = filter.limit
            && events.len() > limit
        {
            events = events.split_off(events.len() - limit);
        }
        Ok(events)
    }

    fn audit_head(&self) -> Result<Option<AuditChainHead>> {
        self.conn()?
            .query_row("SELECT seq, hash FROM audit_head WHERE id = 0", [], |row| {
                Ok(AuditChainHead {
                    seq: row.get(0)?,
                    hash: row.get(1)?,
                })
            })
            .optional()
            .map_err(backend)
    }

    fn verify_audit(&self) -> Result<()> {
        let events = self.list_audit(&AuditFilter::default())?;
        let head = self.audit_head()?;
        verify_chain(&events, head.as_ref())?;
        Ok(())
    }

    fn record_config_load(&self, load: ConfigLoad) -> Result<()> {
        let payload = encode("config load", &load)?;
        self.conn()?
            .execute(
                "INSERT INTO config_loads (workspace, source, digest, payload)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    load.workspace.as_ref().map(WorkspaceId::as_str),
                    load.source,
                    load.digest.as_str(),
                    payload
                ],
            )
            .map(|_| ())
            .map_err(backend)
    }

    fn list_config_loads(&self, workspace: Option<&WorkspaceId>) -> Result<Vec<ConfigLoad>> {
        let guard = self.conn()?;
        match workspace {
            Some(id) => many(
                &guard,
                "config load",
                "SELECT payload FROM config_loads WHERE workspace = ?1 ORDER BY seq",
                &[&id.as_str()],
            ),
            None => many(
                &guard,
                "config load",
                "SELECT payload FROM config_loads ORDER BY seq",
                &[],
            ),
        }
    }

    fn record_bundle(&self, record: BundleRecord) -> Result<()> {
        let payload = encode("bundle", &record)?;
        self.conn()?
            .execute(
                "INSERT INTO dep_bundles (artifact, lockfile_digest, created_at, payload)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(artifact) DO UPDATE SET
                   lockfile_digest = excluded.lockfile_digest,
                   payload = excluded.payload",
                params![
                    record.artifact.as_str(),
                    record.lockfile_digest.as_str(),
                    record.created_at.to_rfc3339(),
                    payload
                ],
            )
            .map(|_| ())
            .map_err(backend)
    }

    fn find_bundle_for_lockfile(&self, lockfile_digest: &Digest) -> Result<Option<BundleRecord>> {
        one(
            &self.conn()?,
            "bundle",
            "SELECT payload FROM dep_bundles WHERE lockfile_digest = ?1
             ORDER BY created_at DESC LIMIT 1",
            lockfile_digest.as_str(),
        )
    }

    fn list_bundles(&self) -> Result<Vec<BundleRecord>> {
        many(
            &self.conn()?,
            "bundle",
            "SELECT payload FROM dep_bundles ORDER BY created_at",
            &[],
        )
    }

    fn set_bundle_inventory_confirmed(
        &self,
        bundle: &ArtifactId,
        confirmed_by: ActorId,
        at: DateTime<Utc>,
    ) -> Result<()> {
        if !confirmed_by.is_human() {
            return Err(StoreError::Validation(
                clyde_core::ValidationError::NotHumanActor {
                    actor: confirmed_by.to_string(),
                },
            ));
        }
        self.conn()?
            .execute(
                "INSERT INTO bundle_inventory_confirmations (artifact, confirmed_by, confirmed_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(artifact) DO UPDATE SET
                   confirmed_by = excluded.confirmed_by,
                   confirmed_at = excluded.confirmed_at",
                params![bundle.as_str(), confirmed_by.as_str(), at.to_rfc3339()],
            )
            .map(|_| ())
            .map_err(backend)
    }

    fn is_bundle_inventory_confirmed(&self, bundle: &ArtifactId) -> Result<bool> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT 1 FROM bundle_inventory_confirmations WHERE artifact = ?1",
                params![bundle.as_str()],
                |_| Ok(true),
            )
            .optional()
            .map_err(backend)?
            .unwrap_or(false))
    }

    fn passing_task_evidence(&self, mission: &MissionId) -> Result<Vec<(TaskType, TaskRunId)>> {
        let runs = self.list_task_runs(mission)?;
        Ok(runs
            .into_iter()
            .filter(|run| run.state == TaskRunState::Succeeded)
            .map(|run| (run.request.task, run.id))
            .collect())
    }
}
