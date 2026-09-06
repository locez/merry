//! Reconstructs and reconciles trajectory snapshots from durable session evidence.

use crate::{
    LedgerProjectionSnapshot, SessionTranscriptItem,
    session::{SessionState, SessionTrajectory, SessionTrajectoryItem},
    trajectory::{
        RuntimeObservability,
        records::{
            lifecycle_record, merge_tool_result, record, tool_call_record, truncate_summary,
        },
    },
    trajectory_replay::{ReplayRecords, ReplaySequences},
};
use merry_core::{
    TrajectoryLane, TrajectoryRecord, TrajectoryRecordDetails, TrajectoryRecordId,
    TrajectoryRecordKind, TrajectoryRecordStatus, TrajectorySnapshot, TrajectoryTurnId,
};
use std::collections::HashMap;

impl RuntimeObservability {
    /// Restores a persisted trajectory projection before a resumed runtime is used.
    ///
    /// The stored snapshot is the authoritative read model for new-format
    /// sessions. Missing assistant details from older snapshots are restored
    /// from the session-owned artifact registry before the snapshot becomes
    /// observable again. A resumed runtime reopens the snapshot so durable
    /// transcript and ledger state can be reconciled before new events arrive.
    pub(crate) fn restore_snapshot(
        &self,
        mut snapshot: TrajectorySnapshot,
        session: &SessionState,
    ) {
        let mut state = self.lock_state();
        if snapshot.session_id() != state.snapshot.session_id() {
            return;
        }
        snapshot.reopen();
        hydrate_assistant_message_details(&mut snapshot, session);
        if snapshot.tool_specs().is_empty() {
            snapshot.set_tool_specs(state.snapshot.tool_specs().to_vec());
        }
        let last_turn_id = snapshot
            .records()
            .iter()
            .filter_map(|record| record.turn_id().map(TrajectoryTurnId::value))
            .max();
        let active_parent_id = snapshot
            .records()
            .iter()
            .rev()
            .find(|record| {
                record.lane() == TrajectoryLane::Input
                    && record.kind() == TrajectoryRecordKind::UserInput
            })
            .map(|record| record.id().clone());
        state.next_turn_id = last_turn_id.map_or(1, |value| value.saturating_add(1).max(1));
        state.active_turn_id = snapshot
            .records()
            .iter()
            .rev()
            .find_map(|record| record.turn_id());
        state.active_parent_id = active_parent_id;
        state.snapshot = snapshot;
    }

    /// Rebuilds records that were durable in the session but absent from an
    /// older trajectory savepoint.
    ///
    /// Transcript artifacts carry exact message and tool evidence. The ledger
    /// supplies the real event sequences for pending calls, resolved calls,
    /// and step starts, so replay does not invent a second sequence space.
    pub(crate) fn reconcile_from_session(
        &self,
        trajectory: &SessionTrajectory,
        ledger: &LedgerProjectionSnapshot,
    ) {
        let mut state = self.lock_state();
        let baseline = state.snapshot.latest_sequence();
        let mut sequences =
            ReplaySequences::from_ledger(baseline, ledger, &trajectory.model_turn_sequences);
        let mut replay_records = ReplayRecords::from_snapshot(&state.snapshot);
        let mut user_index = 0_usize;
        let mut active_turn_id = state.active_turn_id;
        let mut active_parent_id = state.active_parent_id.clone();
        let mut turn_by_model_turn = HashMap::<u64, Option<TrajectoryTurnId>>::new();
        let mut parent_by_turn = HashMap::<TrajectoryTurnId, TrajectoryRecordId>::new();

        for (item_index, item) in trajectory.items.iter().enumerate() {
            let sequence_order = u32::try_from(item_index).unwrap_or(u32::MAX);
            match item {
                SessionTrajectoryItem::UserMessage {
                    item_id,
                    model_turn_id,
                    artifact,
                    text,
                    ..
                } => {
                    let sequence = sequences
                        .model_turn_sequence(*model_turn_id)
                        .unwrap_or_else(|| sequences.next_fallback());
                    let Ok(logical_turn_number) = u64::try_from(user_index.saturating_add(1))
                    else {
                        continue;
                    };
                    user_index = user_index.saturating_add(1);
                    let Some(turn_id) = TrajectoryTurnId::new(logical_turn_number).ok() else {
                        continue;
                    };
                    state.next_turn_id = state
                        .next_turn_id
                        .max(logical_turn_number.saturating_add(1));
                    let mut record =
                        if let Some(record) = replay_records.take_user(artifact, turn_id) {
                            record
                        } else {
                            let identity = format!("{item_id}-{sequence}");
                            let Some(record) = record(
                                "input",
                                &identity,
                                TrajectoryLane::Input,
                                TrajectoryRecordKind::UserInput,
                                TrajectoryRecordStatus::Completed,
                                sequence,
                            ) else {
                                continue;
                            };
                            record
                        };
                    record.set_start_sequence(sequence);
                    record.set_label("User input".to_owned());
                    record.set_sequence_order(sequence_order);
                    record.set_turn_id(Some(turn_id));
                    record.set_summary(Some(truncate_summary(text)));
                    record.set_message_details(text.clone(), false);
                    record.add_artifact(artifact.clone());
                    record.set_relationship(None, None);
                    let record_id = record.id().clone();
                    insert_without_publish(&mut state.snapshot, record);
                    turn_by_model_turn.insert(*model_turn_id, Some(turn_id));
                    parent_by_turn.insert(turn_id, record_id.clone());
                    active_turn_id = Some(turn_id);
                    active_parent_id = Some(record_id);
                }
                SessionTrajectoryItem::AssistantText {
                    model_turn_id,
                    artifact,
                    text,
                    ..
                } => {
                    turn_by_model_turn
                        .entry(*model_turn_id)
                        .or_insert(active_turn_id);
                    let artifact_id = artifact.id().as_str();
                    let artifact_sequence =
                        sequence_from_artifact(artifact_id, "assistant-output-");
                    let sequence = artifact_sequence
                        .or_else(|| sequences.model_turn_sequence(*model_turn_id))
                        .unwrap_or_else(|| sequences.next_fallback());
                    let mut record = if let Some(record) = replay_records.take_assistant(artifact) {
                        record
                    } else {
                        let Some(record) = record(
                            "assistant",
                            artifact_id,
                            TrajectoryLane::Model,
                            TrajectoryRecordKind::AssistantMessage,
                            TrajectoryRecordStatus::Succeeded,
                            sequence,
                        ) else {
                            continue;
                        };
                        record
                    };
                    record.set_start_sequence(sequence);
                    record.set_label("Assistant message".to_owned());
                    record.set_sequence_order(sequence_order);
                    record.set_turn_id(active_turn_id);
                    record.set_summary(Some(truncate_summary(text)));
                    record.set_message_details(text.clone(), false);
                    record.add_artifact(artifact.clone());
                    record.set_relationship(active_parent_id.clone(), None);
                    insert_without_publish(&mut state.snapshot, record);
                }
                SessionTrajectoryItem::ToolCall {
                    model_turn_id,
                    call,
                    ..
                } => {
                    turn_by_model_turn
                        .entry(*model_turn_id)
                        .or_insert(active_turn_id);
                    let sequence = sequences.next_tool_pending_sequence();
                    let existing = state
                        .snapshot
                        .records()
                        .iter()
                        .find(|record| record.tool_call_id() == Some(call.id()))
                        .cloned();
                    let mut record = if let Some(record) = existing {
                        record
                    } else {
                        let Some(record) = tool_call_record(
                            call,
                            sequence,
                            sequence_order,
                            TrajectoryRecordStatus::Running,
                        ) else {
                            continue;
                        };
                        record
                    };
                    record.set_start_sequence(sequence);
                    record.set_sequence_order(sequence_order);
                    record.set_turn_id(active_turn_id);
                    record.set_summary(Some(format!("{}()", call.name())));
                    record.set_tool_details(Some(call.name().clone()), call.arguments().clone());
                    record.set_relationship(active_parent_id.clone(), Some(call.id().clone()));
                    insert_without_publish(&mut state.snapshot, record);
                }
                SessionTrajectoryItem::ToolResult {
                    model_turn_id,
                    call_id,
                    result,
                    artifact,
                    output,
                    ..
                } => {
                    turn_by_model_turn
                        .entry(*model_turn_id)
                        .or_insert(active_turn_id);
                    let existing = state
                        .snapshot
                        .records()
                        .iter()
                        .find(|record| record.tool_call_id() == Some(call_id))
                        .cloned();
                    let artifact_sequence =
                        sequence_from_artifact(artifact.id().as_str(), "tool-result-");
                    let sequence = sequences
                        .next_tool_resolved_sequence()
                        .or_else(|| artifact_sequence.map(|sequence| sequence.saturating_add(1)))
                        .unwrap_or_else(|| sequences.next_fallback());
                    let Some(mut record) =
                        merge_tool_result(existing, call_id, result, sequence, output.as_ref())
                    else {
                        continue;
                    };
                    record.set_turn_id(active_turn_id);
                    record.set_relationship(active_parent_id.clone(), Some(call_id.clone()));
                    record.set_sequence_order(sequence_order);
                    record.add_artifact(artifact.clone());
                    insert_without_publish(&mut state.snapshot, record);
                }
            }
        }
        state.active_turn_id = active_turn_id;
        state.active_parent_id = active_parent_id;
        while let Some(sequence) = sequences.model_retry.pop_front() {
            let model_turn_id = sequences.model_turn_id_for_sequence(sequence);
            let Some(mut record) = lifecycle_record(
                &format!("persisted-retry-{sequence}"),
                "Model retry",
                TrajectoryRecordStatus::Running,
                sequence,
            ) else {
                continue;
            };
            if let Some(existing) = state
                .snapshot
                .records()
                .iter()
                .find(|current| current.id() == record.id())
                .cloned()
            {
                record = existing;
            }
            record.set_start_sequence(sequence);
            record.set_summary(Some("Model retry recorded in session ledger".to_owned()));
            let retry_turn = model_turn_id
                .and_then(|id| turn_by_model_turn.get(&id).copied().flatten())
                .or(state.active_turn_id);
            record.set_turn_id(retry_turn);
            let retry_parent = retry_turn
                .and_then(|turn_id| parent_by_turn.get(&turn_id).cloned())
                .or_else(|| state.active_parent_id.clone());
            record.set_relationship(retry_parent, None);
            insert_without_publish(&mut state.snapshot, record);
        }
        // Ledger lifecycle facts can be newer than the last record that the
        // trajectory can display. Keep the axis anchored to actual records;
        // otherwise a terminal StepCompleted would create an empty tail.
        let latest_record_sequence = state
            .snapshot
            .records()
            .iter()
            .flat_map(|record| [Some(record.start_sequence()), record.end_sequence()])
            .flatten()
            .max()
            .unwrap_or(baseline);
        state
            .snapshot
            .advance_latest_sequence(latest_record_sequence);
    }

    pub(crate) fn seed_from_transcript(&self, transcript: &[SessionTranscriptItem]) {
        let mut state = self.lock_state();
        if !state.snapshot.records().is_empty() {
            return;
        }

        for (index, item) in transcript.iter().enumerate() {
            let sequence = index as u64 + 1;
            let turn_id = match item {
                SessionTranscriptItem::UserMessage { .. } => {
                    let turn_id = TrajectoryTurnId::new(state.next_turn_id).ok();
                    state.next_turn_id = state.next_turn_id.saturating_add(1);
                    state.active_turn_id = turn_id;
                    turn_id
                }
                _ => state.active_turn_id,
            };
            let record = match item {
                SessionTranscriptItem::UserMessage { text, .. } => {
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
                    record.set_message_details(text.clone(), false);
                    Some(record)
                }
                SessionTranscriptItem::AssistantText { text } => {
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
                    record.set_message_details(text.clone(), false);
                    Some(record)
                }
                SessionTranscriptItem::ToolCall { call } => {
                    tool_call_record(call, sequence, 0, TrajectoryRecordStatus::Running)
                }
                SessionTranscriptItem::ToolResult {
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
                    let Some(record) =
                        merge_tool_result(existing, call_id, result, sequence, output.as_ref())
                    else {
                        continue;
                    };
                    Some(record)
                }
            };
            if let Some(mut record) = record {
                record.set_sequence_order(index as u32);
                record.set_turn_id(turn_id);
                let is_user_input = matches!(item, SessionTranscriptItem::UserMessage { .. });
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

pub(super) fn insert_without_publish(snapshot: &mut TrajectorySnapshot, record: TrajectoryRecord) {
    snapshot.advance_latest_sequence(record.start_sequence());
    snapshot.advance_revision();
    snapshot.upsert_record(record);
}

pub(super) fn hydrate_assistant_message_details(
    snapshot: &mut TrajectorySnapshot,
    session: &SessionState,
) {
    for mut record in snapshot.records().to_vec() {
        if record.kind() != TrajectoryRecordKind::AssistantMessage
            || !matches!(record.details(), TrajectoryRecordDetails::None)
        {
            continue;
        }
        let Some(text) = record.artifacts().iter().find_map(|artifact| {
            session
                .read_artifact_content(artifact.id())
                .ok()
                .and_then(|content| content.as_text().map(str::to_owned))
        }) else {
            continue;
        };
        record.set_summary(Some(truncate_summary(&text)));
        record.set_message_details(text, false);
        snapshot.upsert_record(record);
    }
}

pub(super) fn sequence_from_artifact(artifact_id: &str, prefix: &str) -> Option<u64> {
    artifact_id.strip_prefix(prefix)?.parse().ok()
}
