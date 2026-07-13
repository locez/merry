use super::{
    PlanError, PlanState,
    protocol::{
        ControlPlanAttemptInput, PlanDecompositionInput, ReportPlanAttemptInput,
        ReportPlanProgressInput,
    },
    recovery::retry_backoff_elapsed,
    validation,
};
use crate::context::stable_content_hash;
use merry_core::{
    CoordinatorDirectiveSnapshot, PlanAttemptId, PlanAttemptOutcome, PlanAttemptProgressSnapshot,
    PlanAttemptSnapshot, PlanCapabilityEnvelopeSnapshot, PlanDirectiveId, PlanDirectiveStatus,
    PlanLeaseId, PlanLeaseSnapshot, PlanLeaseStatus, PlanNodeId, PlanNodeStatus, PlanPhase,
    PlanRevisionSummary, PlanSchedulerStatus, PlanSnapshot, SessionId,
};
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
    pub(crate) client_key_ids: BTreeMap<String, PlanNodeId>,
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
        let lease_id = PlanLeaseId::new(&format!("plan-lease-{}", candidate.next_lease_sequence))
            .expect("runtime-generated lease id is valid");
        candidate.next_lease_sequence += 1;
        let expires_at_ms = now_ms.saturating_add(
            candidate
                .snapshot
                .resource_policy_snapshot
                .worker_heartbeat_ttl_ms,
        );
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
        let lease = PlanLeaseSnapshot {
            lease_id,
            attempt_id: attempt_id.clone(),
            node_id: node_id.clone(),
            node_revision,
            executor_session_id: actor.executor_session_id,
            started_at_ms: now_ms,
            last_heartbeat_at_ms: now_ms,
            lease_expires_at_ms: expires_at_ms,
            status: PlanLeaseStatus::Live,
        };
        let progress = PlanAttemptProgressSnapshot {
            attempt_id,
            node_id: node_id.clone(),
            elapsed_ms: 0,
            model_turns: 0,
            reported_usage: None,
            last_worker_heartbeat_at_ms: now_ms,
            last_runtime_activity_at_ms: now_ms,
            last_durable_progress_at_ms: None,
            provider_request_in_flight: false,
            tool_call_in_flight: false,
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
        candidate.snapshot.leases.push(lease.clone());
        candidate.snapshot.attempt_progress.push(progress.clone());
        let snapshot = candidate.snapshot.clone();
        *self = candidate;
        Ok(PlanAttemptStartOutput {
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
        let lease = candidate
            .snapshot
            .leases
            .iter()
            .find(|lease| lease.lease_id == input.expected_lease_id)
            .ok_or(PlanError::StaleDirectiveTarget)?;
        if lease.status != PlanLeaseStatus::Live
            || lease.attempt_id != attempt.attempt_id
            || lease.node_revision != input.expected_node_revision
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
        let (attempt_index, lease_index) =
            candidate.validate_live_lease(actor, &input.lease_id, input.expected_node_revision)?;
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
        let started_at_ms = candidate.snapshot.leases[lease_index].started_at_ms;
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

    pub(crate) fn report_attempt(
        &mut self,
        actor: &PlanAttemptActor,
        input: ReportPlanAttemptInput,
        now_ms: u64,
    ) -> Result<PlanAttemptReportOutput, PlanError> {
        validate_attempt_report_contract(&input)?;
        let mut candidate = self.clone();
        let previous_phase = candidate.snapshot.phase;
        let (attempt_index, lease_index) =
            candidate.validate_live_lease(actor, &input.lease_id, input.expected_node_revision)?;
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
        let client_key_ids = match input.decomposition {
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
        candidate.snapshot.leases[lease_index].status = PlanLeaseStatus::Resolved;
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
        let ready_node_ids = candidate.ready_node_ids_at(now_ms);
        let attempt = candidate.snapshot.attempts[attempt_index].clone();
        let snapshot = candidate.snapshot.clone();
        *self = candidate;
        Ok(PlanAttemptReportOutput {
            snapshot,
            attempt,
            updated_directives: all_directives,
            ready_node_ids,
            client_key_ids,
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
            .worker_heartbeat_ttl_ms;
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
        progress.last_worker_heartbeat_at_ms = now_ms;
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

    pub(super) fn validate_live_lease(
        &self,
        actor: &PlanAttemptActor,
        lease_id: &PlanLeaseId,
        expected_node_revision: u64,
    ) -> Result<(usize, usize), PlanError> {
        let lease_index = self
            .snapshot
            .leases
            .iter()
            .position(|lease| &lease.lease_id == lease_id)
            .ok_or_else(|| PlanError::UnknownLease {
                lease_id: lease_id.clone(),
            })?;
        let lease = &self.snapshot.leases[lease_index];
        if lease.status != PlanLeaseStatus::Live {
            return Err(PlanError::LeaseNotLive {
                lease_id: lease_id.clone(),
            });
        }
        let attempt_index = self
            .snapshot
            .attempts
            .iter()
            .position(|attempt| attempt.attempt_id == lease.attempt_id)
            .ok_or_else(|| PlanError::UnknownAttempt {
                attempt_id: lease.attempt_id.clone(),
            })?;
        let attempt = &self.snapshot.attempts[attempt_index];
        if attempt.outcome.is_some() {
            return Err(PlanError::AttemptAlreadyResolved {
                attempt_id: attempt.attempt_id.clone(),
            });
        }
        if attempt.executor_session_id != actor.executor_session_id
            || lease.executor_session_id != actor.executor_session_id
        {
            return Err(PlanError::AttemptOwnershipMismatch {
                attempt_id: attempt.attempt_id.clone(),
            });
        }
        if lease.node_revision != expected_node_revision {
            return Err(PlanError::AttemptNodeRevisionMismatch {
                expected: expected_node_revision,
                actual: lease.node_revision,
            });
        }
        Ok((attempt_index, lease_index))
    }

    fn apply_directive_reports(
        &mut self,
        attempt_id: &PlanAttemptId,
        acknowledged: &[PlanDirectiveId],
        applied: &[PlanDirectiveId],
        now_ms: u64,
    ) -> Result<Vec<CoordinatorDirectiveSnapshot>, PlanError> {
        let acknowledged = acknowledged.iter().cloned().collect::<BTreeSet<_>>();
        let applied = applied.iter().cloned().collect::<BTreeSet<_>>();
        let mut updated = Vec::new();
        for directive_id in acknowledged.union(&applied) {
            let directive = self
                .snapshot
                .directives
                .iter_mut()
                .find(|directive| {
                    &directive.directive_id == directive_id && &directive.attempt_id == attempt_id
                })
                .ok_or_else(|| PlanError::UnknownDirective {
                    directive_id: directive_id.clone(),
                })?;
            if acknowledged.contains(directive_id)
                && matches!(
                    directive.status,
                    PlanDirectiveStatus::Queued | PlanDirectiveStatus::Delivered
                )
            {
                directive.status = PlanDirectiveStatus::Acknowledged;
                directive.acknowledged_at_ms = Some(now_ms);
            }
            if applied.contains(directive_id) {
                if directive.status == PlanDirectiveStatus::Applied {
                    continue;
                }
                if directive.status != PlanDirectiveStatus::Acknowledged {
                    return Err(PlanError::InvalidDirectiveTransition {
                        directive_id: directive_id.clone(),
                        status: directive.status,
                        target: "applied",
                    });
                }
                directive.status = PlanDirectiveStatus::Applied;
                directive.applied_at_ms = Some(now_ms);
            }
            updated.push(directive.clone());
        }
        if let Some(max_sequence) = self
            .snapshot
            .directives
            .iter()
            .filter(|directive| {
                &directive.attempt_id == attempt_id
                    && directive.status == PlanDirectiveStatus::Applied
            })
            .map(|directive| directive.sequence)
            .max()
            && let Some(attempt) = self
                .snapshot
                .attempts
                .iter_mut()
                .find(|attempt| &attempt.attempt_id == attempt_id)
        {
            attempt.last_applied_directive_sequence = max_sequence;
        }
        Ok(updated)
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
                    PlanNodeStatus::Pending | PlanNodeStatus::Expanded
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
                    PlanNodeStatus::Verifying
                } else {
                    PlanNodeStatus::Blocked
                };
                updates.push((node.id.clone(), status));
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
            }
        }
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

fn validate_attempt_report_contract(input: &ReportPlanAttemptInput) -> Result<(), PlanError> {
    match input.outcome {
        PlanAttemptOutcome::Completed
            if input.result.is_some()
                && input.diagnostic.is_none()
                && input.decomposition.is_none() => {}
        PlanAttemptOutcome::Decomposed
            if input.result.is_none()
                && input.diagnostic.is_none()
                && input.decomposition.is_some() =>
        {
            validate_decomposition(input.decomposition.as_ref().expect("matched some"))?;
        }
        PlanAttemptOutcome::Blocked | PlanAttemptOutcome::SemanticFailure
            if input.decomposition.is_none()
                && (input.result.is_some() || input.diagnostic.is_some()) => {}
        PlanAttemptOutcome::TransientFailure
            if input.result.is_none()
                && input.diagnostic.is_some()
                && input.decomposition.is_none() => {}
        PlanAttemptOutcome::Yielded if input.result.is_none() && input.decomposition.is_none() => {}
        PlanAttemptOutcome::Cancelled | PlanAttemptOutcome::Interrupted => {
            return Err(PlanError::InvalidAttemptOutcome {
                outcome: input.outcome,
            });
        }
        _ => {
            return Err(PlanError::InvalidAttemptOutcome {
                outcome: input.outcome,
            });
        }
    }
    Ok(())
}

fn validate_decomposition(input: &PlanDecompositionInput) -> Result<(), PlanError> {
    validation::validate_reason(&input.reason)?;
    if input.children.is_empty() {
        return Err(PlanError::EmptyDecomposition);
    }
    if input
        .children
        .iter()
        .any(|child| !child.children.is_empty())
    {
        return Err(PlanError::NestedDecomposition);
    }
    Ok(())
}
