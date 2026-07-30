mod report_validation;

use super::{
    PlanError, PlanState,
    protocol::{ControlPlanAttemptInput, ReportPlanAttemptInput, ReportPlanProgressInput},
    recovery::retry_backoff_elapsed,
    validation,
};
use crate::context::stable_content_hash;
use merry_core::{
    CoordinatorDirectiveSnapshot, PlanAttemptId, PlanAttemptOutcome, PlanAttemptProgressSnapshot,
    PlanAttemptSnapshot, PlanCapabilityEnvelopeSnapshot, PlanDirectiveId, PlanDirectiveStatus,
    PlanLeaseId, PlanLeaseSnapshot, PlanLeaseStatus, PlanNodeId, PlanNodeResult, PlanNodeStatus,
    PlanPhase, PlanRevisionSummary, PlanSchedulerStatus, PlanSnapshot, SessionId,
};
use report_validation::validate_attempt_report_contract;
use std::collections::{BTreeMap, BTreeSet};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanDirectiveOutput {
    pub(crate) snapshot: PlanSnapshot,
    pub(crate) directive: CoordinatorDirectiveSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanDirectiveDeliveryOutput {
    pub(crate) snapshot: PlanSnapshot,
    pub(crate) updated_directives: Vec<CoordinatorDirectiveSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanProgressOutput {
    pub(crate) snapshot: PlanSnapshot,
    pub(crate) progress: PlanAttemptProgressSnapshot,
    pub(crate) updated_directives: Vec<CoordinatorDirectiveSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanAttemptReportOutput {
    pub(crate) snapshot: PlanSnapshot,
    pub(crate) attempt: PlanAttemptSnapshot,
    pub(crate) updated_directives: Vec<CoordinatorDirectiveSnapshot>,
    pub(crate) ready_node_ids: Vec<PlanNodeId>,
    pub(crate) client_key_to_runtime_node_id: BTreeMap<String, PlanNodeId>,
    pub(crate) previous_phase: PlanPhase,
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

    pub(crate) fn issue_directive(
        &mut self,
        input: ControlPlanAttemptInput,
        now_ms: u64,
    ) -> Result<PlanDirectiveOutput, PlanError> {
        validation::validate_reason(&input.reason)?;
        if let Some(instruction) = input.instruction.as_deref() {
            validation::validate_reason(instruction)?;
        }
        if input.requested_output.len() > validation::MAX_ACCEPTANCE_ITEMS {
            return Err(PlanError::TooManyAcceptanceItems {
                actual: input.requested_output.len(),
                maximum: validation::MAX_ACCEPTANCE_ITEMS,
            });
        }
        for requested in &input.requested_output {
            validation::validate_reason(requested)?;
        }

        let mut candidate = self.clone();
        let attempt = candidate
            .snapshot
            .attempts
            .iter()
            .find(|attempt| attempt.attempt_id == input.attempt_id)
            .ok_or_else(|| PlanError::UnknownAttempt {
                attempt_id: input.attempt_id.clone(),
            })?;
        if attempt.outcome.is_some() {
            return Err(PlanError::AttemptAlreadyResolved {
                attempt_id: input.attempt_id,
            });
        }
        let expected_lease_id = attempt
            .lease_id
            .as_ref()
            .ok_or(PlanError::StaleDirectiveTarget)?;
        let lease = candidate
            .snapshot
            .leases
            .iter()
            .find(|lease| &lease.lease_id == expected_lease_id)
            .ok_or(PlanError::StaleDirectiveTarget)?;
        if lease.status != PlanLeaseStatus::Live
            || lease.attempt_id != attempt.attempt_id
            || lease.node_revision != attempt.node_revision
        {
            return Err(PlanError::StaleDirectiveTarget);
        }

        let directive_id = PlanDirectiveId::new(&format!(
            "plan-directive-{}",
            candidate.next_directive_sequence
        ))
        .expect("runtime-generated directive id is valid");
        let sequence = candidate.next_directive_sequence;
        candidate.next_directive_sequence += 1;
        let directive = CoordinatorDirectiveSnapshot {
            directive_id,
            sequence,
            plan_id: candidate.snapshot.plan_id.clone(),
            node_id: attempt.node_id.clone(),
            node_revision: attempt.node_revision,
            attempt_id: attempt.attempt_id.clone(),
            lease_id: lease.lease_id.clone(),
            kind: input.kind,
            reason: input.reason,
            instruction: input.instruction,
            constraints: input.constraints.unwrap_or_default(),
            requested_output: input.requested_output,
            issued_at_ms: now_ms,
            status: PlanDirectiveStatus::Queued,
            delivered_at_ms: None,
            acknowledged_at_ms: None,
            applied_at_ms: None,
        };
        candidate.snapshot.directives.push(directive.clone());
        candidate.advance_revision("coordinator directive issued")?;
        let snapshot = candidate.snapshot.clone();
        *self = candidate;
        Ok(PlanDirectiveOutput {
            snapshot,
            directive,
        })
    }

    pub(crate) fn deliver_queued_directives(
        &mut self,
        actor: &PlanAttemptActor,
        lease_id: &PlanLeaseId,
        now_ms: u64,
    ) -> Result<PlanDirectiveDeliveryOutput, PlanError> {
        let mut candidate = self.clone();
        let expected_node_revision = candidate
            .snapshot
            .leases
            .iter()
            .find(|lease| &lease.lease_id == lease_id)
            .ok_or_else(|| PlanError::UnknownLease {
                lease_id: lease_id.clone(),
            })?
            .node_revision;
        let (attempt_index, _) =
            candidate.validate_live_lease(actor, lease_id, expected_node_revision)?;
        let attempt_id = candidate.snapshot.attempts[attempt_index]
            .attempt_id
            .clone();
        let mut updated_directives = Vec::new();
        for directive in candidate
            .snapshot
            .directives
            .iter_mut()
            .filter(|directive| {
                directive.attempt_id == attempt_id
                    && directive.status == PlanDirectiveStatus::Queued
            })
        {
            directive.status = PlanDirectiveStatus::Delivered;
            directive.delivered_at_ms = Some(now_ms);
            updated_directives.push(directive.clone());
        }
        if !updated_directives.is_empty() {
            candidate.advance_revision("coordinator directives delivered")?;
        }
        let snapshot = candidate.snapshot.clone();
        if !updated_directives.is_empty() {
            *self = candidate;
        }
        Ok(PlanDirectiveDeliveryOutput {
            snapshot,
            updated_directives,
        })
    }

    pub(crate) fn report_progress(
        &mut self,
        actor: &PlanAttemptActor,
        input: ReportPlanProgressInput,
        now_ms: u64,
    ) -> Result<PlanProgressOutput, PlanError> {
        validation::validate_reason(&input.summary)?;
        if let Some(next_action) = input.next_action.as_deref() {
            validation::validate_reason(next_action)?;
        }
        if let Some(checkpoint_ref) = input.checkpoint_ref.as_deref() {
            validation::validate_reason(checkpoint_ref)?;
        }
        let mut candidate = self.clone();
        let (attempt_index, _) = candidate.validate_current_attempt(actor)?;
        let attempt_id = candidate.snapshot.attempts[attempt_index]
            .attempt_id
            .clone();
        let updated_directives = candidate.apply_directive_reports(
            &attempt_id,
            &input.acknowledged_directive_ids,
            &input.applied_directive_ids,
            now_ms,
        )?;
        if let Some(checkpoint_ref) = input.checkpoint_ref.clone() {
            candidate.snapshot.attempts[attempt_index].latest_checkpoint_ref = Some(checkpoint_ref);
        }
        let started_at_ms = candidate.snapshot.attempts[attempt_index].started_at_ms;
        let progress = candidate
            .snapshot
            .attempt_progress
            .iter_mut()
            .find(|progress| progress.attempt_id == attempt_id)
            .expect("live attempt has progress state");
        progress.elapsed_ms = now_ms.saturating_sub(started_at_ms);
        progress.last_runtime_activity_at_ms = now_ms;
        progress.last_durable_progress_at_ms = Some(now_ms);
        progress.artifacts_created = progress
            .artifacts_created
            .saturating_add(input.artifact_refs.len());
        for artifact in input.artifact_refs {
            if !progress.artifact_refs.contains(&artifact) {
                progress.artifact_refs.push(artifact);
            }
        }
        for evidence in input.evidence_refs {
            if !progress.acceptance_evidence.contains(&evidence) {
                progress.acceptance_evidence.push(evidence);
            }
        }
        progress.summary = Some(input.summary);
        progress.next_action = input.next_action;
        progress.request_coordinator_review = input.request_coordinator_review.unwrap_or(false);
        let progress = progress.clone();
        candidate.advance_revision("attempt progress reported")?;
        let snapshot = candidate.snapshot.clone();
        *self = candidate;
        Ok(PlanProgressOutput {
            snapshot,
            progress,
            updated_directives,
        })
    }

    pub(crate) fn record_runtime_effect(
        &mut self,
        actor: &PlanAttemptActor,
        changed_paths: Vec<String>,
        now_ms: u64,
    ) -> Result<PlanProgressOutput, PlanError> {
        for path in &changed_paths {
            if !crate::workspace_scope::is_valid_workspace_scope(std::path::Path::new(path)) {
                return Err(PlanError::InvalidScopePath {
                    node_id: self
                        .snapshot
                        .attempts
                        .iter()
                        .find(|attempt| {
                            attempt.outcome.is_none()
                                && attempt.executor_session_id == actor.executor_session_id
                        })
                        .map(|attempt| attempt.node_id.clone())
                        .unwrap_or_else(|| {
                            PlanNodeId::new("unknown-plan-node")
                                .expect("static fallback node id is valid")
                        }),
                    path: path.clone(),
                });
            }
        }
        let mut candidate = self.clone();
        let (attempt_index, _) = candidate.validate_current_attempt(actor)?;
        let attempt_id = candidate.snapshot.attempts[attempt_index]
            .attempt_id
            .clone();
        let started_at_ms = candidate.snapshot.attempts[attempt_index].started_at_ms;
        let progress = candidate
            .snapshot
            .attempt_progress
            .iter_mut()
            .find(|progress| progress.attempt_id == attempt_id)
            .expect("live attempt has progress state");
        progress.elapsed_ms = now_ms.saturating_sub(started_at_ms);
        progress.last_runtime_activity_at_ms = now_ms;
        progress.observable_side_effects = progress.observable_side_effects.saturating_add(1);
        for path in changed_paths {
            if !progress.changed_paths.contains(&path) {
                progress.changed_paths.push(path);
            }
        }
        let progress = progress.clone();
        candidate.advance_revision("runtime-observed attempt effect recorded")?;
        let snapshot = candidate.snapshot.clone();
        *self = candidate;
        Ok(PlanProgressOutput {
            snapshot,
            progress,
            updated_directives: Vec::new(),
        })
    }

    pub(crate) fn report_attempt(
        &mut self,
        actor: &PlanAttemptActor,
        input: ReportPlanAttemptInput,
        now_ms: u64,
    ) -> Result<PlanAttemptReportOutput, PlanError> {
        validate_attempt_report_contract(&input)?;
        let mut candidate = self.clone();
        let previous_phase = candidate.snapshot.phase;
        let (attempt_index, lease_index) = candidate.validate_current_attempt(actor)?;
        let attempt_id = candidate.snapshot.attempts[attempt_index]
            .attempt_id
            .clone();
        let node_id = candidate.snapshot.attempts[attempt_index].node_id.clone();
        if input.outcome == PlanAttemptOutcome::Yielded
            && candidate.snapshot.attempts[attempt_index]
                .latest_checkpoint_ref
                .is_none()
        {
            return Err(PlanError::InvalidAttemptOutcome {
                outcome: input.outcome,
            });
        }
        let updated_directives = candidate.apply_directive_reports(
            &attempt_id,
            &input.acknowledged_directive_ids,
            &input.applied_directive_ids,
            now_ms,
        )?;
        if input.outcome == PlanAttemptOutcome::Decomposed
            && candidate.snapshot.directives.iter().any(|directive| {
                directive.attempt_id == attempt_id
                    && !matches!(
                        directive.status,
                        PlanDirectiveStatus::Superseded | PlanDirectiveStatus::Expired
                    )
                    && !directive.constraints.allow_decomposition
            })
        {
            return Err(PlanError::InvalidAttemptOutcome {
                outcome: input.outcome,
            });
        }

        let revision = candidate.snapshot.revision.saturating_add(1);
        let client_key_to_runtime_node_id = match input.decomposition {
            Some(decomposition) => {
                candidate.add_decomposition_children(&node_id, decomposition.children, revision)?
            }
            None => BTreeMap::new(),
        };
        candidate.snapshot.revision = revision;
        candidate.push_revision_summary("plan attempt finished")?;

        let result = input.result;
        let diagnostic = input.diagnostic;
        let node_status =
            candidate.node_status_after_outcome(&node_id, input.outcome, result.as_ref());
        {
            let node = candidate
                .snapshot
                .nodes
                .iter_mut()
                .find(|node| node.id == node_id)
                .expect("attempt node remains present");
            node.status = node_status;
            node.updated_revision = revision;
            if input.outcome == PlanAttemptOutcome::Completed {
                node.result = result.clone();
            }
        }
        {
            let attempt = &mut candidate.snapshot.attempts[attempt_index];
            attempt.finished_at_ms = Some(now_ms);
            attempt.outcome = Some(input.outcome);
            attempt.result = result;
            attempt.diagnostic = diagnostic;
        }
        if let Some(lease_index) = lease_index {
            candidate.snapshot.leases[lease_index].status = PlanLeaseStatus::Resolved;
        }
        if let Some(progress) = candidate
            .snapshot
            .attempt_progress
            .iter_mut()
            .find(|progress| progress.attempt_id == attempt_id)
        {
            progress.elapsed_ms =
                now_ms.saturating_sub(candidate.snapshot.attempts[attempt_index].started_at_ms);
            progress.last_runtime_activity_at_ms = now_ms;
        }
        let mut expired_directives = candidate.expire_attempt_directives(&attempt_id);
        let mut all_directives = updated_directives;
        all_directives.append(&mut expired_directives);
        candidate.refresh_parent_states(revision);
        candidate.refresh_terminal_phase();
        candidate.settle_draining_phase();
        let ready_node_ids = candidate.ready_node_ids_at(now_ms);
        let attempt = candidate.snapshot.attempts[attempt_index].clone();
        let snapshot = candidate.snapshot.clone();
        *self = candidate;
        Ok(PlanAttemptReportOutput {
            snapshot,
            attempt,
            updated_directives: all_directives,
            ready_node_ids,
            client_key_to_runtime_node_id,
            previous_phase,
        })
    }

    pub(crate) fn heartbeat(
        &mut self,
        actor: &PlanAttemptActor,
        lease_id: &PlanLeaseId,
        now_ms: u64,
        provider_request_in_flight: bool,
        tool_call_in_flight: bool,
    ) -> Result<PlanProgressOutput, PlanError> {
        let mut candidate = self.clone();
        let (attempt_index, lease_index) = candidate.validate_live_lease(
            actor,
            lease_id,
            candidate
                .snapshot
                .leases
                .iter()
                .find(|lease| &lease.lease_id == lease_id)
                .ok_or_else(|| PlanError::UnknownLease {
                    lease_id: lease_id.clone(),
                })?
                .node_revision,
        )?;
        let ttl = candidate
            .snapshot
            .resource_policy_snapshot
            .subagent_heartbeat_ttl_ms;
        let lease = &mut candidate.snapshot.leases[lease_index];
        lease.last_heartbeat_at_ms = now_ms;
        lease.lease_expires_at_ms = now_ms.saturating_add(ttl);
        let attempt_id = candidate.snapshot.attempts[attempt_index]
            .attempt_id
            .clone();
        let progress = candidate
            .snapshot
            .attempt_progress
            .iter_mut()
            .find(|progress| progress.attempt_id == attempt_id)
            .expect("live attempt has progress");
        progress.elapsed_ms = now_ms.saturating_sub(lease.started_at_ms);
        progress.last_subagent_heartbeat_at_ms = Some(now_ms);
        progress.last_runtime_activity_at_ms = now_ms;
        progress.provider_request_in_flight = provider_request_in_flight;
        progress.tool_call_in_flight = tool_call_in_flight;
        let progress = progress.clone();
        candidate.advance_revision("attempt heartbeat recorded")?;
        let snapshot = candidate.snapshot.clone();
        *self = candidate;
        Ok(PlanProgressOutput {
            snapshot,
            progress,
            updated_directives: Vec::new(),
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

    pub(super) fn expire_attempt_directives(
        &mut self,
        attempt_id: &PlanAttemptId,
    ) -> Vec<CoordinatorDirectiveSnapshot> {
        let mut expired = Vec::new();
        for directive in self
            .snapshot
            .directives
            .iter_mut()
            .filter(|directive| &directive.attempt_id == attempt_id)
        {
            if !matches!(
                directive.status,
                PlanDirectiveStatus::Applied
                    | PlanDirectiveStatus::Superseded
                    | PlanDirectiveStatus::Expired
            ) {
                directive.status = PlanDirectiveStatus::Expired;
                expired.push(directive.clone());
            }
        }
        expired
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

    fn node_status_after_outcome(
        &self,
        node_id: &PlanNodeId,
        outcome: PlanAttemptOutcome,
        _result: Option<&merry_core::PlanNodeResult>,
    ) -> PlanNodeStatus {
        match outcome {
            PlanAttemptOutcome::Completed => PlanNodeStatus::Completed,
            PlanAttemptOutcome::Decomposed => PlanNodeStatus::Expanded,
            PlanAttemptOutcome::Blocked => PlanNodeStatus::Blocked,
            PlanAttemptOutcome::SemanticFailure => PlanNodeStatus::Failed,
            PlanAttemptOutcome::TransientFailure => {
                let retry_is_safe =
                    self.snapshot
                        .nodes
                        .iter()
                        .find(|node| &node.id == node_id)
                        .is_some_and(|node| {
                            !node
                                .recovery_policy
                                .retry_only_before_observable_side_effects
                                || self
                                    .snapshot
                                    .attempts
                                    .iter()
                                    .find(|attempt| {
                                        &attempt.node_id == node_id && attempt.outcome.is_none()
                                    })
                                    .and_then(|attempt| {
                                        self.snapshot.attempt_progress.iter().find(|progress| {
                                            progress.attempt_id == attempt.attempt_id
                                        })
                                    })
                                    .is_none_or(|progress| progress.observable_side_effects == 0)
                        });
                if !retry_is_safe {
                    return PlanNodeStatus::Blocked;
                }
                let failures = self
                    .snapshot
                    .attempts
                    .iter()
                    .filter(|attempt| {
                        &attempt.node_id == node_id
                            && attempt.outcome == Some(PlanAttemptOutcome::TransientFailure)
                    })
                    .count()
                    + 1;
                let maximum = self
                    .snapshot
                    .nodes
                    .iter()
                    .find(|node| &node.id == node_id)
                    .expect("attempt node exists")
                    .recovery_policy
                    .max_transient_attempts as usize;
                if failures < maximum {
                    PlanNodeStatus::Pending
                } else {
                    PlanNodeStatus::Blocked
                }
            }
            PlanAttemptOutcome::Yielded | PlanAttemptOutcome::Interrupted => {
                PlanNodeStatus::Pending
            }
            PlanAttemptOutcome::Cancelled => PlanNodeStatus::Blocked,
        }
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
        self.snapshot.revision = self.snapshot.revision.saturating_add(1);
        self.push_revision_summary(summary)?;
        Ok(self.snapshot.revision)
    }

    fn push_revision_summary(&mut self, summary: &str) -> Result<(), PlanError> {
        let revision_summary =
            PlanRevisionSummary::new(self.snapshot.revision, summary).map_err(|_| {
                PlanError::InvalidText {
                    field: "revision_summary",
                    reason: "is invalid",
                }
            })?;
        self.snapshot.revision_summaries.push(revision_summary);
        if self.snapshot.revision_summaries.len() > 32 {
            self.snapshot.revision_summaries.remove(0);
        }
        Ok(())
    }
}
