use super::SessionState;
use crate::{
    ActionExecutionEvidence, RuntimeError, ToolExecutionOutcome,
    artifact::{ArtifactContent, ArtifactError},
    ledger::{LedgerFactKind, LedgerUpdateKind},
    session::transcript::ToolResultPromptProjection,
};
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, ErrorInfo, RuntimeJournalEvent, RuntimeJournalPayload,
    ToolCallId, ToolCallResult, ToolCallResultStatus,
};
use std::collections::{BTreeMap, BTreeSet};

impl SessionState {
    pub(crate) fn record_final_output(
        &mut self,
        call_id: ToolCallId,
        json: String,
    ) -> Result<(crate::FinalOutput, Vec<RuntimeJournalEvent>), RuntimeError> {
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

        self.transcript
            .model_turn_id_for_tool_call(&call_id)
            .ok_or_else(|| RuntimeError::TranscriptToolCallMissing {
                call_id: call_id.clone(),
            })?;

        let artifact_sequence = self.next_sequence();
        let artifact = ArtifactRef::new(
            super::super::artifacts::final_output_id(artifact_sequence),
            ArtifactKind::Json,
        );
        let content = ArtifactContent::json(json.clone());
        let content_bytes = content.as_bytes().len();
        self.artifacts.ensure_recordable(&artifact, &content)?;
        let result = ToolCallResult::succeeded(call_id.clone(), artifact.clone());
        let mut transcript = self.transcript.clone();
        transcript.hide_tool_call(&call_id)?;
        transcript.push_tool_result(
            call_id.clone(),
            result,
            artifact.id().clone(),
            ToolResultPromptProjection::Hidden,
        )?;
        let recorded = self.artifacts.record_preflighted(artifact, content);
        Self::trace_artifact_record(self.session_id.as_str(), &recorded, content_bytes);
        self.transcript = transcript;
        self.pending_tool_calls.remove(pending_index);
        self.resolved_tool_calls.insert(call_id.clone());

        let mut events = Vec::with_capacity(if self.session_started { 2 } else { 3 });
        if let Some(started) = self.record_session_started_if_needed() {
            events.push(started);
        }
        events.push(self.record_event(
            RuntimeJournalPayload::ArtifactRecorded {
                artifact: recorded.clone(),
            },
            LedgerFactKind::ArtifactRecorded,
        ));
        events.push(self.record_event(
            RuntimeJournalPayload::FinalOutputRecorded {
                call_id: call_id.clone(),
                artifact: recorded.clone(),
            },
            LedgerFactKind::FinalOutputRecorded,
        ));

        Ok((crate::FinalOutput::new(call_id, recorded, json), events))
    }

    pub(crate) fn submit_tool_result(
        &mut self,
        result: ToolCallResult,
        content: ArtifactContent,
    ) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
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

        self.transcript
            .model_turn_id_for_tool_call(result.call_id())
            .ok_or_else(|| RuntimeError::TranscriptToolCallMissing {
                call_id: result.call_id().clone(),
            })?;

        self.validate_tool_result_content(&result, &content)?;
        self.artifacts
            .ensure_recordable(result.artifact(), &content)?;
        let mut transcript = self.transcript.clone();
        transcript.push_tool_result(
            result.call_id().clone(),
            result.clone(),
            result.artifact().id().clone(),
            ToolResultPromptProjection::Full,
        )?;
        let content_bytes = content.as_bytes().len();
        let recorded = self
            .artifacts
            .record_preflighted(result.artifact().clone(), content);
        Self::trace_artifact_record(self.session_id.as_str(), &recorded, content_bytes);
        debug_assert_eq!(&recorded, result.artifact());
        self.transcript = transcript;
        let pending = self.pending_tool_calls.remove(pending_index);
        let pending_for_skill_event = pending.clone();
        self.resolved_tool_calls.insert(result.call_id().clone());

        let mut events = Vec::with_capacity(if self.session_started { 2 } else { 3 });
        if let Some(started) = self.record_session_started_if_needed() {
            events.push(started);
        }

        events.push(self.record_event(
            RuntimeJournalPayload::ArtifactRecorded { artifact: recorded },
            LedgerFactKind::ArtifactRecorded,
        ));
        events.push(self.record_event(
            RuntimeJournalPayload::ToolCallResolved {
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
    ) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
        debug_assert!(execution_evidence.is_none());
        let artifact_kind = self.tool_result_artifact_kind(&content)?;
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

    pub(crate) fn submit_tool_execution_outcomes(
        &mut self,
        outcomes: Vec<(ToolCallId, ToolExecutionOutcome)>,
    ) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
        if outcomes.is_empty() {
            return Err(RuntimeError::BridgeToolResultBatchEmpty {
                session_id: self.session_id.clone(),
            });
        }

        self.validate_tool_execution_outcomes(&outcomes)?;

        let mut outcomes_by_call_id = outcomes.into_iter().collect::<BTreeMap<_, _>>();
        let ordered_outcomes = self
            .pending_tool_calls
            .iter()
            .filter_map(|call| {
                outcomes_by_call_id
                    .remove(call.id())
                    .map(|outcome| (call.id().clone(), outcome))
            })
            .collect::<Vec<_>>();
        let mut events = Vec::new();
        for (call_id, outcome) in ordered_outcomes {
            let (status, content, diagnostic, execution_evidence) = outcome.into_parts();
            let mut call_events = self.submit_tool_execution_outcome(
                &call_id,
                status,
                content,
                diagnostic,
                execution_evidence,
            )?;
            events.append(&mut call_events);
        }
        Ok(events)
    }

    fn validate_tool_execution_outcomes(
        &self,
        outcomes: &[(ToolCallId, ToolExecutionOutcome)],
    ) -> Result<(), RuntimeError> {
        let mut received_call_ids = BTreeSet::new();
        for (call_id, outcome) in outcomes {
            if !received_call_ids.insert(call_id.clone()) {
                return Err(RuntimeError::BridgeToolResultBatchMismatch {
                    session_id: self.session_id.clone(),
                    expected_call_ids: self
                        .pending_tool_calls
                        .iter()
                        .map(|call| call.id().clone())
                        .collect(),
                    received_call_ids: received_call_ids.into_iter().collect(),
                });
            }

            if !self
                .pending_tool_calls
                .iter()
                .any(|call| call.id() == call_id)
            {
                return if self.resolved_tool_calls.contains(call_id) {
                    Err(RuntimeError::ToolCallAlreadyResolved {
                        session_id: self.session_id.clone(),
                        call_id: call_id.clone(),
                    })
                } else {
                    Err(RuntimeError::UnknownToolCall {
                        session_id: self.session_id.clone(),
                        call_id: call_id.clone(),
                    })
                };
            }

            self.transcript
                .model_turn_id_for_tool_call(call_id)
                .ok_or_else(|| RuntimeError::TranscriptToolCallMissing {
                    call_id: call_id.clone(),
                })?;

            let artifact_kind = self.tool_result_artifact_kind(outcome.content())?;
            let artifact = ArtifactRef::new(self.next_tool_result_artifact_id(), artifact_kind);
            let result = match outcome.status() {
                ToolCallResultStatus::Succeeded => ToolCallResult::new(
                    call_id.clone(),
                    ToolCallResultStatus::Succeeded,
                    artifact,
                    outcome.diagnostic().cloned(),
                )?,
                ToolCallResultStatus::Failed => {
                    let diagnostic = outcome.diagnostic().cloned().ok_or(RuntimeError::Core {
                        source: merry_core::CoreError::InvalidToolCallResult {
                            reason: "failed tool execution outcome must include a diagnostic",
                        },
                    })?;
                    ToolCallResult::failed(call_id.clone(), artifact, diagnostic)
                }
            };
            self.validate_tool_result_content(&result, outcome.content())?;
            self.artifacts
                .ensure_recordable(result.artifact(), outcome.content())?;
        }
        Ok(())
    }

    pub(super) fn record_tool_result_observation(&mut self, observation: LedgerUpdateKind) {
        self.ledger.append(observation);
    }

    pub(super) fn next_tool_result_artifact_id(&self) -> ArtifactId {
        super::super::artifacts::tool_result_id(self.next_sequence())
    }

    pub(crate) fn tool_result_artifact_kind(
        &self,
        content: &ArtifactContent,
    ) -> Result<ArtifactKind, RuntimeError> {
        match content {
            ArtifactContent::Text { .. } => Ok(ArtifactKind::Text),
            ArtifactContent::Json { .. } => Ok(ArtifactKind::Json),
            ArtifactContent::Binary { .. }
            | ArtifactContent::Image { .. }
            | ArtifactContent::Other { .. } => Err(RuntimeError::UnsupportedToolResultContent {
                artifact_id: self.next_tool_result_artifact_id(),
                content_kind: content.kind(),
            }),
        }
    }

    pub(super) fn validate_tool_result_content(
        &self,
        result: &ToolCallResult,
        content: &ArtifactContent,
    ) -> Result<(), RuntimeError> {
        let supported = matches!(
            content,
            ArtifactContent::Text { .. } | ArtifactContent::Json { .. }
        );
        let compatible = matches!(
            (result.artifact().kind(), content),
            (ArtifactKind::Text, ArtifactContent::Text { .. })
                | (ArtifactKind::Json, ArtifactContent::Json { .. })
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
            ArtifactContent::Text { content: text } | ArtifactContent::Json { content: text }
                if text.trim().is_empty() =>
            {
                Err(RuntimeError::UnsupportedToolResultContent {
                    artifact_id: result.artifact().id().clone(),
                    content_kind: content.kind(),
                })
            }
            ArtifactContent::Text { .. } | ArtifactContent::Json { .. } => Ok(()),
            ArtifactContent::Binary { .. }
            | ArtifactContent::Image { .. }
            | ArtifactContent::Other { .. } => {
                unreachable!("unsupported tool result content is rejected before blank validation")
            }
        }
    }
}
