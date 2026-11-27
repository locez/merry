//! Projects in-memory trajectory snapshots from durable session transcripts.
//!
//! Trajectories are not persisted to disk. Resuming a session seeds the
//! in-memory observability view directly from the replayed transcript, keeping
//! only bounded previews of each tool result's model-facing output.

#[cfg(test)]
use crate::SessionTranscriptItem;
use crate::{
    session::{SessionState, TranscriptItem},
    trajectory::{
        RuntimeObservability,
        records::{
            TRAJECTORY_TOOL_PREVIEW_BYTES, merge_tool_result, record, tool_call_record,
            truncate_summary,
        },
    },
};
use merry_core::{
    PendingToolCall, ToolCallId, ToolCallResult, ToolOutput, TrajectoryLane, TrajectoryRecord,
    TrajectoryRecordKind, TrajectoryRecordStatus, TrajectorySnapshot, TrajectoryTurnId,
};

enum SeedItem<'a> {
    UserMessage {
        text: &'a str,
    },
    AssistantText {
        text: &'a str,
    },
    ToolCall {
        call: &'a PendingToolCall,
    },
    ToolResult {
        call_id: &'a ToolCallId,
        result: &'a ToolCallResult,
        output: Option<&'a ToolOutput>,
    },
}

impl RuntimeObservability {
    /// Seeds the in-memory trajectory directly from a resumed session's
    /// transcript without cloning full tool outputs.
    pub(crate) fn seed_from_session(&self, session: &SessionState) {
        let items = session.transcript().items().iter().filter_map(|item| {
            Some(match item {
                TranscriptItem::UserMessage { text, .. } => SeedItem::UserMessage {
                    text: text.as_ref(),
                },
                TranscriptItem::AssistantText { text, .. } => SeedItem::AssistantText {
                    text: text.as_ref(),
                },
                TranscriptItem::AssistantReasoning { .. } => return None,
                TranscriptItem::ToolCall { call, .. } => SeedItem::ToolCall { call },
                TranscriptItem::ToolResult {
                    call_id,
                    result,
                    payload,
                    ..
                } => SeedItem::ToolResult {
                    call_id,
                    result,
                    output: Some(payload.model_or_full()),
                },
            })
        });
        self.seed_items(items);
    }

    #[cfg(test)]
    pub(crate) fn seed_from_transcript(&self, transcript: &[SessionTranscriptItem]) {
        let items = transcript.iter().filter_map(|item| {
            Some(match item {
                SessionTranscriptItem::UserMessage { text, .. } => SeedItem::UserMessage { text },
                SessionTranscriptItem::AssistantText { text } => SeedItem::AssistantText { text },
                SessionTranscriptItem::AssistantReasoning { .. } => return None,
                SessionTranscriptItem::ToolCall { call } => SeedItem::ToolCall { call },
                SessionTranscriptItem::ToolResult {
                    call_id,
                    result,
                    output,
                } => SeedItem::ToolResult {
                    call_id,
                    result,
                    output: output.as_ref(),
                },
            })
        });
        self.seed_items(items);
    }

    fn seed_items<'a>(&self, items: impl IntoIterator<Item = SeedItem<'a>>) {
        let mut state = self.lock_state();
        if !state.snapshot.records().is_empty() {
            return;
        }

        for (index, item) in items.into_iter().enumerate() {
            let sequence = index as u64 + 1;
            let is_user_input = matches!(item, SeedItem::UserMessage { .. });
            let turn_id = if is_user_input {
                let turn_id = TrajectoryTurnId::new(state.next_turn_id).ok();
                state.next_turn_id = state.next_turn_id.saturating_add(1);
                state.active_turn_id = turn_id;
                turn_id
            } else {
                state.active_turn_id
            };
            let record = match item {
                SeedItem::UserMessage { text } => {
                    let identity = format!("transcript-{sequence}");
                    let Some(mut record) = record(
                        "user",
                        &identity,
                        TrajectoryLane::Input,
                        TrajectoryRecordKind::UserInput,
                        TrajectoryRecordStatus::Completed,
                        sequence,
                    ) else {
                        continue;
                    };
                    record.set_summary(Some(truncate_summary(text)));
                    record.set_message_details(text.to_owned(), false);
                    Some(record)
                }
                SeedItem::AssistantText { text } => {
                    let identity = format!("transcript-{sequence}");
                    let Some(mut record) = record(
                        "assistant",
                        &identity,
                        TrajectoryLane::Model,
                        TrajectoryRecordKind::AssistantMessage,
                        TrajectoryRecordStatus::Succeeded,
                        sequence,
                    ) else {
                        continue;
                    };
                    record.set_summary(Some(truncate_summary(text)));
                    record.set_message_details(text.to_owned(), false);
                    Some(record)
                }
                SeedItem::ToolCall { call } => {
                    tool_call_record(call, sequence, 0, TrajectoryRecordStatus::Running)
                }
                SeedItem::ToolResult {
                    call_id,
                    result,
                    output,
                } => {
                    let existing = state
                        .snapshot
                        .records()
                        .iter()
                        .find(|record| record.tool_call_id() == Some(call_id))
                        .cloned();
                    let preview =
                        output.map(|output| output.bounded_preview(TRAJECTORY_TOOL_PREVIEW_BYTES));
                    merge_tool_result(existing, call_id, result, sequence, preview.as_ref())
                }
            };
            if let Some(mut record) = record {
                record.set_sequence_order(u32::try_from(index).unwrap_or(u32::MAX));
                record.set_turn_id(turn_id);
                let record_id = record.id().clone();
                if !is_user_input {
                    let tool_call_id = record.tool_call_id().cloned();
                    record.set_relationship(state.active_parent_id.clone(), tool_call_id);
                }
                insert_without_publish(&mut state.snapshot, record);
                if is_user_input {
                    state.active_parent_id = Some(record_id);
                }
            }
        }
    }
}

fn insert_without_publish(snapshot: &mut TrajectorySnapshot, record: TrajectoryRecord) {
    snapshot.advance_latest_sequence(record.start_sequence());
    snapshot.advance_revision();
    snapshot.upsert_record(record);
}
