//! Forward-only schema migrations.
//!
//! Migrations are versioned and forward-only from Phase 0, even though the
//! schema is expected to churn: a schema that cannot be migrated cannot be
//! dogfooded (schema reference: storage rules).
//!
//! Entities are stored as their canonical JSON encoding in a `payload` column,
//! alongside real columns for everything queried or constrained. The types are
//! validated Rust values with a stable serde encoding, so this keeps one
//! definition of each entity's shape while leaving the queries, foreign keys, and
//! uniqueness constraints in SQL where the database can enforce them.

/// One migration step.
pub struct Migration {
    pub version: u32,
    pub name: &'static str,
    pub sql: &'static str,
}

/// Every migration, in order. Never edit a released migration; add another.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial-schema",
        sql: r#"
CREATE TABLE workspaces (
    id          TEXT PRIMARY KEY,
    root        TEXT NOT NULL UNIQUE,
    payload     TEXT NOT NULL
) STRICT;

CREATE TABLE actors (
    id          TEXT PRIMARY KEY,
    kind        TEXT NOT NULL,
    payload     TEXT NOT NULL
) STRICT;

CREATE TABLE missions (
    id          TEXT PRIMARY KEY,
    workspace   TEXT NOT NULL REFERENCES workspaces(id),
    state       TEXT NOT NULL,
    is_terminal INTEGER NOT NULL,
    created_at  TEXT NOT NULL,
    payload     TEXT NOT NULL
) STRICT;

-- One mission in a non-terminal state per workspace (D16). Expressed as a
-- partial unique index so the database enforces it, not only the store code.
CREATE UNIQUE INDEX missions_one_active_per_workspace
    ON missions(workspace) WHERE is_terminal = 0;

CREATE TABLE leases (
    id          TEXT PRIMARY KEY,
    mission     TEXT NOT NULL REFERENCES missions(id),
    parent      TEXT REFERENCES leases(id),
    actor       TEXT NOT NULL,
    state       TEXT NOT NULL,
    expires_at  TEXT NOT NULL,
    payload     TEXT NOT NULL
) STRICT;
CREATE INDEX leases_by_mission ON leases(mission);
CREATE INDEX leases_by_parent ON leases(parent);

CREATE TABLE actor_sessions (
    token_hash  TEXT PRIMARY KEY,
    lease       TEXT NOT NULL REFERENCES leases(id),
    actor       TEXT NOT NULL,
    expires_at  TEXT NOT NULL,
    revoked_at  TEXT,
    payload     TEXT NOT NULL
) STRICT;
CREATE INDEX sessions_by_lease ON actor_sessions(lease);

CREATE TABLE task_requests (
    id           TEXT PRIMARY KEY,
    lease        TEXT NOT NULL REFERENCES leases(id),
    actor        TEXT NOT NULL,
    task         TEXT NOT NULL,
    path         TEXT NOT NULL,
    requested_at TEXT NOT NULL,
    payload      TEXT NOT NULL
) STRICT;

CREATE TABLE task_runs (
    id          TEXT PRIMARY KEY REFERENCES task_requests(id),
    lease       TEXT NOT NULL REFERENCES leases(id),
    mission     TEXT NOT NULL REFERENCES missions(id),
    task        TEXT NOT NULL,
    state       TEXT NOT NULL,
    payload     TEXT NOT NULL
) STRICT;
CREATE INDEX task_runs_by_mission ON task_runs(mission);

CREATE TABLE snapshots (
    id          TEXT PRIMARY KEY,
    workspace   TEXT NOT NULL REFERENCES workspaces(id),
    mission     TEXT NOT NULL REFERENCES missions(id),
    payload     TEXT NOT NULL
) STRICT;

CREATE TABLE snapshot_entries (
    snapshot    TEXT NOT NULL REFERENCES snapshots(id),
    path        TEXT NOT NULL,
    mode        INTEGER NOT NULL,
    size        INTEGER NOT NULL,
    blake3      TEXT NOT NULL,
    PRIMARY KEY (snapshot, path)
) STRICT;

CREATE TABLE artifacts (
    id          TEXT PRIMARY KEY,
    mission     TEXT NOT NULL REFERENCES missions(id),
    kind        TEXT NOT NULL,
    produced_by TEXT,
    payload     TEXT NOT NULL
) STRICT;
CREATE INDEX artifacts_by_mission ON artifacts(mission);

CREATE TABLE approval_requests (
    id             TEXT PRIMARY KEY,
    mission        TEXT NOT NULL REFERENCES missions(id),
    lease          TEXT NOT NULL REFERENCES leases(id),
    request_digest TEXT NOT NULL,
    expires_at     TEXT NOT NULL,
    payload        TEXT NOT NULL
) STRICT;
CREATE INDEX approvals_by_mission ON approval_requests(mission);
CREATE INDEX approvals_by_digest ON approval_requests(request_digest);

CREATE TABLE approval_decisions (
    request     TEXT PRIMARY KEY REFERENCES approval_requests(id),
    decision    TEXT NOT NULL,
    decided_by  TEXT NOT NULL,
    consumed_at TEXT,
    payload     TEXT NOT NULL
) STRICT;

CREATE TABLE broker_ops (
    id          TEXT PRIMARY KEY,
    mission     TEXT NOT NULL REFERENCES missions(id),
    lease       TEXT NOT NULL REFERENCES leases(id),
    approval    TEXT NOT NULL REFERENCES approval_requests(id),
    state       TEXT NOT NULL,
    payload     TEXT NOT NULL
) STRICT;
CREATE INDEX broker_ops_by_mission ON broker_ops(mission);

CREATE TABLE policy_decisions (
    id          TEXT PRIMARY KEY,
    outcome     TEXT NOT NULL,
    decided_at  TEXT NOT NULL,
    payload     TEXT NOT NULL
) STRICT;

CREATE TABLE egress_attempts (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    task_run    TEXT,
    host        TEXT NOT NULL,
    decision    TEXT NOT NULL,
    payload     TEXT NOT NULL
) STRICT;
CREATE INDEX egress_by_task_run ON egress_attempts(task_run);

CREATE TABLE access_baselines (
    workspace   TEXT NOT NULL REFERENCES workspaces(id),
    task        TEXT NOT NULL,
    target      TEXT NOT NULL,
    origin      TEXT NOT NULL,
    digest      TEXT NOT NULL,
    payload     TEXT NOT NULL,
    PRIMARY KEY (workspace, task, target)
) STRICT;

CREATE TABLE baseline_paths (
    workspace   TEXT NOT NULL,
    task        TEXT NOT NULL,
    target      TEXT NOT NULL,
    path        TEXT NOT NULL,
    kind        TEXT NOT NULL,
    reason      TEXT,
    PRIMARY KEY (workspace, task, target, path),
    FOREIGN KEY (workspace, task, target)
        REFERENCES access_baselines(workspace, task, target) ON DELETE CASCADE
) STRICT;

CREATE TABLE code_exec_inventory (
    workspace     TEXT NOT NULL,
    task          TEXT NOT NULL,
    target        TEXT NOT NULL,
    crate_name    TEXT NOT NULL,
    version       TEXT NOT NULL,
    kind          TEXT NOT NULL,
    source_blake3 TEXT NOT NULL,
    PRIMARY KEY (workspace, task, target, crate_name),
    FOREIGN KEY (workspace, task, target)
        REFERENCES access_baselines(workspace, task, target) ON DELETE CASCADE
) STRICT;

CREATE TABLE baseline_proposals (
    workspace   TEXT NOT NULL REFERENCES workspaces(id),
    task        TEXT NOT NULL,
    target      TEXT NOT NULL,
    origin      TEXT NOT NULL,
    payload     TEXT NOT NULL,
    PRIMARY KEY (workspace, task, target)
) STRICT;

-- Append-only. No UPDATE or DELETE path exists in the store API, and the
-- head table records the tip so tail truncation is detectable.
CREATE TABLE audit_events (
    seq         INTEGER PRIMARY KEY,
    at          TEXT NOT NULL,
    kind        TEXT NOT NULL,
    workspace   TEXT,
    mission     TEXT,
    lease       TEXT,
    actor       TEXT,
    task_run    TEXT,
    snapshot    TEXT,
    approval    TEXT,
    broker_op   TEXT,
    payload     TEXT NOT NULL,
    prev_hash   TEXT NOT NULL,
    hash        TEXT NOT NULL
) STRICT;
CREATE INDEX audit_by_mission ON audit_events(mission);
CREATE INDEX audit_by_kind ON audit_events(kind);

CREATE TABLE audit_head (
    id          INTEGER PRIMARY KEY CHECK (id = 0),
    seq         INTEGER NOT NULL,
    hash        TEXT NOT NULL
) STRICT;

CREATE TABLE config_loads (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace   TEXT,
    source      TEXT NOT NULL,
    digest      TEXT NOT NULL,
    payload     TEXT NOT NULL
) STRICT;

CREATE TABLE dep_bundles (
    artifact        TEXT PRIMARY KEY,
    lockfile_digest TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    payload         TEXT NOT NULL
) STRICT;
CREATE INDEX bundles_by_lockfile ON dep_bundles(lockfile_digest);

CREATE TABLE bundle_inventory_confirmations (
    artifact     TEXT PRIMARY KEY,
    confirmed_by TEXT NOT NULL,
    confirmed_at TEXT NOT NULL
) STRICT;
"#,
    },
    Migration {
        version: 2,
        name: "task-request-principal",
        // D25 adds `principal` to a task request: how the request was authenticated,
        // as opposed to `actor`, which is who the work is attributed to.
        //
        // Rows written before this migration were all actor-driven, because the
        // operator surface did not exist to write any others. Backfilling them as
        // sessions is therefore a statement of fact about the code that wrote them
        // rather than an inference, and `hosted` follows the same reasoning: the only
        // way to hold a token then was to be an environment Clyde launched.
        sql: r#"
UPDATE task_requests
   SET payload = json_set(
         payload,
         '$.principal',
         json_object('kind', 'session', 'session_actor', actor, 'hosted', json('true'))
       )
 WHERE json_extract(payload, '$.principal') IS NULL;
"#,
    },
];
