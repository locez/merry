use crate::plan::execution::report_validation::validate_attempt_report_contract;
use crate::plan::{
    PlanError, PlanState,
    execution::PlanAttemptActor,
    protocol::{ReportPlanAttemptInput, ReportPlanProgressInput},
    validation,
};
use merry_core::{
    CoordinatorDirectiveSnapshot, PlanAttemptOutcome, PlanAttemptProgressSnapshot,
    PlanAttemptSnapshot, PlanDirectiveStatus, PlanLeaseId, PlanLeaseStatus, PlanNodeId,
    PlanNodeStatus, PlanPhase, PlanSnapshot,
};
use std::collections::BTreeMap;

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
        validation::validate_payload_items("evidence_refs", input.evidence_refs.len())?;
        validation::validate_payload_items("artifact_refs", input.artifact_refs.len())?;
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
        Self::append_revision_summary(&mut candidate.snapshot, "plan attempt finished")?;

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
        validation::validate_snapshot_limits(&candidate.snapshot)?;
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

    pub(super) fn node_status_after_outcome(
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
}
