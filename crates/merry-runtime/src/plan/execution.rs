use crate::{
    context::stable_content_hash,
    plan::{PlanError, PlanState, recovery::retry_backoff_elapsed, validation},
};
pub(crate) use directives::{PlanDirectiveDeliveryOutput, PlanDirectiveOutput};
use merry_core::{
    PlanAttemptId, PlanAttemptProgressSnapshot, PlanAttemptSnapshot,
    PlanCapabilityEnvelopeSnapshot, PlanLeaseId, PlanLeaseSnapshot, PlanLeaseStatus, PlanNodeId,
    PlanNodeResult, PlanNodeStatus, PlanPhase, PlanRevisionSummary, PlanSchedulerStatus,
    PlanSnapshot, SessionId,
};
pub(crate) use reporting::{PlanAttemptReportOutput, PlanProgressOutput};
use std::collections::{BTreeMap, BTreeSet};

mod directives;

mod reporting;

mod report_validation;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanAttemptActor {
    pub(crate) executor_session_id: SessionId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanAttemptStartOutput {
    pub(crate) snapshot: PlanSnapshot,
    pub(crate) attempt: PlanAttemptSnapshot,
    pub(crate) lease: PlanLeaseSnapshot,
    pub(crate) progress: PlanAttemptProgressSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanLocalAttemptStartOutput {
    pub(crate) snapshot: PlanSnapshot,
    pub(crate) attempt: PlanAttemptSnapshot,
    pub(crate) progress: PlanAttemptProgressSnapshot,
}

struct PlanAttemptStartRecords {
    snapshot: PlanSnapshot,
    attempt: PlanAttemptSnapshot,
    lease: Option<PlanLeaseSnapshot>,
    progress: PlanAttemptProgressSnapshot,
}

impl PlanState {
    pub(crate) fn ready_node_ids(&self) -> Vec<PlanNodeId> {
        self.ready_node_ids_at(u64::MAX)
    }

    pub(crate) fn ready_node_ids_at(&self, now_ms: u64) -> Vec<PlanNodeId> {
        if self.snapshot.phase != PlanPhase::Executing
            || self.snapshot.scheduler_status != PlanSchedulerStatus::Active
        {
            return Vec::new();
        }
        let completed = self
            .snapshot
            .nodes
            .iter()
            .filter(|node| node.status == PlanNodeStatus::Completed)
            .map(|node| node.id.clone())
            .collect::<BTreeSet<_>>();
        let live_leases = self
            .snapshot
            .leases
            .iter()
            .filter(|lease| lease.status == PlanLeaseStatus::Live)
            .map(|lease| lease.node_id.clone())
            .collect::<BTreeSet<_>>();
        let mut ready = self
            .snapshot
            .nodes
            .iter()
            .filter(|node| {
                matches!(
                    node.status,
                    PlanNodeStatus::Pending | PlanNodeStatus::Verifying
                )
            })
            .filter(|node| !live_leases.contains(&node.id))
            .filter(|node| node.depends_on.iter().all(|id| completed.contains(id)))
            .filter(|node| self.node_execution_shape_is_ready(node))
            .filter(|node| retry_backoff_elapsed(&self.snapshot, node, now_ms))
            .map(|node| node.id.clone())
            .collect::<Vec<_>>();
        ready.sort_by_key(|id| self.node_order_key(id));
        ready
    }

    pub(crate) fn start_attempt(
        &mut self,
        node_id: &PlanNodeId,
        actor: PlanAttemptActor,
        now_ms: u64,
    ) -> Result<PlanAttemptStartOutput, PlanError> {
        let records = self.start_attempt_records(node_id, actor, now_ms, true)?;
        Ok(PlanAttemptStartOutput {
            snapshot: records.snapshot,
            attempt: records.attempt,
            lease: records
                .lease
                .expect("subagent attempt creation always returns a lease"),
            progress: records.progress,
        })
    }

    pub(crate) fn start_local_attempt(
        &mut self,
        node_id: &PlanNodeId,
        actor: PlanAttemptActor,
        now_ms: u64,
    ) -> Result<PlanLocalAttemptStartOutput, PlanError> {
        let records = self.start_attempt_records(node_id, actor, now_ms, false)?;
        debug_assert!(records.lease.is_none());
        Ok(PlanLocalAttemptStartOutput {
            snapshot: records.snapshot,
            attempt: records.attempt,
            progress: records.progress,
        })
    }

    fn start_attempt_records(
        &mut self,
        node_id: &PlanNodeId,
        actor: PlanAttemptActor,
        now_ms: u64,
        create_subagent_lease: bool,
    ) -> Result<PlanAttemptStartRecords, PlanError> {
        let mut candidate = self.clone();
        if candidate.snapshot.phase != PlanPhase::Executing {
            return Err(PlanError::WrongPhase {
                actual: candidate.snapshot.phase,
                operation: "start plan attempt",
            });
        }
        if candidate.snapshot.scheduler_status != PlanSchedulerStatus::Active
            || !candidate.ready_node_ids_at(now_ms).contains(node_id)
        {
            return Err(PlanError::NodeNotReady {
                node_id: node_id.clone(),
            });
        }
        if candidate
            .snapshot
            .leases
            .iter()
            .any(|lease| lease.node_id == *node_id && lease.status == PlanLeaseStatus::Live)
        {
            return Err(PlanError::LiveLeaseExists {
                node_id: node_id.clone(),
            });
        }
        if candidate.snapshot.attempts.iter().any(|attempt| {
            attempt.outcome.is_none() && attempt.executor_session_id == actor.executor_session_id
        }) {
            return Err(PlanError::ActiveAttemptExistsForExecutor {
                executor_session_id: actor.executor_session_id,
            });
        }

        let revision = candidate.advance_revision("plan attempt started")?;
        let node = candidate
            .snapshot
            .nodes
            .iter_mut()
            .find(|node| &node.id == node_id)
            .expect("ready node came from snapshot");
        node.status = PlanNodeStatus::InProgress;
        node.updated_revision = revision;
        let harness_fingerprint = stable_content_hash(
            &serde_json::to_vec(&node.harness).expect("validated harness serializes"),
        );
        let node_revision = node.updated_revision;
        let attempt_id =
            PlanAttemptId::new(&format!("plan-attempt-{}", candidate.next_attempt_sequence))
                .expect("runtime-generated attempt id is valid");
        candidate.next_attempt_sequence += 1;
        let lease_id = create_subagent_lease.then(|| {
            let lease_id =
                PlanLeaseId::new(&format!("plan-lease-{}", candidate.next_lease_sequence))
                    .expect("runtime-generated lease id is valid");
            candidate.next_lease_sequence += 1;
            lease_id
        });
        let attempt = PlanAttemptSnapshot {
            attempt_id: attempt_id.clone(),
            node_id: node_id.clone(),
            node_revision,
            lease_id: lease_id.clone(),
            executor_session_id: actor.executor_session_id.clone(),
            harness_fingerprint,
            started_at_ms: now_ms,
            finished_at_ms: None,
            outcome: None,
            result: None,
            diagnostic: None,
            latest_checkpoint_ref: None,
            last_applied_directive_sequence: 0,
        };
        let lease = lease_id.map(|lease_id| PlanLeaseSnapshot {
            lease_id,
            attempt_id: attempt_id.clone(),
            node_id: node_id.clone(),
            node_revision,
            executor_session_id: actor.executor_session_id,
            started_at_ms: now_ms,
            last_heartbeat_at_ms: now_ms,
            lease_expires_at_ms: now_ms.saturating_add(
                candidate
                    .snapshot
                    .resource_policy_snapshot
                    .subagent_heartbeat_ttl_ms,
            ),
            status: PlanLeaseStatus::Live,
        });
        let progress = PlanAttemptProgressSnapshot {
            attempt_id,
            node_id: node_id.clone(),
            elapsed_ms: 0,
            model_turns: 0,
            reported_usage: None,
            last_subagent_heartbeat_at_ms: lease.as_ref().map(|_| now_ms),
            last_runtime_activity_at_ms: now_ms,
            last_durable_progress_at_ms: None,
            provider_request_in_flight: false,
            tool_call_in_flight: false,
            observable_side_effects: 0,
            artifacts_created: 0,
            artifact_refs: Vec::new(),
            changed_paths: Vec::new(),
            acceptance_evidence: Vec::new(),
            repeated_failure_fingerprint: None,
            summary: None,
            next_action: None,
            request_coordinator_review: false,
        };
        candidate.snapshot.attempts.push(attempt.clone());
        if let Some(lease) = lease.as_ref() {
            candidate.snapshot.leases.push(lease.clone());
        }
        candidate.snapshot.attempt_progress.push(progress.clone());
        let snapshot = candidate.snapshot.clone();
        *self = candidate;
        Ok(PlanAttemptStartRecords {
            snapshot,
            attempt,
            lease,
            progress,
        })
    }

    pub(crate) fn enter_execution(
        &mut self,
        envelope: PlanCapabilityEnvelopeSnapshot,
        authorization_refs: Vec<String>,
    ) -> Result<PlanSnapshot, PlanError> {
        if self.snapshot.root_node_id.is_none() {
            return Err(PlanError::EmptyPlan);
        }
        let mut candidate = self.clone();
        candidate.snapshot.authorized_capability_envelope = Some(envelope);
        let root_id = candidate
            .snapshot
            .root_node_id
            .clone()
            .expect("checked root");
        let nodes = candidate
            .snapshot
            .nodes
            .iter()
            .map(|node| (node.id.clone(), node.clone()))
            .collect::<BTreeMap<_, _>>();
        validation::validate_authorized_envelope(
            &nodes,
            &root_id,
            candidate.snapshot.authorized_capability_envelope.as_ref(),
        )?;
        candidate.snapshot.execution_authorization_refs = authorization_refs;
        candidate.snapshot.phase = PlanPhase::Executing;
        candidate.snapshot.execution_contract_fingerprint = Some(candidate.contract_fingerprint());
        candidate.advance_revision("plan execution authorized")?;
        let snapshot = candidate.snapshot.clone();
        *self = candidate;
        Ok(snapshot)
    }

    fn node_execution_shape_is_ready(&self, node: &merry_core::PlanNodeSnapshot) -> bool {
        let children = self
            .snapshot
            .nodes
            .iter()
            .filter(|candidate| {
                candidate.parent_id.as_ref() == Some(&node.id)
                    && candidate.status != PlanNodeStatus::Superseded
            })
            .collect::<Vec<_>>();
        if children.is_empty() {
            return node.status == PlanNodeStatus::Pending;
        }
        node.status == PlanNodeStatus::Verifying
            && children
                .iter()
                .all(|child| child.status == PlanNodeStatus::Completed)
    }

    fn node_order_key(&self, node_id: &PlanNodeId) -> Vec<u16> {
        let mut order = Vec::new();
        let mut cursor = self.snapshot.nodes.iter().find(|node| &node.id == node_id);
        while let Some(node) = cursor {
            order.push(node.sibling_order);
            cursor = node.parent_id.as_ref().and_then(|parent_id| {
                self.snapshot
                    .nodes
                    .iter()
                    .find(|node| &node.id == parent_id)
            });
        }
        order.reverse();
        order
    }

    pub(super) fn refresh_parent_states(&mut self, revision: u64) {
        loop {
            let mut updates = Vec::new();
            for node in &self.snapshot.nodes {
                if !matches!(
                    node.status,
                    PlanNodeStatus::Pending | PlanNodeStatus::Expanded | PlanNodeStatus::Verifying
                ) {
                    continue;
                }
                let children = self
                    .snapshot
                    .nodes
                    .iter()
                    .filter(|child| {
                        child.parent_id.as_ref() == Some(&node.id)
                            && child.status != PlanNodeStatus::Superseded
                    })
                    .collect::<Vec<_>>();
                if children.is_empty()
                    || !children.iter().all(|child| {
                        matches!(
                            child.status,
                            PlanNodeStatus::Completed
                                | PlanNodeStatus::Blocked
                                | PlanNodeStatus::Failed
                        )
                    })
                {
                    continue;
                }
                let status = if children
                    .iter()
                    .all(|child| child.status == PlanNodeStatus::Completed)
                {
                    let is_root = self.snapshot.root_node_id.as_ref() == Some(&node.id);
                    let can_auto_complete_root = is_root
                        && self.snapshot.phase == PlanPhase::Executing
                        && self.snapshot.scheduler_status == PlanSchedulerStatus::Active
                        && !self.node_has_live_execution(&node.id);
                    if can_auto_complete_root {
                        PlanNodeStatus::Completed
                    } else {
                        PlanNodeStatus::Verifying
                    }
                } else {
                    PlanNodeStatus::Blocked
                };
                if node.status != status
                    || (status == PlanNodeStatus::Completed && node.result.is_none())
                {
                    updates.push((node.id.clone(), status));
                }
            }
            if updates.is_empty() {
                break;
            }
            for (node_id, status) in updates {
                let node = self
                    .snapshot
                    .nodes
                    .iter_mut()
                    .find(|node| node.id == node_id)
                    .expect("parent update node exists");
                node.status = status;
                node.updated_revision = revision;
                if status == PlanNodeStatus::Completed && node.result.is_none() {
                    node.result = Some(PlanNodeResult {
                        conclusion: "All declared child work completed.".to_owned(),
                        evidence_refs: Vec::new(),
                        artifact_refs: Vec::new(),
                        changed_paths: Vec::new(),
                        verification: vec![
                            "Every declared child node reached completed status.".to_owned(),
                        ],
                        open_questions: Vec::new(),
                    });
                }
            }
        }
        self.refresh_terminal_phase();
    }

    fn node_has_live_execution(&self, node_id: &PlanNodeId) -> bool {
        self.snapshot
            .attempts
            .iter()
            .any(|attempt| &attempt.node_id == node_id && attempt.outcome.is_none())
            || self
                .snapshot
                .leases
                .iter()
                .any(|lease| &lease.node_id == node_id && lease.status == PlanLeaseStatus::Live)
            || self.snapshot.nodes.iter().any(|node| {
                &node.id == node_id
                    && node
                        .links
                        .iter()
                        .any(|link| link.status == merry_core::PlanLinkStatus::Active)
            })
    }

    pub(super) fn refresh_terminal_phase(&mut self) {
        let Some(root_id) = self.snapshot.root_node_id.as_ref() else {
            return;
        };
        let root_status = self
            .snapshot
            .nodes
            .iter()
            .find(|node| &node.id == root_id)
            .map(|node| node.status);
        self.snapshot.phase = match root_status {
            Some(PlanNodeStatus::Completed) => PlanPhase::Completed,
            Some(PlanNodeStatus::Blocked | PlanNodeStatus::Failed) => PlanPhase::Blocked,
            _ => self.snapshot.phase,
        };
    }

    pub(super) fn settle_draining_phase(&mut self) {
        if self.snapshot.scheduler_status == PlanSchedulerStatus::Draining
            && self
                .snapshot
                .attempts
                .iter()
                .all(|attempt| attempt.outcome.is_some())
        {
            self.snapshot.phase = PlanPhase::Cancelled;
        }
    }

    pub(super) fn contract_fingerprint(&self) -> String {
        #[derive(serde::Serialize)]
        struct Contract<'a> {
            root_objective: Option<&'a str>,
            root_acceptance: Option<&'a [String]>,
            envelope: Option<&'a PlanCapabilityEnvelopeSnapshot>,
        }
        let root = self
            .snapshot
            .root_node_id
            .as_ref()
            .and_then(|root_id| self.snapshot.nodes.iter().find(|node| &node.id == root_id));
        let bytes = serde_json::to_vec(&Contract {
            root_objective: root.map(|root| root.objective.as_str()),
            root_acceptance: root.map(|root| root.acceptance.as_slice()),
            envelope: self.snapshot.authorized_capability_envelope.as_ref(),
        })
        .expect("execution contract serializes");
        stable_content_hash(&bytes)
    }

    pub(super) fn advance_revision(&mut self, summary: &str) -> Result<u64, PlanError> {
        let mut snapshot = self.snapshot.clone();
        snapshot.revision = snapshot.revision.saturating_add(1);
        Self::append_revision_summary(&mut snapshot, summary)?;
        validation::validate_snapshot_limits(&snapshot)?;
        let revision = snapshot.revision;
        self.snapshot = snapshot;
        Ok(revision)
    }

    fn append_revision_summary(
        snapshot: &mut PlanSnapshot,
        summary: &str,
    ) -> Result<(), PlanError> {
        let revision_summary =
            PlanRevisionSummary::new(snapshot.revision, summary).map_err(|_| {
                PlanError::InvalidText {
                    field: "revision_summary",
                    reason: "is invalid",
                }
            })?;
        snapshot.revision_summaries.push(revision_summary);
        if snapshot.revision_summaries.len() > 32 {
            snapshot.revision_summaries.remove(0);
        }
        Ok(())
    }
}
