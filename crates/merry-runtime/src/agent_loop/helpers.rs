//! Shared diagnostics and event-publishing helpers for agent-loop drivers.

use super::{
    AgentLoopBlockedReason, AgentLoopConfig, AgentLoopError, AgentLoopResult, AgentLoopStatus,
    AgentRunMessage,
};
use crate::{
    FinalOutput, FinalOutputContract, Runtime, RuntimeError, RuntimeJournalEventStream, StepInput,
    events::RuntimeEventProjector, subagent::completion_notification_text,
};
use futures_util::StreamExt;
use merry_core::{
    ArtifactKind, ErrorInfo, PendingToolCall, RuntimeJournalEvent, RuntimeJournalPayload,
    SessionId, ToolCallResultStatus,
};
use tokio::sync::mpsc;

pub(crate) fn trace_loop_finish(
    session_id: &str,
    status: &'static str,
    model_turns_run: usize,
    diagnostic_code: Option<&str>,
) {
    tracing::info!(
        event = "runtime.loop.finish",
        session_id,
        status,
        model_turns_run,
        diagnostic_code = diagnostic_code.unwrap_or(""),
        "runtime loop finish"
    );
}

pub(crate) fn trace_loop_error(session_id: &str, model_turns_run: usize, source: &RuntimeError) {
    trace_loop_finish(
        session_id,
        "error",
        model_turns_run,
        Some(runtime_error_code(source)),
    );
}

pub(crate) async fn collect_step_events(
    stream: RuntimeJournalEventStream,
) -> Vec<RuntimeJournalEvent> {
    stream.collect().await
}

pub(crate) async fn final_assistant_output_from_step(
    runtime: &Runtime,
    events: &[RuntimeJournalEvent],
) -> Option<String> {
    for event in events.iter().rev() {
        let RuntimeJournalPayload::AssistantOutputRecorded { artifact } = &event.payload else {
            continue;
        };
        if artifact.kind() != &ArtifactKind::Text {
            continue;
        }
        let Ok(content) = runtime.read_artifact_content(artifact.id()).await else {
            continue;
        };
        if let Some(text) = content.as_text() {
            return Some(text.to_owned());
        }
    }

    None
}

pub(crate) async fn record_final_output_tool_call(
    runtime: &Runtime,
    call: PendingToolCall,
) -> Result<(FinalOutput, Vec<RuntimeJournalEvent>), RuntimeError> {
    runtime.record_final_output_tool_call(call).await
}

pub(crate) fn validate_final_output(
    contract: &FinalOutputContract,
    call: &PendingToolCall,
) -> Result<(), String> {
    let json = serde_json::to_string(call.arguments().as_object())
        .map_err(|source| format!("structured output could not be serialized: {source}"))?;
    contract.validate_output(&json)
}

pub(crate) fn can_retry_structured_output(
    config: &AgentLoopConfig,
    retries: usize,
    model_turns_run: usize,
) -> bool {
    retries <= config.structured_output_retry_policy().max_retries()
        && model_turns_run < config.max_model_turns()
}

pub(crate) async fn structured_output_failure_result(
    runtime: &Runtime,
    events: Vec<RuntimeJournalEvent>,
    model_turns_run: usize,
    message: String,
) -> AgentLoopResult {
    tracing::debug!(
        session_id = runtime.session_id().as_str(),
        model_turns_run,
        error = %message,
        "structured output retry policy exhausted"
    );
    let session_usage = runtime.usage().await;
    AgentLoopResult::new(
        AgentLoopStatus::Failed {
            diagnostic: ErrorInfo::new(
                "structured_output_invalid",
                "structured final output could not be decoded as the requested type",
            )
            .expect("static structured output diagnostic is valid"),
        },
        events,
        model_turns_run,
        None,
        session_usage,
    )
}

pub(crate) async fn publish_journal_event(
    runtime: &Runtime,
    projector: &mut RuntimeEventProjector,
    sender: &mpsc::Sender<AgentRunMessage>,
    events: &mut Vec<RuntimeJournalEvent>,
    event: RuntimeJournalEvent,
) -> Result<(), RuntimeError> {
    let projected = projector.project(event.clone(), runtime).await?;

    events.push(event);

    if let Some(projected) = projected
        && sender
            .send(AgentRunMessage::Event(projected))
            .await
            .is_err()
    {
        return Err(RuntimeError::AgentRunClosed {
            session_id: runtime.session_id().clone(),
            message: "public event receiver closed before the event was delivered",
        });
    }

    Ok(())
}

pub(crate) fn continuation_step_input() -> StepInput {
    StepInput::no_new_user_input()
}

pub(crate) async fn take_subagent_notification(runtime: &Runtime) -> Option<(StepInput, String)> {
    let statuses = runtime.take_subagent_completion_notifications().await;
    if statuses.is_empty() {
        return None;
    }
    let text = completion_notification_text(&statuses);
    let input = match StepInput::loop_control_text(&text) {
        Ok(input) => input,
        Err(error) => {
            tracing::warn!(%error, "discarding invalid subagent completion notification");
            return None;
        }
    };
    Some((input, text))
}

pub(crate) async fn take_subagent_notification_input(runtime: &Runtime) -> Option<StepInput> {
    take_subagent_notification(runtime)
        .await
        .map(|(input, _)| input)
}

pub(crate) fn agent_loop_cancelled_diagnostic() -> ErrorInfo {
    ErrorInfo::new(
        "agent_loop_cancelled",
        "agent loop cancellation was requested",
    )
    .expect("static agent loop cancellation diagnostic must be valid")
}

pub(crate) fn agent_loop_stream_error(
    session_id: &SessionId,
    events: Vec<RuntimeJournalEvent>,
    message: &'static str,
) -> AgentLoopError {
    AgentLoopError::new(
        events,
        RuntimeError::AgentRunClosed {
            session_id: session_id.clone(),
            message,
        },
    )
}

pub(crate) fn agent_loop_stream_error_with_source(
    runtime: &Runtime,
    model_turns_run: usize,
    events: &[RuntimeJournalEvent],
    source: RuntimeError,
) -> AgentLoopError {
    trace_loop_error(runtime.session_id().as_str(), model_turns_run, &source);
    AgentLoopError::new(events.to_vec(), source)
}

pub(crate) fn tool_execution_cancelled_diagnostic(call_id: &merry_core::ToolCallId) -> ErrorInfo {
    ErrorInfo::new(
        "tool_execution_cancelled",
        &format!("tool call {call_id} execution was cancelled"),
    )
    .expect("static code and runtime-owned tool call id form a valid diagnostic")
}

pub(crate) fn blocked_reason_code(reason: &AgentLoopBlockedReason) -> &'static str {
    match reason {
        AgentLoopBlockedReason::MaxModelTurnsReached { .. } => "max_model_turns_reached",
        AgentLoopBlockedReason::MultiplePendingToolCalls { .. } => "multiple_pending_tool_calls",
        AgentLoopBlockedReason::StepCompletedWithPendingToolCall { .. } => {
            "step_completed_with_pending_tool_call"
        }
        AgentLoopBlockedReason::StepEndedWithoutTerminalEvent => {
            "step_ended_without_terminal_event"
        }
        AgentLoopBlockedReason::FinalOutputToolNotCalled => "final_output_tool_not_called",
        AgentLoopBlockedReason::BridgeToolCallRequested { .. } => "bridge_tool_call_requested",
    }
}

fn runtime_error_code(error: &RuntimeError) -> &'static str {
    match error {
        RuntimeError::StepAlreadyActive { .. } => "step_already_active",
        RuntimeError::InvalidStepInput { .. } => "invalid_step_input",
        RuntimeError::AgentLoopConfig { .. } => "agent_loop_config",
        RuntimeError::ChildRuntimeBuild { .. } => "child_runtime_build",
        RuntimeError::InvalidSubagentSelection { .. } => "invalid_subagent_selection",
        RuntimeError::PlanEffectAttribution { .. } => "plan_effect_attribution_failed",
        RuntimeError::PlanSubagentAttemptInactive { .. } => "plan_subagent_attempt_inactive",
        RuntimeError::InvalidUserImageInput { .. } => "invalid_user_image_input",
        RuntimeError::ReservedArtifactId { .. } => "reserved_artifact_id",
        RuntimeError::UnknownToolCall { .. } => "unknown_tool_call",
        RuntimeError::BridgeToolResultBatchEmpty { .. } => "bridge_tool_result_batch_empty",
        RuntimeError::BridgeToolResultBatchMismatch { .. } => "bridge_tool_result_batch_mismatch",
        RuntimeError::BridgeToolResultBatchIdMismatch { .. } => {
            "bridge_tool_result_batch_id_mismatch"
        }
        RuntimeError::AgentRunToolInvocationsPending { .. } => "agent_run_tool_invocations_pending",
        RuntimeError::NoPendingAgentRunToolInvocations { .. } => {
            "no_pending_agent_run_tool_invocations"
        }
        RuntimeError::ToolCallAlreadyResolved { .. } => "tool_call_already_resolved",
        RuntimeError::DuplicateToolRegistration { .. } => "duplicate_tool_registration",
        RuntimeError::ReservedToolName { .. } => "reserved_tool_name",
        RuntimeError::InvalidToolInputSchema { .. } => "invalid_tool_input_schema",
        RuntimeError::BridgeToolsNotAllowed { .. } => "bridge_tools_not_allowed",
        RuntimeError::ToolExecutionCancelled { .. } => "tool_execution_cancelled",
        RuntimeError::ToolExecutionFailed { .. } => "tool_execution_failed",
        RuntimeError::AgentRunClosed { .. } => "agent_run_closed",
        RuntimeError::BridgeToolResultRejected { .. } => "bridge_tool_result_rejected",
        RuntimeError::MissingActionExecutionEvidence { .. } => "missing_action_execution_evidence",
        RuntimeError::MutatingActionCommitLifecycleRequired { .. } => {
            "mutating_action_commit_lifecycle_required"
        }
        RuntimeError::UnsupportedToolResultContent { .. } => "unsupported_tool_result_content",
        RuntimeError::TranscriptItemIdExhausted => "transcript_item_id_exhausted",
        RuntimeError::ModelTurnIdExhausted => "model_turn_id_exhausted",
        RuntimeError::UnknownModelTurn { .. } => "unknown_model_turn",
        RuntimeError::InvalidModelTurnTransition { .. } => "invalid_model_turn_transition",
        RuntimeError::TranscriptToolCallMissing { .. } => "transcript_tool_call_missing",
        RuntimeError::Core { .. } => "core_error",
        RuntimeError::Artifact { .. } => "artifact_error",
        RuntimeError::Context { .. } => "context_error",
        RuntimeError::Checkpoint { .. } => "checkpoint_error",
        RuntimeError::Compaction { .. } => "compaction_error",
        RuntimeError::SessionStore { .. } => "session_store",
        RuntimeError::MissingModelProvider { .. } => "missing_model_provider",
        RuntimeError::CompactionModelRequest { .. } => "compaction_model_request",
        RuntimeError::CompactionModelWindowTooSmall { .. } => "compaction_model_window_too_small",
        RuntimeError::CompactionModelInputTooLarge { .. } => "compaction_model_input_too_large",
        RuntimeError::CompactionModelSetup { .. } => "compaction_model_setup",
        RuntimeError::CompactionModelStream { .. } => "compaction_model_stream",
    }
}

pub(crate) fn tool_resolution_status(events: &[RuntimeJournalEvent]) -> &'static str {
    events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => Some(match result.status() {
                ToolCallResultStatus::Succeeded => "succeeded",
                ToolCallResultStatus::Failed => "failed",
            }),
            _ => None,
        })
        .unwrap_or("unresolved")
}

pub(crate) fn tool_resolution_artifact_id(events: &[RuntimeJournalEvent]) -> String {
    events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => {
                Some(result.artifact().id().as_str().to_owned())
            }
            _ => None,
        })
        .unwrap_or_default()
}

pub(crate) fn tool_resolution_diagnostic_code(events: &[RuntimeJournalEvent]) -> Option<&str> {
    events.iter().find_map(|event| match &event.payload {
        RuntimeJournalPayload::ToolCallResolved { result } => {
            result.diagnostic().map(merry_core::ErrorInfo::code)
        }
        _ => None,
    })
}

pub(crate) fn tool_resolution_is_policy_denied(events: &[RuntimeJournalEvent]) -> bool {
    tool_resolution_diagnostic_code(events) == Some("action_policy_denied")
}
