use super::SessionState;
use crate::{
    ActionExecutionEvidence, RuntimeError,
    artifact::{ArtifactContent, ArtifactError},
    ledger::{LedgerFactKind, LedgerUpdateKind},
};
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, ErrorInfo, RuntimeEvent, RuntimeEventKind, ToolCallId,
    ToolCallResult, ToolCallResultStatus,
};

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
        let artifact = ArtifactRef::new(
            super::super::artifacts::final_output_id(artifact_sequence),
            ArtifactKind::Json,
        );
        let content = ArtifactContent::json(json.clone());
        let content_bytes = content.as_bytes().len();
        let recorded = self.record_artifact_state(artifact, content)?;
        Self::trace_artifact_record(self.session_id.as_str(), &recorded, content_bytes);
        self.pending_tool_calls.remove(pending_index);
        self.resolved_tool_calls.insert(call_id.clone());
        self.transcript.remove_tool_call(&call_id);

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

        self.transcript.push_tool_result(
            result.call_id().clone(),
            result.clone(),
            result.artifact().id().clone(),
        )?;
        let pending = self.pending_tool_calls.remove(pending_index);
        let pending_for_skill_event = pending.clone();
        self.resolved_tool_calls.insert(result.call_id().clone());

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

    pub(super) fn record_tool_result_observation(&mut self, observation: LedgerUpdateKind) {
        self.ledger.append(observation);
    }

    pub(super) fn next_tool_result_artifact_id(&self) -> ArtifactId {
        super::super::artifacts::tool_result_id(self.next_sequence())
    }

    pub(super) fn tool_result_artifact_kind(
        &self,
        content: &ArtifactContent,
    ) -> Result<ArtifactKind, RuntimeError> {
        match content {
            ArtifactContent::Text(_) => Ok(ArtifactKind::Text),
            ArtifactContent::Json(_) => Ok(ArtifactKind::Json),
            ArtifactContent::Binary(_) | ArtifactContent::Image(_) | ArtifactContent::Other(_) => {
                Err(RuntimeError::UnsupportedToolResultContent {
                    artifact_id: self.next_tool_result_artifact_id(),
                    content_kind: content.kind(),
                })
            }
        }
    }

    pub(super) fn validate_tool_result_content(
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
