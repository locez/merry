//! Typed trajectory record construction and bounded payload normalization.

use merry_core::{
    ErrorInfo, PendingToolCall, ToolCallArguments, ToolCallId, ToolCallResult,
    ToolCallResultStatus, ToolOutput, TrajectoryLane, TrajectoryPayload, TrajectoryPayloadKind,
    TrajectoryRecord, TrajectoryRecordId, TrajectoryRecordKind, TrajectoryRecordStatus,
};
use serde_json::Map;
use sha2::{Digest, Sha256};

pub(super) fn record(
    prefix: &str,
    identity: &str,
    lane: TrajectoryLane,
    kind: TrajectoryRecordKind,
    status: TrajectoryRecordStatus,
    sequence: u64,
) -> Option<TrajectoryRecord> {
    Some(TrajectoryRecord::new(
        record_id(prefix, identity)?,
        lane,
        kind,
        identity.to_owned(),
        status,
        sequence,
    ))
}

pub(super) fn tool_call_record(
    call: &PendingToolCall,
    sequence: u64,
    sequence_order: u32,
    status: TrajectoryRecordStatus,
) -> Option<TrajectoryRecord> {
    let mut record = record(
        "tool",
        call.id().as_str(),
        TrajectoryLane::Tools,
        TrajectoryRecordKind::ToolCall,
        status,
        sequence,
    )?;
    record.set_summary(Some(format!("{}()", call.name())));
    record.set_sequence_order(sequence_order);
    record.set_relationship(None, Some(call.id().clone()));
    record.set_tool_details(Some(call.name().clone()), call.arguments().clone());
    Some(record)
}

pub(super) fn tool_result_record(
    call_id: &ToolCallId,
    result: &ToolCallResult,
    sequence: u64,
    output: Option<&ToolOutput>,
) -> Option<TrajectoryRecord> {
    let mut record = record(
        "tool",
        call_id.as_str(),
        TrajectoryLane::Tools,
        TrajectoryRecordKind::ToolCall,
        TrajectoryRecordStatus::Completed,
        sequence,
    )?;
    record.set_label("Tool".to_owned());
    record.set_tool_details(None, ToolCallArguments::new(Map::new()));
    record.set_tool_output(output.and_then(trajectory_payload));
    record.add_artifact(result.artifact().clone());
    record.set_relationship(None, Some(call_id.clone()));
    Some(record)
}

pub(super) fn merge_tool_result(
    existing: Option<TrajectoryRecord>,
    call_id: &ToolCallId,
    result: &ToolCallResult,
    sequence: u64,
    output: Option<&ToolOutput>,
) -> Option<TrajectoryRecord> {
    let mut record = existing.or_else(|| tool_result_record(call_id, result, sequence, output))?;
    record.finish(tool_status(result.status()), sequence);
    record.add_artifact(result.artifact().clone());
    let parent_id = record.parent_id().cloned();
    record.set_relationship(parent_id, Some(call_id.clone()));
    if let Some(output) = output.and_then(trajectory_payload) {
        record.set_tool_output(Some(output));
    }
    if let Some(diagnostic) = result.diagnostic() {
        record.fail(diagnostic.clone(), sequence);
    }
    Some(record)
}

pub(super) fn lifecycle_record(
    identity: &str,
    label: &str,
    status: TrajectoryRecordStatus,
    sequence: u64,
) -> Option<TrajectoryRecord> {
    typed_lifecycle_record(
        identity,
        label,
        TrajectoryRecordKind::Lifecycle,
        status,
        sequence,
    )
}

pub(super) fn compaction_record(
    identity: &str,
    label: &str,
    status: TrajectoryRecordStatus,
    sequence: u64,
) -> Option<TrajectoryRecord> {
    typed_lifecycle_record(
        identity,
        label,
        TrajectoryRecordKind::Compaction,
        status,
        sequence,
    )
}

pub(super) fn typed_lifecycle_record(
    identity: &str,
    label: &str,
    kind: TrajectoryRecordKind,
    status: TrajectoryRecordStatus,
    sequence: u64,
) -> Option<TrajectoryRecord> {
    let identity = format!("{identity}-{sequence}");
    let mut record = record(
        "system",
        &identity,
        TrajectoryLane::System,
        kind,
        status,
        sequence,
    )?;
    record.set_label(label.to_owned());
    Some(record)
}

pub(super) fn lifecycle_failure_record(
    identity: &str,
    label: &str,
    status: TrajectoryRecordStatus,
    diagnostic: ErrorInfo,
    sequence: u64,
) -> Option<TrajectoryRecord> {
    let mut record = lifecycle_record(identity, label, status, sequence)?;
    record.fail(diagnostic, sequence);
    Some(record)
}

pub(super) fn tool_status(status: ToolCallResultStatus) -> TrajectoryRecordStatus {
    match status {
        ToolCallResultStatus::Succeeded => TrajectoryRecordStatus::Succeeded,
        ToolCallResultStatus::Failed => TrajectoryRecordStatus::Failed,
    }
}

pub(super) fn record_id(prefix: &str, identity: &str) -> Option<TrajectoryRecordId> {
    let digest = Sha256::digest(identity.as_bytes());
    let digest = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let value = format!("{prefix}-{digest}");
    TrajectoryRecordId::new(&value).ok()
}

pub(super) fn truncate_summary(value: &str) -> String {
    const MAX_SUMMARY_CHARS: usize = 512;
    let mut summary = value.chars().take(MAX_SUMMARY_CHARS).collect::<String>();
    if value.chars().count() > MAX_SUMMARY_CHARS {
        summary.push_str("...");
    }
    summary
}

pub(super) fn trajectory_payload(output: &ToolOutput) -> Option<TrajectoryPayload> {
    let (kind, content) = match output {
        ToolOutput::Text { text } => (TrajectoryPayloadKind::Text, text.as_str()),
        ToolOutput::Json { json } => (TrajectoryPayloadKind::Json, json.as_str()),
    };
    Some(TrajectoryPayload::new(kind, content.to_owned(), false))
}
