use merry_core::{
    ArtifactRef, PendingToolCall, PendingToolCallBatch, RuntimeJournalEvent, RuntimeJournalPayload,
    ToolCallResult,
};

pub(crate) fn event_kind_names(events: &[RuntimeJournalEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|event| match event.payload {
            RuntimeJournalPayload::SessionStarted => "SessionStarted",
            RuntimeJournalPayload::StepStarted => "StepStarted",
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
            RuntimeJournalPayload::ToolCallResolved { .. } => "ToolCallResolved",
            RuntimeJournalPayload::SkillUsed { .. } => "SkillUsed",
            _ => "Unknown",
        })
        .collect()
}

pub(crate) fn failed_code(events: &[RuntimeJournalEvent]) -> Option<&str> {
    events.iter().find_map(|event| match &event.payload {
        RuntimeJournalPayload::Failed { diagnostic } => Some(diagnostic.code()),
        _ => None,
    })
}

pub(crate) fn failed_sequence(events: &[RuntimeJournalEvent]) -> u64 {
    events
        .iter()
        .find_map(|event| match event.payload {
            RuntimeJournalPayload::Failed { .. } => Some(event.sequence),
            _ => None,
        })
        .expect("failed event should be present")
}

pub(crate) fn assert_no_completion(events: &[RuntimeJournalEvent]) {
    assert!(
        events
            .iter()
            .all(|event| !matches!(event.payload, RuntimeJournalPayload::StepCompleted)),
        "terminal failure/cancellation must not be followed by StepCompleted: {events:?}"
    );
}

pub(crate) fn assert_no_artifact_recorded(events: &[RuntimeJournalEvent]) {
    assert!(
        events.iter().all(|event| !matches!(
            event.payload,
            RuntimeJournalPayload::ArtifactRecorded { .. }
        )),
        "terminal failure/cancellation must not record artifacts: {events:?}"
    );
}

pub(crate) fn assert_no_tool_call_pending(events: &[RuntimeJournalEvent]) {
    assert!(
        events.iter().all(|event| !matches!(
            event.payload,
            RuntimeJournalPayload::ToolCallPending { .. }
                | RuntimeJournalPayload::ToolCallBatchPending { .. }
        )),
        "terminal failure/cancellation must not record pending tool calls: {events:?}"
    );
}

pub(crate) fn assert_no_failed(events: &[RuntimeJournalEvent]) {
    assert!(
        events
            .iter()
            .all(|event| !matches!(event.payload, RuntimeJournalPayload::Failed { .. })),
        "terminal cancellation must not emit Failed: {events:?}"
    );
}

pub(crate) fn assistant_output_artifact(events: &[RuntimeJournalEvent]) -> &ArtifactRef {
    events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::AssistantOutputRecorded { artifact } => Some(artifact),
            _ => None,
        })
        .expect("assistant output artifact should be recorded")
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

pub(crate) fn pending_tool_call_batch(events: &[RuntimeJournalEvent]) -> &PendingToolCallBatch {
    events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallBatchPending { batch } => Some(batch),
            _ => None,
        })
        .expect("pending tool call batch should be emitted")
}

pub(crate) fn resolved_tool_result(events: &[RuntimeJournalEvent]) -> &ToolCallResult {
    events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .expect("resolved tool call should be emitted")
}
