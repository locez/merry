//! Runtime-owned trajectory projection and subscriptions.

use crate::{
    LedgerProjectionSnapshot, ProjectRules, PromptProfile, SessionTranscriptItem, SkillCatalog,
    session::{SessionState, SessionTrajectory, SessionTrajectoryItem},
    trajectory_replay::{ReplayRecords, ReplaySequences},
};
use merry_core::{
    ErrorInfo, PendingToolCall, QueuedInputView, RuntimeJournalEvent, RuntimeJournalPayload,
    ToolCallArguments, ToolCallId, ToolCallResult, ToolCallResultStatus, ToolOutput, ToolSpec,
    TrajectoryEvent, TrajectoryLane, TrajectoryPayload, TrajectoryPayloadKind,
    TrajectoryPromptBlock, TrajectoryRecord, TrajectoryRecordDetails, TrajectoryRecordId,
    TrajectoryRecordKind, TrajectoryRecordStatus, TrajectorySnapshot, TrajectoryTurnId,
};
use merry_llm::{ModelInputItem, ModelMessageRole, ModelRequest};
use serde_json::Map;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
};
use tokio::sync::broadcast;

const TRAJECTORY_EVENT_BUFFER: usize = 256;

/// Runtime-owned read model for one session's trajectory.
pub(crate) struct RuntimeObservability {
    state: Mutex<ProjectionState>,
    updates: broadcast::Sender<TrajectoryEvent>,
}

struct ProjectionState {
    snapshot: TrajectorySnapshot,
    pending_inputs: Vec<QueuedInputView>,
    next_turn_id: u64,
    active_turn_id: Option<TrajectoryTurnId>,
    active_parent_id: Option<TrajectoryRecordId>,
}

impl RuntimeObservability {
    pub(crate) fn new(session_id: merry_core::SessionId, tool_specs: Vec<ToolSpec>) -> Arc<Self> {
        let (updates, _) = broadcast::channel(TRAJECTORY_EVENT_BUFFER);
        let mut snapshot = TrajectorySnapshot::new(session_id);
        snapshot.set_tool_specs(tool_specs);
        Arc::new(Self {
            state: Mutex::new(ProjectionState {
                snapshot,
                pending_inputs: Vec::new(),
                next_turn_id: 1,
                active_turn_id: None,
                active_parent_id: None,
            }),
            updates,
        })
    }

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

    /// Seeds stable prompt material for a new or legacy-resumed session.
    ///
    /// New-format resumed sessions restore the persisted projection instead;
    /// this fallback only supplies initial navigation data when no projection
    /// was stored by an older session format.
    pub(crate) fn seed_stable_prompt(&self, blocks: &[String]) {
        let mut state = self.lock_state();
        if state.snapshot.is_closed() || !state.snapshot.prompt().stable_blocks().is_empty() {
            return;
        }
        for (index, content) in blocks.iter().enumerate() {
            let Some(block) = prompt_block(content, index as u32) else {
                continue;
            };
            state.snapshot.upsert_prompt_block(block);
        }
    }

    pub(crate) fn seed_prompt_profile(
        &self,
        profile: &PromptProfile,
        progress_commentary: bool,
        skill_catalog: Option<&SkillCatalog>,
        project_rules: Option<&ProjectRules>,
    ) {
        let mut blocks = vec![profile.base_instructions().to_owned()];
        if progress_commentary {
            blocks.push(profile.progress_commentary_instructions().to_owned());
        }
        blocks.extend(profile.stable_blocks().iter().map(|block| block.render()));
        if let Some(skill_catalog) = skill_catalog
            && let Some(text) = skill_catalog.to_stable_prefix_message_text()
        {
            blocks.push(format_prompt_block("merry_skill_catalog", &text));
        }
        if let Some(project_rules) = project_rules {
            blocks.push(format_prompt_block(
                "merry_project_rules",
                &project_rules.to_stable_prefix_message_text(),
            ));
        }
        self.seed_stable_prompt(&blocks);
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

    /// Projects provider-visible prompt evidence into the session-level prompt
    /// snapshot. Prompt blocks are deduplicated by content identity and
    /// dynamic context is represented by an aggregate count rather than one
    /// repeated row per model request.
    ///
    /// The request is the runtime-owned normalized boundary, so this keeps the
    /// trajectory aligned with what the provider actually receives while
    /// leaving provider wire formats outside the projection.
    pub(crate) fn observe_model_request(&self, request: &ModelRequest, sequence: u64) {
        let mut stable_blocks = Vec::new();
        let mut dynamic_context_count = 0_u64;
        for (index, item) in request.input().iter().enumerate() {
            let ModelInputItem::Message(message) = item else {
                continue;
            };
            if message.role() != ModelMessageRole::System {
                continue;
            }

            let content = message.content().as_text();
            if index < request.stable_prefix_item_count() {
                if let Some(block) = prompt_block(content, index as u32) {
                    stable_blocks.push(block);
                }
            } else {
                dynamic_context_count = dynamic_context_count.saturating_add(1);
            }
        }

        if stable_blocks.is_empty() && dynamic_context_count == 0 {
            return;
        }
        let event = {
            let mut state = self.lock_state();
            if state.snapshot.is_closed() {
                return;
            }
            let mut changed = false;
            for block in stable_blocks {
                changed |= state.snapshot.upsert_prompt_block(block);
            }
            if dynamic_context_count > 0 {
                state
                    .snapshot
                    .add_dynamic_context(dynamic_context_count, sequence);
                changed = true;
            }
            state.snapshot.advance_latest_sequence(sequence);
            if !changed {
                return;
            }
            state.snapshot.advance_revision();
            TrajectoryEvent::PromptUpdated {
                revision: state.snapshot.revision(),
                latest_sequence: state.snapshot.latest_sequence(),
                prompt: state.snapshot.prompt().clone(),
            }
        };
        let _ = self.updates.send(event);
    }

    pub(crate) fn snapshot(&self) -> TrajectorySnapshot {
        self.lock_state().snapshot.clone()
    }

    pub(crate) fn subscribe_with_snapshot(
        &self,
    ) -> (TrajectorySnapshot, broadcast::Receiver<TrajectoryEvent>) {
        let state = self.lock_state();
        let receiver = self.updates.subscribe();
        (state.snapshot.clone(), receiver)
    }

    pub(crate) fn record_queued_input_accepted(&self, inputs: &[QueuedInputView]) {
        let mut state = self.lock_state();
        state.pending_inputs.extend(inputs.iter().cloned());
    }

    pub(crate) fn close(&self) {
        self.publish_session_closed();
    }

    pub(crate) fn observe_journal_event(&self, event: &RuntimeJournalEvent) {
        self.observe_journal_event_with_assistant_text(event, None);
    }

    pub(crate) fn observe_journal_event_with_assistant_text(
        &self,
        event: &RuntimeJournalEvent,
        assistant_text: Option<&str>,
    ) {
        self.observe_journal_event_with_contents(event, assistant_text, None);
    }

    pub(crate) fn observe_journal_event_with_contents(
        &self,
        event: &RuntimeJournalEvent,
        assistant_text: Option<&str>,
        tool_output: Option<&ToolOutput>,
    ) {
        if matches!(&event.payload, RuntimeJournalPayload::StepStarted) {
            self.publish_pending_inputs(event.sequence);
        }
        match &event.payload {
            RuntimeJournalPayload::AssistantOutputRecorded { artifact } => {
                let Some(mut record) = record(
                    "assistant",
                    artifact.id().as_str(),
                    TrajectoryLane::Model,
                    TrajectoryRecordKind::AssistantMessage,
                    TrajectoryRecordStatus::Succeeded,
                    event.sequence,
                ) else {
                    return;
                };
                record.set_label("Assistant message".to_owned());
                if let Some(text) = assistant_text {
                    record.set_summary(Some(truncate_summary(text)));
                    record.set_message_details(text.to_owned(), false);
                }
                record.add_artifact(artifact.clone());
                self.publish(Some(record), event.sequence);
            }
            RuntimeJournalPayload::ToolCallPending { call }
            | RuntimeJournalPayload::BridgeToolCallRequested { call } => {
                self.publish(
                    tool_call_record(call, event.sequence, 0, TrajectoryRecordStatus::Pending),
                    event.sequence,
                );
            }
            RuntimeJournalPayload::ToolCallBatchPending { batch } => {
                for (index, call) in batch.calls().iter().enumerate() {
                    self.publish(
                        tool_call_record(
                            call,
                            event.sequence,
                            index as u32,
                            TrajectoryRecordStatus::Pending,
                        ),
                        event.sequence,
                    );
                }
            }
            RuntimeJournalPayload::ToolCallResolved { result } => {
                self.publish_tool_result(result, event.sequence, tool_output);
            }
            RuntimeJournalPayload::CompactionStarted => self.publish(
                compaction_record(
                    "compaction",
                    "Compaction",
                    TrajectoryRecordStatus::Running,
                    event.sequence,
                ),
                event.sequence,
            ),
            RuntimeJournalPayload::CompactionCompleted {
                checkpoint_id,
                covered_history_item_count,
            } => {
                let Some(mut record) = self.active_compaction_record() else {
                    return;
                };
                record.set_summary(Some(format!(
                    "Context checkpoint installed: {checkpoint_id} ({covered_history_item_count} history items)"
                )));
                record.finish(TrajectoryRecordStatus::Completed, event.sequence);
                self.publish(Some(record), event.sequence);
            }
            RuntimeJournalPayload::Cancelled { diagnostic } => {
                self.publish(
                    lifecycle_failure_record(
                        "cancelled",
                        "Run cancelled",
                        TrajectoryRecordStatus::Cancelled,
                        diagnostic.clone(),
                        event.sequence,
                    ),
                    event.sequence,
                );
            }
            RuntimeJournalPayload::Failed { diagnostic } => {
                if diagnostic.code().starts_with("auto_compaction") {
                    self.publish_active_compaction_failure(diagnostic, event.sequence);
                }
                self.publish(
                    lifecycle_failure_record(
                        "failed",
                        "Run failed",
                        TrajectoryRecordStatus::Failed,
                        diagnostic.clone(),
                        event.sequence,
                    ),
                    event.sequence,
                );
            }
            RuntimeJournalPayload::ModelRetryAttemptStarted { attempt, .. } => self.publish(
                lifecycle_record(
                    &format!("retry-{attempt}"),
                    "Model retry",
                    TrajectoryRecordStatus::Running,
                    event.sequence,
                ),
                event.sequence,
            ),
            RuntimeJournalPayload::StepStarted | RuntimeJournalPayload::StepCompleted => {}
            _ => {}
        }
    }

    fn publish_pending_inputs(&self, sequence: u64) {
        let inputs = {
            let mut state = self.lock_state();
            std::mem::take(&mut state.pending_inputs)
        };
        for (index, input) in inputs.into_iter().enumerate() {
            let identity = format!("{sequence}-{index}-{}", input.text);
            let turn_id = {
                let mut state = self.lock_state();
                let turn_id = TrajectoryTurnId::new(state.next_turn_id).ok();
                state.next_turn_id = state.next_turn_id.saturating_add(1);
                state.active_turn_id = turn_id;
                turn_id
            };
            let Some(mut record) = record(
                "input",
                &identity,
                TrajectoryLane::Input,
                TrajectoryRecordKind::UserInput,
                TrajectoryRecordStatus::Completed,
                sequence,
            ) else {
                continue;
            };
            record.set_label("User input".to_owned());
            record.set_sequence_order(index as u32);
            record.set_turn_id(turn_id);
            record.set_summary(Some(truncate_summary(&input.text)));
            record.set_message_details(input.text.clone(), false);
            let record_id = record.id().clone();
            self.publish(Some(record), sequence);
            self.set_active_parent(record_id);
        }
    }

    fn publish_tool_result(
        &self,
        result: &ToolCallResult,
        sequence: u64,
        output: Option<&ToolOutput>,
    ) {
        let existing = self.find_record_by_tool_call(result.call_id());
        let Some(record) = merge_tool_result(existing, result.call_id(), result, sequence, output)
        else {
            return;
        };
        self.publish(Some(record), sequence);
    }

    fn publish(&self, record: Option<TrajectoryRecord>, sequence: u64) {
        let Some(mut record) = record else {
            return;
        };
        let event = {
            let mut state = self.lock_state();
            if state.snapshot.is_closed() {
                return;
            }
            if record.turn_id().is_none() {
                record.set_turn_id(state.active_turn_id);
            }
            if record.parent_id().is_none() && record.lane() != TrajectoryLane::Input {
                let tool_call_id = record.tool_call_id().cloned();
                record.set_relationship(state.active_parent_id.clone(), tool_call_id);
            }
            state.snapshot.advance_latest_sequence(sequence);
            if !state.snapshot.upsert_record(record.clone()) {
                return;
            }
            state.snapshot.advance_revision();
            TrajectoryEvent::RecordUpsert {
                revision: state.snapshot.revision(),
                latest_sequence: state.snapshot.latest_sequence(),
                record: Box::new(record),
            }
        };
        let _ = self.updates.send(event);
    }

    fn publish_active_compaction_failure(&self, diagnostic: &ErrorInfo, sequence: u64) {
        let Some(mut record) = self.active_compaction_record() else {
            return;
        };
        record.set_summary(Some("Context checkpoint failed".to_owned()));
        record.fail(diagnostic.clone(), sequence);
        self.publish(Some(record), sequence);
    }

    fn active_compaction_record(&self) -> Option<TrajectoryRecord> {
        let state = self.lock_state();
        state
            .snapshot
            .records()
            .iter()
            .rev()
            .find(|record| {
                record.kind() == TrajectoryRecordKind::Compaction
                    && record.status() == TrajectoryRecordStatus::Running
            })
            .cloned()
    }

    fn publish_session_closed(&self) {
        let event = {
            let mut state = self.lock_state();
            if state.snapshot.is_closed() {
                return;
            }
            state.snapshot.mark_closed();
            state.snapshot.advance_revision();
            TrajectoryEvent::SessionClosed {
                revision: state.snapshot.revision(),
                latest_sequence: state.snapshot.latest_sequence(),
            }
        };
        let _ = self.updates.send(event);
    }

    fn find_record_by_tool_call(&self, call_id: &ToolCallId) -> Option<TrajectoryRecord> {
        self.lock_state()
            .snapshot
            .records()
            .iter()
            .find(|record| record.tool_call_id() == Some(call_id))
            .cloned()
    }

    fn set_active_parent(&self, parent_id: TrajectoryRecordId) {
        self.lock_state().active_parent_id = Some(parent_id);
    }

    fn lock_state(&self) -> MutexGuard<'_, ProjectionState> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

fn record(
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

fn tool_call_record(
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

fn tool_result_record(
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

fn merge_tool_result(
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

fn lifecycle_record(
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

fn compaction_record(
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

fn typed_lifecycle_record(
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

fn lifecycle_failure_record(
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

fn tool_status(status: ToolCallResultStatus) -> TrajectoryRecordStatus {
    match status {
        ToolCallResultStatus::Succeeded => TrajectoryRecordStatus::Succeeded,
        ToolCallResultStatus::Failed => TrajectoryRecordStatus::Failed,
    }
}

fn insert_without_publish(snapshot: &mut TrajectorySnapshot, record: TrajectoryRecord) {
    snapshot.advance_latest_sequence(record.start_sequence());
    snapshot.advance_revision();
    snapshot.upsert_record(record);
}

fn hydrate_assistant_message_details(snapshot: &mut TrajectorySnapshot, session: &SessionState) {
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

fn record_id(prefix: &str, identity: &str) -> Option<TrajectoryRecordId> {
    let digest = Sha256::digest(identity.as_bytes());
    let digest = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let value = format!("{prefix}-{digest}");
    TrajectoryRecordId::new(&value).ok()
}

fn sequence_from_artifact(artifact_id: &str, prefix: &str) -> Option<u64> {
    artifact_id.strip_prefix(prefix)?.parse().ok()
}

fn prompt_block(content: &str, sequence_order: u32) -> Option<TrajectoryPromptBlock> {
    let identity = format!("{sequence_order}:{content}");
    let id = record_id("prompt", &identity)?;
    Some(TrajectoryPromptBlock::new(
        id,
        sequence_order,
        content.to_owned(),
        false,
    ))
}

fn format_prompt_block(tag: &str, content: &str) -> String {
    format!("<{tag}>{content}</{tag}>")
}

fn truncate_summary(value: &str) -> String {
    const MAX_SUMMARY_CHARS: usize = 512;
    let mut summary = value.chars().take(MAX_SUMMARY_CHARS).collect::<String>();
    if value.chars().count() > MAX_SUMMARY_CHARS {
        summary.push_str("...");
    }
    summary
}

fn trajectory_payload(output: &ToolOutput) -> Option<TrajectoryPayload> {
    let (kind, content) = match output {
        ToolOutput::Text { text } => (TrajectoryPayloadKind::Text, text.as_str()),
        ToolOutput::Json { json } => (TrajectoryPayloadKind::Json, json.as_str()),
    };
    Some(TrajectoryPayload::new(kind, content.to_owned(), false))
}

#[cfg(test)]
#[path = "trajectory_tests.rs"]
mod tests;
