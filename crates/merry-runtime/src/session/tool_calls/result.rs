use super::SessionState;
use crate::{
    ActionExecutionEvidence, RuntimeError, ToolExecutionOutcome,
    session::transcript::{ToolResultPayload, ToolResultPromptProjection},
    tool::ToolResultContent,
};
use merry_core::{
    ErrorInfo, RuntimeJournalEvent, RuntimeJournalPayload, ToolCallId, ToolCallResult,
    ToolCallResultStatus, ToolOutput,
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

        let output = ToolOutput::json(json.clone());
        self.validate_tool_output(&call_id, &output)?;
        let result = ToolCallResult::succeeded(call_id.clone());
        let mut transcript = self.transcript.clone();
        transcript.hide_tool_call(&call_id)?;
        transcript.push_tool_result(
            call_id.clone(),
            result,
            ToolResultPayload::new(output, None, Vec::new()),
            ToolResultPromptProjection::Hidden,
        )?;
        self.transcript = transcript;
        self.pending_tool_calls.remove(pending_index);
        self.resolved_tool_calls.insert(call_id.clone());

        let mut events = Vec::with_capacity(if self.session_started { 1 } else { 2 });
        if let Some(started) = self.record_session_started_if_needed() {
            events.push(started);
        }
        events.push(
            self.record_event(RuntimeJournalPayload::FinalOutputRecorded {
                call_id: call_id.clone(),
                json: json.clone(),
            }),
        );

        Ok((crate::FinalOutput::new(call_id, json), events))
    }

    pub(crate) fn submit_tool_result(
        &mut self,
        result: ToolCallResult,
        content: impl Into<ToolResultContent>,
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

        let payload = self.prepare_tool_result_payload(result.call_id(), content.into())?;
        let mut transcript = self.transcript.clone();
        transcript.push_tool_result(
            result.call_id().clone(),
            result.clone(),
            payload,
            ToolResultPromptProjection::Full,
        )?;
        self.transcript = transcript;
        let pending = self.pending_tool_calls.remove(pending_index);
        let pending_for_skill_event = pending.clone();
        self.resolved_tool_calls.insert(result.call_id().clone());

        let mut events = Vec::with_capacity(if self.session_started { 2 } else { 3 });
        if let Some(started) = self.record_session_started_if_needed() {
            events.push(started);
        }

        events.push(self.record_event(RuntimeJournalPayload::ToolCallResolved {
            result: result.clone(),
        }));
        if let Some(event) = self.skill_used_event_for_read(&pending_for_skill_event, &result) {
            events.push(event);
        }

        Ok(events)
    }

    pub(crate) fn replace_tool_result_output(
        &mut self,
        call_id: &ToolCallId,
        output: ToolOutput,
    ) -> Result<(), RuntimeError> {
        let payload = self.prepare_tool_result_payload(call_id, output.into())?;
        self.transcript
            .replace_tool_result_payload(call_id, payload)
    }

    pub(crate) fn submit_tool_execution_outcome(
        &mut self,
        call_id: &ToolCallId,
        status: ToolCallResultStatus,
        content: impl Into<ToolResultContent>,
        diagnostic: Option<ErrorInfo>,
        execution_evidence: Option<ActionExecutionEvidence>,
    ) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
        debug_assert!(execution_evidence.is_none());
        let result = ToolCallResult::new(call_id.clone(), status, diagnostic)?;
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

    pub(crate) fn validate_tool_execution_outcomes(
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

            ToolCallResult::new(
                call_id.clone(),
                outcome.status(),
                outcome.diagnostic().cloned(),
            )?;
            self.validate_tool_output(call_id, outcome.content())?;
            if let Some(model) = outcome.model_content() {
                self.validate_tool_output(call_id, model)?;
            }
        }
        Ok(())
    }
}
