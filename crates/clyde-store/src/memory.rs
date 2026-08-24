//! In-memory store.
//!
//! Used by the mission, lease, and approval tests so that logic above the store
//! is testable with no SQLite dependency (Phase 0 deliverable 8). It implements
//! the same invariants as the SQLite store — one active mission per workspace,
//! transactional revocation fan-out, an append-only hash-chained audit log — so a
//! test that passes here is testing the real rules.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use clyde_core::Digest;
use clyde_core::RepoPath;
use clyde_core::actor::Actor;
use clyde_core::approval::{ApprovalDecision, ApprovalRequest, Decision};
use clyde_core::artifact::Artifact;
use clyde_core::audit::{AuditChainHead, AuditEvent, AuditEventDraft, GENESIS_HASH, verify_chain};
use clyde_core::baseline::{AccessBaseline, BaselineKey, BaselineProposal};
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

#[derive(Debug, Default)]
struct State {
    workspaces: BTreeMap<WorkspaceId, Workspace>,
    actors: BTreeMap<ActorId, Actor>,
    missions: BTreeMap<MissionId, Mission>,
    leases: BTreeMap<LeaseId, Lease>,
    sessions: Vec<ActorSession>,
    task_runs: BTreeMap<TaskRunId, TaskRun>,
    policy_decisions: Vec<PolicyDecision>,
    snapshots: BTreeMap<SnapshotId, Snapshot>,
    artifacts: BTreeMap<ArtifactId, Artifact>,
    approvals: BTreeMap<ApprovalId, ApprovalRecord>,
    broker_ops: BTreeMap<BrokerOpId, BrokeredOperation>,
    egress: Vec<EgressAttempt>,
    baselines: BTreeMap<BaselineKey, AccessBaseline>,
    proposals: BTreeMap<BaselineKey, BaselineProposal>,
    audit: Vec<AuditEvent>,
    audit_head: Option<AuditChainHead>,
    config_loads: Vec<ConfigLoad>,
    bundles: BTreeMap<ArtifactId, BundleRecord>,
    bundle_confirmations: BTreeMap<ArtifactId, (ActorId, DateTime<Utc>)>,
}

/// An in-memory [`Store`].
#[derive(Debug, Default)]
pub struct MemoryStore {
    state: Mutex<State>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes the lock, converting poisoning into a typed error rather than a
    /// panic. A poisoned lock means another thread failed mid-transaction, and
    /// the honest response is to refuse further work.
    fn state(&self) -> Result<std::sync::MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| StoreError::Unavailable("in-memory store lock is poisoned"))
    }

    /// Test-only accessor for corrupting the audit log, so truncation detection
    /// can be exercised.
    #[cfg(any(test, feature = "test-support"))]
    pub fn truncate_audit_for_test(&self, keep: usize) -> Result<()> {
        let mut state = self.state()?;
        state.audit.truncate(keep);
        Ok(())
    }
}

/// Resolves the mission a task run belongs to, via its lease.
fn mission_of_run(state: &State, run: &TaskRun) -> Option<MissionId> {
    state
        .leases
        .get(&run.request.lease)
        .map(|lease| lease.mission.clone())
}

impl Store for MemoryStore {
    fn register_workspace(&self, workspace: Workspace) -> Result<()> {
        workspace.validate()?;
        let mut state = self.state()?;
        if state.workspaces.contains_key(&workspace.id) {
            return Err(StoreError::AlreadyExists(workspace.id.to_string()));
        }
        state.workspaces.insert(workspace.id.clone(), workspace);
        Ok(())
    }

    fn get_workspace(&self, id: &WorkspaceId) -> Result<Workspace> {
        self.state()?
            .workspaces
            .get(id)
            .cloned()
            .ok_or_else(|| StoreError::UnknownWorkspace(id.clone()))
    }

    fn find_workspace_by_root(&self, root: &Path) -> Result<Option<Workspace>> {
        Ok(self
            .state()?
            .workspaces
            .values()
            .find(|workspace| workspace.root == root)
            .cloned())
    }

    fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        Ok(self.state()?.workspaces.values().cloned().collect())
    }

    fn set_workspace_policy_digest(&self, id: &WorkspaceId, digest: Option<Digest>) -> Result<()> {
        let mut state = self.state()?;
        let workspace = state
            .workspaces
            .get_mut(id)
            .ok_or_else(|| StoreError::UnknownWorkspace(id.clone()))?;
        workspace.policy_digest = digest;
        Ok(())
    }

    fn upsert_actor(&self, actor: Actor) -> Result<()> {
        actor.validate()?;
        self.state()?.actors.insert(actor.id.clone(), actor);
        Ok(())
    }

    fn get_actor(&self, id: &ActorId) -> Result<Option<Actor>> {
        Ok(self.state()?.actors.get(id).cloned())
    }

    fn create_mission(&self, mission: Mission) -> Result<()> {
        mission.validate()?;
        let mut state = self.state()?;
        if !state.workspaces.contains_key(&mission.workspace) {
            return Err(StoreError::UnknownWorkspace(mission.workspace.clone()));
        }
        // One active mission per workspace (D16), checked in the same critical
        // section as the insert.
        if let Some(existing) = state
            .missions
            .values()
            .find(|candidate| {
                candidate.workspace == mission.workspace && !candidate.state.is_terminal()
            })
            .map(|candidate| candidate.id.clone())
        {
            return Err(StoreError::MissionAlreadyActive {
                workspace: mission.workspace,
                existing,
            });
        }
        if state.missions.contains_key(&mission.id) {
            return Err(StoreError::AlreadyExists(mission.id.to_string()));
        }
        state.missions.insert(mission.id.clone(), mission);
        Ok(())
    }

    fn get_mission(&self, id: &MissionId) -> Result<Mission> {
        self.state()?
            .missions
            .get(id)
            .cloned()
            .ok_or_else(|| StoreError::UnknownMission(id.clone()))
    }

    fn list_missions(&self, workspace: Option<&WorkspaceId>) -> Result<Vec<Mission>> {
        Ok(self
            .state()?
            .missions
            .values()
            .filter(|mission| workspace.is_none_or(|id| &mission.workspace == id))
            .cloned()
            .collect())
    }

    fn active_mission(&self, workspace: &WorkspaceId) -> Result<Option<Mission>> {
        Ok(self
            .state()?
            .missions
            .values()
            .find(|mission| &mission.workspace == workspace && !mission.state.is_terminal())
            .cloned())
    }

    fn transition_mission(&self, id: &MissionId, to: MissionState) -> Result<Mission> {
        let mut state = self.state()?;
        let mission = state
            .missions
            .get_mut(id)
            .ok_or_else(|| StoreError::UnknownMission(id.clone()))?;
        mission.state = mission.state.transition(to)?;
        Ok(mission.clone())
    }

    fn set_mission_cache_dir(&self, id: &MissionId, cache_dir: Option<PathBuf>) -> Result<()> {
        let mut state = self.state()?;
        let mission = state
            .missions
            .get_mut(id)
            .ok_or_else(|| StoreError::UnknownMission(id.clone()))?;
        mission.cache_dir = cache_dir;
        Ok(())
    }

    fn close_mission(&self, closeout: MissionCloseout) -> Result<Mission> {
        let mut state = self.state()?;
        let mission = state
            .missions
            .get(&closeout.mission)
            .cloned()
            .ok_or_else(|| StoreError::UnknownMission(closeout.mission.clone()))?;
        let next_state = mission.state.transition(closeout.final_state)?;

        // Revoke every lease and session for the mission, and freeze in-flight
        // brokered work, in the same critical section as the transition.
        let lease_ids: Vec<LeaseId> = state
            .leases
            .values()
            .filter(|lease| lease.mission == closeout.mission)
            .map(|lease| lease.id.clone())
            .collect();
        for id in &lease_ids {
            if let Some(lease) = state.leases.get_mut(id)
                && !lease.state.is_terminal()
            {
                lease.state = LeaseState::Revoked;
            }
        }
        for session in state.sessions.iter_mut() {
            if lease_ids.contains(&session.lease) && session.revoked_at.is_none() {
                session.revoked_at = Some(closeout.closed_at);
            }
        }
        let broker_ids: Vec<BrokerOpId> = state
            .broker_ops
            .values()
            .filter(|op| op.mission == closeout.mission && !op.state.is_terminal())
            .map(|op| op.id.clone())
            .collect();
        for id in broker_ids {
            if let Some(op) = state.broker_ops.get_mut(&id) {
                op.state = op.state.transition(BrokerOpState::Frozen)?;
                op.finished_at = Some(closeout.closed_at);
            }
        }

        let mission = state
            .missions
            .get_mut(&closeout.mission)
            .ok_or_else(|| StoreError::UnknownMission(closeout.mission.clone()))?;
        mission.state = next_state;
        mission.closed_at = Some(closeout.closed_at);
        Ok(mission.clone())
    }

    fn insert_lease(&self, lease: Lease) -> Result<()> {
        lease.validate()?;
        let mut state = self.state()?;
        if !state.missions.contains_key(&lease.mission) {
            return Err(StoreError::UnknownMission(lease.mission.clone()));
        }
        if state.leases.contains_key(&lease.id) {
            return Err(StoreError::AlreadyExists(lease.id.to_string()));
        }
        state.leases.insert(lease.id.clone(), lease);
        Ok(())
    }

    fn get_lease(&self, id: &LeaseId) -> Result<Lease> {
        self.state()?
            .leases
            .get(id)
            .cloned()
            .ok_or_else(|| StoreError::UnknownLease(id.clone()))
    }

    fn list_leases(&self, mission: &MissionId) -> Result<Vec<Lease>> {
        Ok(self
            .state()?
            .leases
            .values()
            .filter(|lease| &lease.mission == mission)
            .cloned()
            .collect())
    }

    fn set_lease_state(&self, id: &LeaseId, to: LeaseState) -> Result<Lease> {
        let mut state = self.state()?;
        let lease = state
            .leases
            .get_mut(id)
            .ok_or_else(|| StoreError::UnknownLease(id.clone()))?;
        lease.state = lease.state.transition(to)?;
        Ok(lease.clone())
    }

    fn revoke_lease_tree(&self, id: &LeaseId, at: DateTime<Utc>) -> Result<Vec<LeaseId>> {
        let mut state = self.state()?;
        if !state.leases.contains_key(id) {
            return Err(StoreError::UnknownLease(id.clone()));
        }
        // Collect the transitive closure first, so the fan-out is computed once
        // and applied atomically.
        let mut to_revoke = vec![id.clone()];
        let mut frontier = vec![id.clone()];
        while let Some(current) = frontier.pop() {
            let children: Vec<LeaseId> = state
                .leases
                .values()
                .filter(|lease| lease.parent.as_ref() == Some(&current))
                .map(|lease| lease.id.clone())
                .collect();
            for child in children {
                if !to_revoke.contains(&child) {
                    to_revoke.push(child.clone());
                    frontier.push(child);
                }
            }
        }
        for lease_id in &to_revoke {
            if let Some(lease) = state.leases.get_mut(lease_id)
                && !lease.state.is_terminal()
            {
                lease.state = LeaseState::Revoked;
            }
        }
        for session in state.sessions.iter_mut() {
            if to_revoke.contains(&session.lease) && session.revoked_at.is_none() {
                session.revoked_at = Some(at);
            }
        }
        Ok(to_revoke)
    }

    fn charge_lease(&self, id: &LeaseId, cost: &BudgetCost) -> Result<BudgetUsage> {
        let mut state = self.state()?;
        let lease = state
            .leases
            .get_mut(id)
            .ok_or_else(|| StoreError::UnknownLease(id.clone()))?;
        let next = clyde_policy::charge_budget(&lease.budget, &lease.usage, cost)
            .map_err(|reason| StoreError::Backend(reason.render()))?;
        lease.usage = next;
        if clyde_policy::budget::exhausted_dimension(&lease.budget, &lease.usage).is_some()
            && lease.state == LeaseState::Active
        {
            lease.state = LeaseState::Exhausted;
        }
        Ok(next)
    }

    fn release_parallel_slot(&self, id: &LeaseId) -> Result<BudgetUsage> {
        let mut state = self.state()?;
        let lease = state
            .leases
            .get_mut(id)
            .ok_or_else(|| StoreError::UnknownLease(id.clone()))?;
        lease.usage = lease.usage.release_parallel_subagent();
        Ok(lease.usage)
    }

    fn renew_lease(&self, renewal: LeaseRenewal) -> Result<Lease> {
        renewal.replacement.validate()?;
        let mut state = self.state()?;
        {
            let old = state
                .leases
                .get_mut(&renewal.superseded)
                .ok_or_else(|| StoreError::UnknownLease(renewal.superseded.clone()))?;
            old.state = old.state.transition(LeaseState::Superseded)?;
        }
        if state.leases.contains_key(&renewal.replacement.id) {
            return Err(StoreError::AlreadyExists(
                renewal.replacement.id.to_string(),
            ));
        }
        state
            .leases
            .insert(renewal.replacement.id.clone(), renewal.replacement.clone());
        Ok(renewal.replacement)
    }

    fn bind_session(&self, session: ActorSession) -> Result<()> {
        let mut state = self.state()?;
        let lease = state
            .leases
            .get(&session.lease)
            .ok_or_else(|| StoreError::UnknownLease(session.lease.clone()))?;
        session.validate(lease.expires_at)?;
        state.sessions.push(session);
        Ok(())
    }

    fn resolve_token(
        &self,
        hash: &TokenHash,
        now: DateTime<Utc>,
    ) -> Result<Option<ResolvedSession>> {
        let state = self.state()?;
        // Unknown, expired, and revoked tokens all return None: the caller must
        // not be able to tell which.
        let Some(session) = state
            .sessions
            .iter()
            .find(|session| session.token_hash.matches(hash) && session.is_valid_at(now))
        else {
            return Ok(None);
        };
        let Some(lease) = state.leases.get(&session.lease) else {
            return Ok(None);
        };
        let Some(mission) = state.missions.get(&lease.mission) else {
            return Ok(None);
        };
        Ok(Some(ResolvedSession {
            session: session.clone(),
            lease: lease.clone(),
            mission: mission.clone(),
        }))
    }

    fn list_sessions(&self, mission: &MissionId) -> Result<Vec<ActorSession>> {
        let state = self.state()?;
        let leases: Vec<&LeaseId> = state
            .leases
            .values()
            .filter(|lease| &lease.mission == mission)
            .map(|lease| &lease.id)
            .collect();
        Ok(state
            .sessions
            .iter()
            .filter(|session| leases.contains(&&session.lease))
            .cloned()
            .collect())
    }

    fn revoke_sessions_for_lease(&self, lease: &LeaseId, at: DateTime<Utc>) -> Result<usize> {
        let mut state = self.state()?;
        let mut revoked = 0;
        for session in state.sessions.iter_mut() {
            if &session.lease == lease && session.revoked_at.is_none() {
                session.revoked_at = Some(at);
                revoked += 1;
            }
        }
        Ok(revoked)
    }

    fn set_session_sandbox(&self, lease: &LeaseId, sandbox: Option<String>) -> Result<()> {
        let mut state = self.state()?;
        for session in state.sessions.iter_mut() {
            if &session.lease == lease {
                session.sandbox = sandbox.clone();
            }
        }
        Ok(())
    }

    fn insert_task_run(&self, run: TaskRun) -> Result<()> {
        run.request.validate()?;
        let mut state = self.state()?;
        if !state.leases.contains_key(&run.request.lease) {
            return Err(StoreError::UnknownLease(run.request.lease.clone()));
        }
        if state.task_runs.contains_key(&run.id) {
            return Err(StoreError::AlreadyExists(run.id.to_string()));
        }
        state.task_runs.insert(run.id.clone(), run);
        Ok(())
    }

    fn get_task_run(&self, id: &TaskRunId) -> Result<TaskRun> {
        self.state()?
            .task_runs
            .get(id)
            .cloned()
            .ok_or_else(|| StoreError::UnknownTaskRun(id.clone()))
    }

    fn list_task_runs(&self, mission: &MissionId) -> Result<Vec<TaskRun>> {
        let state = self.state()?;
        Ok(state
            .task_runs
            .values()
            .filter(|run| mission_of_run(&state, run).as_ref() == Some(mission))
            .cloned()
            .collect())
    }

    fn transition_task_run(&self, id: &TaskRunId, to: TaskRunState) -> Result<TaskRun> {
        let mut state = self.state()?;
        let run = state
            .task_runs
            .get_mut(id)
            .ok_or_else(|| StoreError::UnknownTaskRun(id.clone()))?;
        run.state = run.state.transition(to)?;
        if matches!(to, TaskRunState::Running) && run.started_at.is_none() {
            run.started_at = Some(Utc::now());
        }
        Ok(run.clone())
    }

    fn complete_task_run(
        &self,
        id: &TaskRunId,
        to: TaskRunState,
        outcome: TaskOutcome,
        finished_at: DateTime<Utc>,
        artifacts: Vec<ArtifactId>,
    ) -> Result<TaskRun> {
        let mut state = self.state()?;
        let run = state
            .task_runs
            .get_mut(id)
            .ok_or_else(|| StoreError::UnknownTaskRun(id.clone()))?;
        run.state = run.state.transition(to)?;
        run.outcome = Some(outcome);
        run.finished_at = Some(finished_at);
        run.artifacts.extend(artifacts);
        Ok(run.clone())
    }

    fn set_task_run_snapshot(&self, id: &TaskRunId, snapshot: SnapshotId) -> Result<()> {
        let mut state = self.state()?;
        let run = state
            .task_runs
            .get_mut(id)
            .ok_or_else(|| StoreError::UnknownTaskRun(id.clone()))?;
        run.snapshot = Some(snapshot);
        Ok(())
    }

    fn set_task_run_bundle(&self, id: &TaskRunId, bundle: ArtifactId) -> Result<()> {
        let mut state = self.state()?;
        let run = state
            .task_runs
            .get_mut(id)
            .ok_or_else(|| StoreError::UnknownTaskRun(id.clone()))?;
        run.dependency_bundle = Some(bundle);
        Ok(())
    }

    fn record_policy_decision(&self, decision: PolicyDecision) -> Result<()> {
        self.state()?.policy_decisions.push(decision);
        Ok(())
    }

    fn list_policy_decisions(&self, _mission: &MissionId) -> Result<Vec<PolicyDecision>> {
        Ok(self.state()?.policy_decisions.clone())
    }

    fn insert_snapshot(&self, snapshot: Snapshot) -> Result<()> {
        snapshot.manifest.validate()?;
        self.state()?
            .snapshots
            .insert(snapshot.id.clone(), snapshot);
        Ok(())
    }

    fn get_snapshot(&self, id: &SnapshotId) -> Result<Snapshot> {
        self.state()?
            .snapshots
            .get(id)
            .cloned()
            .ok_or_else(|| StoreError::UnknownSnapshot(id.clone()))
    }

    fn snapshot_contains(&self, id: &SnapshotId, path: &RepoPath) -> Result<bool> {
        let state = self.state()?;
        let snapshot = state
            .snapshots
            .get(id)
            .ok_or_else(|| StoreError::UnknownSnapshot(id.clone()))?;
        Ok(snapshot.manifest.contains(path))
    }

    fn insert_artifact(&self, artifact: Artifact) -> Result<()> {
        self.state()?
            .artifacts
            .insert(artifact.id.clone(), artifact);
        Ok(())
    }

    fn get_artifact(&self, id: &ArtifactId) -> Result<Artifact> {
        self.state()?
            .artifacts
            .get(id)
            .cloned()
            .ok_or_else(|| StoreError::UnknownArtifact(id.clone()))
    }

    fn list_artifacts(&self, mission: &MissionId) -> Result<Vec<Artifact>> {
        Ok(self
            .state()?
            .artifacts
            .values()
            .filter(|artifact| &artifact.mission == mission)
            .cloned()
            .collect())
    }

    fn insert_approval_request(&self, request: ApprovalRequest) -> Result<()> {
        request.validate()?;
        let mut state = self.state()?;
        if state.approvals.contains_key(&request.id) {
            return Err(StoreError::AlreadyExists(request.id.to_string()));
        }
        state.approvals.insert(
            request.id.clone(),
            ApprovalRecord {
                request,
                decision: None,
            },
        );
        Ok(())
    }

    fn get_approval(&self, id: &ApprovalId) -> Result<ApprovalRecord> {
        self.state()?
            .approvals
            .get(id)
            .cloned()
            .ok_or_else(|| StoreError::UnknownApproval(id.clone()))
    }

    fn list_pending_approvals(&self, now: DateTime<Utc>) -> Result<Vec<ApprovalRequest>> {
        Ok(self
            .state()?
            .approvals
            .values()
            .filter(|record| record.decision.is_none() && !record.request.is_expired_at(now))
            .map(|record| record.request.clone())
            .collect())
    }

    fn list_approvals(&self, mission: &MissionId) -> Result<Vec<ApprovalRecord>> {
        Ok(self
            .state()?
            .approvals
            .values()
            .filter(|record| &record.request.mission == mission)
            .cloned()
            .collect())
    }

    fn record_approval_decision(&self, decision: ApprovalDecision) -> Result<()> {
        decision.validate()?;
        let mut state = self.state()?;
        let record = state
            .approvals
            .get_mut(&decision.request)
            .ok_or_else(|| StoreError::UnknownApproval(decision.request.clone()))?;
        record.decision = Some(decision);
        Ok(())
    }

    fn find_authorising_approval(
        &self,
        mission: &MissionId,
        digest: &Digest,
        now: DateTime<Utc>,
    ) -> Result<Option<ApprovalRecord>> {
        Ok(self
            .state()?
            .approvals
            .values()
            .find(|record| &record.request.mission == mission && record.authorises(digest, now))
            .cloned())
    }

    fn consume_approval(&self, id: &ApprovalId, at: DateTime<Utc>) -> Result<()> {
        let mut state = self.state()?;
        let record = state
            .approvals
            .get_mut(id)
            .ok_or_else(|| StoreError::UnknownApproval(id.clone()))?;
        let decision = record
            .decision
            .as_mut()
            .ok_or_else(|| StoreError::ApprovalUndecided(id.clone()))?;
        match decision.decision {
            Decision::ApproveOnce => {
                if decision.consumed_at.is_some() {
                    return Err(StoreError::ApprovalAlreadyConsumed(id.clone()));
                }
                decision.consumed_at = Some(at);
                Ok(())
            }
            // Mission-scoped approvals are not consumed; denials cannot be.
            Decision::ApproveForMission => Ok(()),
            Decision::Deny => Err(StoreError::ApprovalUndecided(id.clone())),
        }
    }

    fn insert_broker_op(&self, op: BrokeredOperation) -> Result<()> {
        op.kind.validate()?;
        let mut state = self.state()?;
        if state.broker_ops.contains_key(&op.id) {
            return Err(StoreError::AlreadyExists(op.id.to_string()));
        }
        state.broker_ops.insert(op.id.clone(), op);
        Ok(())
    }

    fn get_broker_op(&self, id: &BrokerOpId) -> Result<BrokeredOperation> {
        self.state()?
            .broker_ops
            .get(id)
            .cloned()
            .ok_or_else(|| StoreError::UnknownBrokerOp(id.clone()))
    }

    fn list_broker_ops(&self, mission: &MissionId) -> Result<Vec<BrokeredOperation>> {
        Ok(self
            .state()?
            .broker_ops
            .values()
            .filter(|op| &op.mission == mission)
            .cloned()
            .collect())
    }

    fn transition_broker_op(
        &self,
        id: &BrokerOpId,
        to: BrokerOpState,
        summary: Option<String>,
        at: DateTime<Utc>,
    ) -> Result<BrokeredOperation> {
        let mut state = self.state()?;
        let op = state
            .broker_ops
            .get_mut(id)
            .ok_or_else(|| StoreError::UnknownBrokerOp(id.clone()))?;
        op.state = op.state.transition(to)?;
        if let Some(summary) = summary {
            op.result_summary = Some(summary);
        }
        if op.state.is_terminal() {
            op.finished_at = Some(at);
        }
        Ok(op.clone())
    }

    fn freeze_broker_ops(&self, mission: &MissionId, at: DateTime<Utc>) -> Result<Vec<BrokerOpId>> {
        let mut state = self.state()?;
        let ids: Vec<BrokerOpId> = state
            .broker_ops
            .values()
            .filter(|op| &op.mission == mission && !op.state.is_terminal())
            .map(|op| op.id.clone())
            .collect();
        for id in &ids {
            if let Some(op) = state.broker_ops.get_mut(id) {
                op.state = op.state.transition(BrokerOpState::Frozen)?;
                op.finished_at = Some(at);
            }
        }
        Ok(ids)
    }

    fn insert_egress_attempt(&self, attempt: EgressAttempt) -> Result<()> {
        self.state()?.egress.push(attempt);
        Ok(())
    }

    fn list_egress_attempts(&self, task_run: &TaskRunId) -> Result<Vec<EgressAttempt>> {
        Ok(self
            .state()?
            .egress
            .iter()
            .filter(|attempt| attempt.task_run.as_ref() == Some(task_run))
            .cloned()
            .collect())
    }

    fn list_mission_egress_attempts(&self, mission: &MissionId) -> Result<Vec<EgressAttempt>> {
        let state = self.state()?;
        let runs: Vec<TaskRunId> = state
            .task_runs
            .values()
            .filter(|run| mission_of_run(&state, run).as_ref() == Some(mission))
            .map(|run| run.id.clone())
            .collect();
        Ok(state
            .egress
            .iter()
            .filter(|attempt| {
                attempt
                    .task_run
                    .as_ref()
                    .is_some_and(|id| runs.contains(id))
            })
            .cloned()
            .collect())
    }

    fn put_baseline(&self, baseline: AccessBaseline) -> Result<()> {
        baseline.validate()?;
        self.state()?.baselines.insert(baseline.key(), baseline);
        Ok(())
    }

    fn get_baseline(&self, key: &BaselineKey) -> Result<Option<AccessBaseline>> {
        Ok(self.state()?.baselines.get(key).cloned())
    }

    fn list_baselines(&self, workspace: &WorkspaceId) -> Result<Vec<AccessBaseline>> {
        Ok(self
            .state()?
            .baselines
            .values()
            .filter(|baseline| &baseline.workspace == workspace)
            .cloned()
            .collect())
    }

    fn delete_baseline(&self, key: &BaselineKey) -> Result<bool> {
        Ok(self.state()?.baselines.remove(key).is_some())
    }

    fn put_baseline_proposal(&self, proposal: BaselineProposal) -> Result<()> {
        let key = BaselineKey {
            workspace: proposal.workspace.clone(),
            task: proposal.task,
            target: proposal.target.clone(),
        };
        self.state()?.proposals.insert(key, proposal);
        Ok(())
    }

    fn get_baseline_proposal(&self, key: &BaselineKey) -> Result<Option<BaselineProposal>> {
        Ok(self.state()?.proposals.get(key).cloned())
    }

    fn delete_baseline_proposal(&self, key: &BaselineKey) -> Result<bool> {
        Ok(self.state()?.proposals.remove(key).is_some())
    }

    fn append_audit(&self, draft: AuditEventDraft) -> Result<AuditEvent> {
        let mut state = self.state()?;
        let (seq, prev_hash) = match &state.audit_head {
            Some(head) => (head.seq.saturating_add(1), head.hash.clone()),
            None => (1, GENESIS_HASH.to_owned()),
        };
        let event = AuditEvent::seal(draft, seq, prev_hash)
            .map_err(|error| StoreError::Backend(error.to_string()))?;
        state.audit_head = Some(AuditChainHead {
            seq: event.seq,
            hash: event.hash.clone(),
        });
        state.audit.push(event.clone());
        Ok(event)
    }

    fn list_audit(&self, filter: &AuditFilter) -> Result<Vec<AuditEvent>> {
        let state = self.state()?;
        let mut events: Vec<AuditEvent> = state
            .audit
            .iter()
            .filter(|event| filter.matches(event))
            .cloned()
            .collect();
        if let Some(limit) = filter.limit
            && events.len() > limit
        {
            events = events.split_off(events.len() - limit);
        }
        Ok(events)
    }

    fn audit_head(&self) -> Result<Option<AuditChainHead>> {
        Ok(self.state()?.audit_head.clone())
    }

    fn verify_audit(&self) -> Result<()> {
        let state = self.state()?;
        verify_chain(&state.audit, state.audit_head.as_ref())?;
        Ok(())
    }

    fn record_config_load(&self, load: ConfigLoad) -> Result<()> {
        self.state()?.config_loads.push(load);
        Ok(())
    }

    fn list_config_loads(&self, workspace: Option<&WorkspaceId>) -> Result<Vec<ConfigLoad>> {
        Ok(self
            .state()?
            .config_loads
            .iter()
            .filter(|load| workspace.is_none_or(|id| load.workspace.as_ref() == Some(id)))
            .cloned()
            .collect())
    }

    fn record_bundle(&self, record: BundleRecord) -> Result<()> {
        self.state()?
            .bundles
            .insert(record.artifact.clone(), record);
        Ok(())
    }

    fn find_bundle_for_lockfile(&self, lockfile_digest: &Digest) -> Result<Option<BundleRecord>> {
        Ok(self
            .state()?
            .bundles
            .values()
            .filter(|record| &record.lockfile_digest == lockfile_digest)
            .max_by_key(|record| record.created_at)
            .cloned())
    }

    fn list_bundles(&self) -> Result<Vec<BundleRecord>> {
        Ok(self.state()?.bundles.values().cloned().collect())
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
        self.state()?
            .bundle_confirmations
            .insert(bundle.clone(), (confirmed_by, at));
        Ok(())
    }

    fn is_bundle_inventory_confirmed(&self, bundle: &ArtifactId) -> Result<bool> {
        Ok(self.state()?.bundle_confirmations.contains_key(bundle))
    }

    fn passing_task_evidence(&self, mission: &MissionId) -> Result<Vec<(TaskType, TaskRunId)>> {
        let state = self.state()?;
        Ok(state
            .task_runs
            .values()
            .filter(|run| mission_of_run(&state, run).as_ref() == Some(mission))
            .filter(|run| run.state == TaskRunState::Succeeded)
            .map(|run| (run.request.task, run.id.clone()))
            .collect())
    }
}
