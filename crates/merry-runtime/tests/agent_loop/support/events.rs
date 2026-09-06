use merry_core::{PendingToolCall, RuntimeEvent, RuntimeJournalEvent, RuntimeJournalPayload};
use merry_llm::ModelRequest;
use merry_runtime::{AgentRun, AgentRunMessage};
use serde_json::{Value, json};

pub(crate) async fn next_agent_run_event(stream: &mut AgentRun) -> RuntimeEvent {
    match stream
        .next_message()
        .await
        .expect("agent run message should be readable")
    {
        Some(AgentRunMessage::Event(event)) => event,
        Some(AgentRunMessage::ToolInvocations { .. }) => {
            panic!("agent run emitted an unexpected host tool batch")
        }
        Some(_) => panic!("agent run emitted an unsupported message"),
        None => panic!("agent run closed before the expected event"),
    }
}

pub(crate) fn assert_sanitized_policy_denial_json(value: &Value, tool_name: &str) {
    assert_eq!(
        value,
        &json!({
            "ok": false,
            "tool": tool_name,
            "error": {
                "code": "action_policy_denied",
                "message": "tool action was blocked by runtime policy"
            }
        })
    );
    assert!(value.get("call_id").is_none());
    assert!(value.get("action_kind").is_none());
    assert!(value.get("policy").is_none());
    assert!(value.get("reason").is_none());
}

pub(crate) fn event_kind_names(events: &[RuntimeJournalEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|event| match event.payload {
            RuntimeJournalPayload::SessionStarted => "SessionStarted",
            RuntimeJournalPayload::StepStarted => "StepStarted",
            RuntimeJournalPayload::CompactionStarted => "CompactionStarted",
            RuntimeJournalPayload::CompactionCompleted { .. } => "CompactionCompleted",
            RuntimeJournalPayload::SessionUsageUpdated { .. } => "SessionUsageUpdated",
            RuntimeJournalPayload::StepCompleted => "StepCompleted",
            RuntimeJournalPayload::Cancelled { .. } => "Cancelled",
            RuntimeJournalPayload::Failed { .. } => "Failed",
            RuntimeJournalPayload::ArtifactRecorded { .. } => "ArtifactRecorded",
            RuntimeJournalPayload::AssistantOutputDelta { .. } => "AssistantOutputDelta",
            RuntimeJournalPayload::AssistantOutputRecorded { .. } => "AssistantOutputRecorded",
            RuntimeJournalPayload::EvidenceReferenced { .. } => "EvidenceReferenced",
            RuntimeJournalPayload::ToolCallPending { .. } => "ToolCallPending",
            RuntimeJournalPayload::ToolCallBatchPending { .. } => "ToolCallBatchPending",
            RuntimeJournalPayload::BridgeToolCallRequested { .. } => "BridgeToolCallRequested",
            RuntimeJournalPayload::ToolCallResolved { .. } => "ToolCallResolved",
            RuntimeJournalPayload::FinalOutputRecorded { .. } => "FinalOutputRecorded",
            RuntimeJournalPayload::SkillUsed { .. } => "SkillUsed",
            _ => "Unknown",
        })
        .collect()
}

pub(crate) fn public_event_kind_names(events: &[RuntimeEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|event| match event {
            RuntimeEvent::SessionStarted { .. } => "SessionStarted",
            RuntimeEvent::StepStarted { .. } => "StepStarted",
            RuntimeEvent::StepCompleted { .. } => "StepCompleted",
            RuntimeEvent::CompactionStarted { .. } => "CompactionStarted",
            RuntimeEvent::CompactionCompleted { .. } => "CompactionCompleted",
            RuntimeEvent::AssistantMessage { .. } => "AssistantMessage",
            RuntimeEvent::ToolCallStarted { .. } => "ToolCallStarted",
            RuntimeEvent::ToolCallBatchStarted { .. } => "ToolCallBatchStarted",
            RuntimeEvent::ToolCallFinished { .. } => "ToolCallFinished",
            RuntimeEvent::FinalOutputRecorded { .. } => "FinalOutputRecorded",
            RuntimeEvent::ModelRetryAttemptStarted { .. } => "ModelRetryAttemptStarted",
            RuntimeEvent::ModelRetryScheduled { .. } => "ModelRetryScheduled",
            RuntimeEvent::ModelRetryExhausted { .. } => "ModelRetryExhausted",
            RuntimeEvent::EvidenceReferenced { .. } => "EvidenceReferenced",
            RuntimeEvent::SkillUsed { .. } => "SkillUsed",
            RuntimeEvent::SubagentSpawned { .. } => "SubagentSpawned",
            RuntimeEvent::SubagentStarted { .. } => "SubagentStarted",
            RuntimeEvent::SubagentStatusChanged { .. } => "SubagentStatusChanged",
            RuntimeEvent::SubagentCompleted { .. } => "SubagentCompleted",
            RuntimeEvent::SubagentFailed { .. } => "SubagentFailed",
            RuntimeEvent::SubagentCancelled { .. } => "SubagentCancelled",
            RuntimeEvent::RunFailed { .. } => "RunFailed",
            RuntimeEvent::RunCancelled { .. } => "RunCancelled",
            RuntimeEvent::InteractiveRunStateChanged { .. } => "InteractiveRunStateChanged",
            RuntimeEvent::QueuedInputAccepted { .. } => "QueuedInputAccepted",
            RuntimeEvent::QueuedInputsChanged { .. } => "QueuedInputsChanged",
            RuntimeEvent::Closed => "Closed",
            _ => "Unknown",
        })
        .collect()
}

pub(crate) fn assert_continuation_request_body(request: &ModelRequest, original_task: &str) {
    let dynamic_text = request
        .dynamic_messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n---\n");

    assert!(dynamic_text.contains(original_task));
    assert!(
        !dynamic_text.contains("Continue after tool result."),
        "agent-loop continuation must not inject a synthetic user prompt"
    );
    assert!(
        !dynamic_text.contains("Original task:"),
        "agent-loop continuation must not inject the original task label"
    );
}

pub(crate) fn pending_tool_call(events: &[RuntimeJournalEvent]) -> &PendingToolCall {
    events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallPending { call } => Some(call),
            _ => None,
        })
        .expect("pending tool call should be emitted")
}
