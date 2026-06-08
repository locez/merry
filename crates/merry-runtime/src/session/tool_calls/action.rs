use super::SessionState;
use crate::{
    ActionExecutionEvidence, ActionProposal, RuntimeError, ToolActionKind,
    action_audit::ActionAuditPolicy, action_policy::ActionPolicyDecision,
    artifact::ArtifactContent, ledger::LedgerFactKind,
    session::tool_result::ProposedToolExecutionOutcome,
};
use merry_core::{
    ArtifactRef, ErrorInfo, PendingToolCall, RuntimeEvent, RuntimeEventKind, ToolCallResult,
    ToolCallResultStatus,
};

impl SessionState {
    pub(crate) fn submit_proposed_tool_execution_outcome(
        &mut self,
        proposal: ActionProposal,
        status: ToolCallResultStatus,
        content: ArtifactContent,
        diagnostic: Option<ErrorInfo>,
        execution_evidence: Option<ActionExecutionEvidence>,
        policy: ActionAuditPolicy,
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
        self.submit_proposed_tool_execution_outcome_record(ProposedToolExecutionOutcome::new(
            proposal,
            status,
            content,
            diagnostic,
            execution_evidence,
            policy,
        ))
    }

    pub(crate) fn submit_proposed_tool_execution_outcome_record(
        &mut self,
        outcome: ProposedToolExecutionOutcome,
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
        let ProposedToolExecutionOutcome {
            proposal,
            status,
            content,
            diagnostic,
            execution_evidence,
            policy,
            observation,
        } = outcome;
        let call_id = proposal.tool_call_id().clone();
        let Some(pending_index) = self
            .pending_tool_calls
            .iter()
            .position(|call| call.id() == &call_id)
        else {
            return if self.resolved_tool_calls.contains(&call_id) {
                Err(RuntimeError::ToolCallAlreadyResolved {
                    session_id: self.session_id.clone(),
                    call_id,
                })
            } else {
                Err(RuntimeError::UnknownToolCall {
                    session_id: self.session_id.clone(),
                    call_id,
                })
            };
        };

        let artifact_kind = self.tool_result_artifact_kind(&content)?;
        let artifact = ArtifactRef::new(self.next_tool_result_artifact_id(), artifact_kind);
        let result = match status {
            ToolCallResultStatus::Succeeded => ToolCallResult::new(
                call_id,
                ToolCallResultStatus::Succeeded,
                artifact,
                diagnostic,
            )?,
            ToolCallResultStatus::Failed => {
                let diagnostic = diagnostic.ok_or(RuntimeError::Core {
                    source: merry_core::CoreError::InvalidToolCallResult {
                        reason: "failed tool execution outcome must include a diagnostic",
                    },
                })?;
                ToolCallResult::failed(call_id, artifact, diagnostic)
            }
        };
        let observation =
            observation.map(|observation| observation.into_update_for_artifact(result.artifact()));

        self.validate_tool_result_content(&result, &content)?;
        self.artifacts
            .ensure_recordable(result.artifact(), &content)?;

        let pending = self.pending_tool_calls.remove(pending_index);
        let pending_for_skill_event = pending.clone();
        let mut events = Vec::with_capacity(if self.session_started { 2 } else { 3 });
        if let Some(started) = self.record_session_started_if_needed() {
            events.push(started);
        }

        let action_kind = proposal.action_kind();
        self.record_proposed_tool_action_audit(proposal);
        let content_bytes = content.as_bytes().len();
        if let Some(execution_evidence) = execution_evidence {
            self.record_executed_tool_action_audit(
                &pending,
                action_kind,
                policy,
                execution_evidence,
            );
        }
        let recorded = self
            .artifacts
            .record_preflighted(result.artifact().clone(), content);
        Self::trace_artifact_record(self.session_id.as_str(), &recorded, content_bytes);
        debug_assert_eq!(&recorded, result.artifact());
        self.transcript.push_tool_result(
            result.call_id().clone(),
            result.clone(),
            result.artifact().id().clone(),
        )?;
        self.resolved_tool_calls.insert(result.call_id().clone());

        events.push(self.record_event(
            RuntimeEventKind::ArtifactRecorded { artifact: recorded },
            LedgerFactKind::ArtifactRecorded,
        ));
        if let Some(observation) = observation {
            self.record_tool_result_observation(observation);
        }
        events.push(self.record_event(
            RuntimeEventKind::ToolCallResolved {
                result: result.clone(),
            },
            LedgerFactKind::ToolCallResolved,
        ));
        if let Some(event) = self.skill_used_event_for_read(&pending_for_skill_event, &result) {
            events.push(event);
        }

        Ok(events)
    }

    pub(crate) fn submit_denied_tool_action(
        &mut self,
        pending: &PendingToolCall,
        decision: &ActionPolicyDecision,
        proposal: Option<ActionProposal>,
        content: ArtifactContent,
        diagnostic: ErrorInfo,
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
        debug_assert!(!decision.is_allowed());

        let Some(pending_index) = self
            .pending_tool_calls
            .iter()
            .position(|call| call.id() == pending.id())
        else {
            return if self.resolved_tool_calls.contains(pending.id()) {
                Err(RuntimeError::ToolCallAlreadyResolved {
                    session_id: self.session_id.clone(),
                    call_id: pending.id().clone(),
                })
            } else {
                Err(RuntimeError::UnknownToolCall {
                    session_id: self.session_id.clone(),
                    call_id: pending.id().clone(),
                })
            };
        };

        let artifact_kind = self.tool_result_artifact_kind(&content)?;
        let artifact = ArtifactRef::new(self.next_tool_result_artifact_id(), artifact_kind);
        let result = ToolCallResult::failed(pending.id().clone(), artifact, diagnostic);

        self.validate_tool_result_content(&result, &content)?;
        self.artifacts
            .ensure_recordable(result.artifact(), &content)?;

        let pending = self.pending_tool_calls.remove(pending_index);
        if let Some(started) = self.record_session_started_if_needed() {
            if let Some(proposal) = proposal {
                self.record_proposed_tool_action_audit(proposal);
            }
            self.record_denied_tool_action_audit(&pending, decision);
            let content_bytes = content.as_bytes().len();
            let artifact = self
                .artifacts
                .record_preflighted(result.artifact().clone(), content);
            Self::trace_artifact_record(self.session_id.as_str(), &artifact, content_bytes);
            debug_assert_eq!(artifact, *result.artifact());
            self.transcript.push_tool_result(
                result.call_id().clone(),
                result.clone(),
                result.artifact().id().clone(),
            )?;
            self.resolved_tool_calls.insert(result.call_id().clone());
            return Ok(vec![
                started,
                self.record_event(
                    RuntimeEventKind::ArtifactRecorded { artifact },
                    LedgerFactKind::ArtifactRecorded,
                ),
                self.record_event(
                    RuntimeEventKind::ToolCallResolved { result },
                    LedgerFactKind::ToolCallResolved,
                ),
            ]);
        }

        if let Some(proposal) = proposal {
            self.record_proposed_tool_action_audit(proposal);
        }
        self.record_denied_tool_action_audit(&pending, decision);
        let content_bytes = content.as_bytes().len();
        let artifact = self
            .artifacts
            .record_preflighted(result.artifact().clone(), content);
        Self::trace_artifact_record(self.session_id.as_str(), &artifact, content_bytes);
        debug_assert_eq!(artifact, *result.artifact());
        self.transcript.push_tool_result(
            result.call_id().clone(),
            result.clone(),
            result.artifact().id().clone(),
        )?;
        self.resolved_tool_calls.insert(result.call_id().clone());
        Ok(vec![
            self.record_event(
                RuntimeEventKind::ArtifactRecorded { artifact },
                LedgerFactKind::ArtifactRecorded,
            ),
            self.record_event(
                RuntimeEventKind::ToolCallResolved { result },
                LedgerFactKind::ToolCallResolved,
            ),
        ])
    }

    pub(crate) fn record_guarded_tool_action(
        &mut self,
        pending: &PendingToolCall,
        action_kind: ToolActionKind,
        policy: ActionAuditPolicy,
    ) -> Result<(), RuntimeError> {
        debug_assert!(action_kind.is_mutating());

        if !self
            .pending_tool_calls
            .iter()
            .any(|call| call.id() == pending.id())
        {
            return if self.resolved_tool_calls.contains(pending.id()) {
                Err(RuntimeError::ToolCallAlreadyResolved {
                    session_id: self.session_id.clone(),
                    call_id: pending.id().clone(),
                })
            } else {
                Err(RuntimeError::UnknownToolCall {
                    session_id: self.session_id.clone(),
                    call_id: pending.id().clone(),
                })
            };
        }

        if self.record_guarded_tool_action_audit(pending, action_kind, policy) {
            self.ledger
                .record_lifecycle(self.next_sequence, LedgerFactKind::ActionAuditRecorded);
        }
        Ok(())
    }

    fn record_denied_tool_action_audit(
        &mut self,
        pending: &PendingToolCall,
        decision: &ActionPolicyDecision,
    ) {
        self.action_audits
            .record_denied_tool_action(pending, decision);
        self.ledger
            .record_lifecycle(self.next_sequence, LedgerFactKind::ActionAuditRecorded);
    }

    fn record_proposed_tool_action_audit(&mut self, proposal: ActionProposal) {
        self.action_audits.record_proposed_tool_action(proposal);
        self.ledger
            .record_lifecycle(self.next_sequence, LedgerFactKind::ActionAuditRecorded);
    }

    fn record_executed_tool_action_audit(
        &mut self,
        pending: &PendingToolCall,
        action_kind: ToolActionKind,
        policy: ActionAuditPolicy,
        evidence: ActionExecutionEvidence,
    ) {
        self.action_audits
            .record_executed_tool_action(pending, action_kind, policy, evidence);
        self.ledger
            .record_lifecycle(self.next_sequence, LedgerFactKind::ActionAuditRecorded);
    }

    fn record_guarded_tool_action_audit(
        &mut self,
        pending: &PendingToolCall,
        action_kind: ToolActionKind,
        policy: ActionAuditPolicy,
    ) -> bool {
        self.action_audits
            .record_guarded_tool_action(pending, action_kind, policy)
    }
}
