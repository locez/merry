use super::{
    SessionState,
    artifacts::{final_output_id, tool_result_id},
    history::{ResolvedToolContinuation, ResolvedToolContinuationSnapshot},
    tool_result::ProposedToolExecutionOutcome,
};
use crate::{
    ActionExecutionEvidence, ActionProposal, RuntimeError, ToolActionKind,
    action_audit::ActionAuditPolicy,
    action_policy::ActionPolicyDecision,
    artifact::{ArtifactContent, ArtifactError},
    ledger::{LedgerFactKind, LedgerUpdateKind},
};
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, ErrorInfo, PendingToolCall, RuntimeEvent,
    RuntimeEventKind, ToolCallId, ToolCallResult, ToolCallResultStatus,
};

const WORKSPACE_READ_FILE_TOOL_NAME: &str = "workspace_read_file";

impl SessionState {
    pub(crate) fn record_final_output(
        &mut self,
        call_id: ToolCallId,
        json: String,
    ) -> Result<(crate::FinalOutput, Vec<RuntimeEvent>), RuntimeError> {
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

        let artifact_sequence = self.next_sequence();
        let artifact = ArtifactRef::new(final_output_id(artifact_sequence), ArtifactKind::Json);
        let content = ArtifactContent::json(json.clone());
        let content_bytes = content.as_bytes().len();
        let recorded = self.record_artifact_state(artifact, content)?;
        Self::trace_artifact_record(self.session_id.as_str(), &recorded, content_bytes);
        self.pending_tool_calls.remove(pending_index);
        self.resolved_tool_calls.insert(call_id.clone());

        let mut events = Vec::with_capacity(if self.session_started { 2 } else { 3 });
        if let Some(started) = self.record_session_started_if_needed() {
            events.push(started);
        }
        events.push(self.record_event(
            RuntimeEventKind::ArtifactRecorded {
                artifact: recorded.clone(),
            },
            LedgerFactKind::ArtifactRecorded,
        ));
        events.push(self.record_event(
            RuntimeEventKind::FinalOutputRecorded {
                call_id: call_id.clone(),
                artifact: recorded.clone(),
            },
            LedgerFactKind::FinalOutputRecorded,
        ));

        Ok((crate::FinalOutput::new(call_id, recorded, json), events))
    }

    pub(crate) fn pending_tool_calls(&self) -> Vec<PendingToolCall> {
        self.pending_tool_calls.clone()
    }

    pub(crate) fn pending_tool_call(&self, call_id: &ToolCallId) -> Option<PendingToolCall> {
        self.pending_tool_calls
            .iter()
            .find(|call| call.id() == call_id)
            .cloned()
    }

    pub(crate) fn has_pending_tool_calls(&self) -> bool {
        !self.pending_tool_calls.is_empty()
    }

    /// Returns tool call/result pairs not yet covered by a checkpoint.
    ///
    /// These continuations are exact provider-visible protocol history for
    /// stateless calls. They are not ledger projection; future checkpointing
    /// owns when older entries can be removed from compiled context.
    pub(crate) fn uncheckpointed_tool_continuation_snapshots(
        &self,
    ) -> Result<Vec<ResolvedToolContinuationSnapshot>, ArtifactError> {
        self.uncheckpointed_tool_continuations
            .iter()
            .map(|continuation| {
                let content = self
                    .artifacts
                    .read_content(continuation.result.artifact().id())?
                    .clone();
                Ok(ResolvedToolContinuationSnapshot::new(
                    continuation.call.clone(),
                    continuation.result.clone(),
                    content,
                ))
            })
            .collect()
    }

    pub(crate) fn record_tool_call_pending(
        &mut self,
        call: PendingToolCall,
    ) -> Result<RuntimeEvent, ErrorInfo> {
        if self
            .pending_tool_calls
            .iter()
            .any(|pending| pending.id() == call.id())
        {
            return Err(duplicate_tool_call_diagnostic(call.id(), "already pending"));
        }

        if self.resolved_tool_calls.contains(call.id()) {
            return Err(duplicate_tool_call_diagnostic(
                call.id(),
                "already resolved",
            ));
        }

        self.pending_tool_calls.push(call.clone());
        Ok(self.record_event(
            RuntimeEventKind::ToolCallPending { call },
            LedgerFactKind::ToolCallPending,
        ))
    }

    pub(crate) fn record_bridge_tool_call_requested(
        &mut self,
        call: PendingToolCall,
    ) -> RuntimeEvent {
        self.record_event(
            RuntimeEventKind::BridgeToolCallRequested { call },
            LedgerFactKind::BridgeToolCallRequested,
        )
    }

    pub(crate) fn submit_tool_result(
        &mut self,
        result: ToolCallResult,
        content: ArtifactContent,
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
        let Some(pending_index) = self
            .pending_tool_calls
            .iter()
            .position(|call| call.id() == result.call_id())
        else {
            return if self.resolved_tool_calls.contains(result.call_id()) {
                Err(RuntimeError::ToolCallAlreadyResolved {
                    session_id: self.session_id.clone(),
                    call_id: result.call_id().clone(),
                })
            } else {
                Err(RuntimeError::UnknownToolCall {
                    session_id: self.session_id.clone(),
                    call_id: result.call_id().clone(),
                })
            };
        };

        self.validate_tool_result_content(&result, &content)?;
        let content_bytes = content.as_bytes().len();
        let recorded = self.record_artifact_state(result.artifact().clone(), content)?;
        Self::trace_artifact_record(self.session_id.as_str(), &recorded, content_bytes);
        debug_assert_eq!(&recorded, result.artifact());

        let pending = self.pending_tool_calls.remove(pending_index);
        let pending_for_skill_event = pending.clone();
        self.resolved_tool_calls.insert(result.call_id().clone());
        let history_id = self.next_history_id();
        self.uncheckpointed_tool_continuations
            .push(ResolvedToolContinuation::new(
                history_id,
                pending,
                result.clone(),
            ));

        let mut events = Vec::with_capacity(if self.session_started { 2 } else { 3 });
        if let Some(started) = self.record_session_started_if_needed() {
            events.push(started);
        }

        events.push(self.record_event(
            RuntimeEventKind::ArtifactRecorded { artifact: recorded },
            LedgerFactKind::ArtifactRecorded,
        ));
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

    pub(crate) fn submit_tool_execution_outcome(
        &mut self,
        call_id: &ToolCallId,
        status: ToolCallResultStatus,
        content: ArtifactContent,
        diagnostic: Option<ErrorInfo>,
        execution_evidence: Option<ActionExecutionEvidence>,
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
        debug_assert!(execution_evidence.is_none());
        let artifact_kind = match &content {
            ArtifactContent::Text(_) => ArtifactKind::Text,
            ArtifactContent::Json(_) => ArtifactKind::Json,
            ArtifactContent::Binary(_) | ArtifactContent::Image(_) | ArtifactContent::Other(_) => {
                return Err(RuntimeError::UnsupportedToolResultContent {
                    artifact_id: self.next_tool_result_artifact_id(),
                    content_kind: content.kind(),
                });
            }
        };
        let artifact = ArtifactRef::new(self.next_tool_result_artifact_id(), artifact_kind);
        let result = match status {
            ToolCallResultStatus::Succeeded => ToolCallResult::new(
                call_id.clone(),
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
                ToolCallResult::failed(call_id.clone(), artifact, diagnostic)
            }
        };

        self.submit_tool_result(result, content)
    }

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

        let artifact_kind = match &content {
            ArtifactContent::Text(_) => ArtifactKind::Text,
            ArtifactContent::Json(_) => ArtifactKind::Json,
            ArtifactContent::Binary(_) | ArtifactContent::Image(_) | ArtifactContent::Other(_) => {
                return Err(RuntimeError::UnsupportedToolResultContent {
                    artifact_id: self.next_tool_result_artifact_id(),
                    content_kind: content.kind(),
                });
            }
        };
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
        self.resolved_tool_calls.insert(result.call_id().clone());
        let history_id = self.next_history_id();
        self.uncheckpointed_tool_continuations
            .push(ResolvedToolContinuation::new(
                history_id,
                pending,
                result.clone(),
            ));

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

        let artifact_kind = match &content {
            ArtifactContent::Text(_) => ArtifactKind::Text,
            ArtifactContent::Json(_) => ArtifactKind::Json,
            ArtifactContent::Binary(_) | ArtifactContent::Image(_) | ArtifactContent::Other(_) => {
                return Err(RuntimeError::UnsupportedToolResultContent {
                    artifact_id: self.next_tool_result_artifact_id(),
                    content_kind: content.kind(),
                });
            }
        };
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
            self.resolved_tool_calls.insert(result.call_id().clone());
            let history_id = self.next_history_id();
            self.uncheckpointed_tool_continuations
                .push(ResolvedToolContinuation::new(
                    history_id,
                    pending,
                    result.clone(),
                ));
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
        self.resolved_tool_calls.insert(result.call_id().clone());
        let history_id = self.next_history_id();
        self.uncheckpointed_tool_continuations
            .push(ResolvedToolContinuation::new(
                history_id,
                pending,
                result.clone(),
            ));
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

    fn skill_used_event_for_read(
        &mut self,
        pending: &PendingToolCall,
        result: &ToolCallResult,
    ) -> Option<RuntimeEvent> {
        if result.status() != ToolCallResultStatus::Succeeded {
            return None;
        }
        if pending.name().as_str() != WORKSPACE_READ_FILE_TOOL_NAME {
            return None;
        }
        let path = pending
            .arguments()
            .as_object()
            .get("path")
            .and_then(serde_json::Value::as_str)?;
        let (skill_name, skill_md_path) = {
            let skill = self.skill_catalog.as_ref()?.find_by_skill_md_path(path)?;
            (
                skill.name().to_owned(),
                skill.skill_md_path().display().to_string(),
            )
        };

        Some(self.record_event(
            RuntimeEventKind::SkillUsed {
                skill_name,
                skill_md_path,
                tool_call_id: pending.id().clone(),
                artifact: result.artifact().clone(),
            },
            LedgerFactKind::SkillUsed,
        ))
    }

    fn record_tool_result_observation(&mut self, observation: LedgerUpdateKind) {
        self.ledger.append(observation);
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

    fn next_tool_result_artifact_id(&self) -> ArtifactId {
        tool_result_id(self.next_sequence())
    }

    fn validate_tool_result_content(
        &self,
        result: &ToolCallResult,
        content: &ArtifactContent,
    ) -> Result<(), RuntimeError> {
        let supported = matches!(content, ArtifactContent::Text(_) | ArtifactContent::Json(_));
        let compatible = matches!(
            (result.artifact().kind(), content),
            (ArtifactKind::Text, ArtifactContent::Text(_))
                | (ArtifactKind::Json, ArtifactContent::Json(_))
        );

        if !supported {
            return Err(RuntimeError::UnsupportedToolResultContent {
                artifact_id: result.artifact().id().clone(),
                content_kind: content.kind(),
            });
        }

        if !compatible {
            return Err(ArtifactError::IncompatibleContent {
                id: result.artifact().id().clone(),
                artifact_kind: result.artifact().kind().clone(),
                content_kind: content.kind(),
            }
            .into());
        }

        match content {
            ArtifactContent::Text(text) | ArtifactContent::Json(text) if text.trim().is_empty() => {
                Err(RuntimeError::UnsupportedToolResultContent {
                    artifact_id: result.artifact().id().clone(),
                    content_kind: content.kind(),
                })
            }
            ArtifactContent::Text(_) | ArtifactContent::Json(_) => Ok(()),
            ArtifactContent::Binary(_) | ArtifactContent::Image(_) | ArtifactContent::Other(_) => {
                unreachable!("unsupported tool result content is rejected before blank validation")
            }
        }
    }
}

fn duplicate_tool_call_diagnostic(call_id: &ToolCallId, state: &'static str) -> ErrorInfo {
    ErrorInfo::new(
        "tool_call_duplicate",
        &format!("tool call {call_id} is {state}; duplicate pending admission rejected"),
    )
    .expect("duplicate tool call diagnostic uses static code and validated call id")
}
