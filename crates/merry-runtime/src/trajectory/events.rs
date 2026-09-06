//! Journal-to-trajectory reduction under the owning observability state lock.

use crate::trajectory::{
    RuntimeObservability,
    records::{
        compaction_record, lifecycle_failure_record, lifecycle_record, merge_tool_result, record,
        tool_call_record, truncate_summary,
    },
};
use merry_core::{
    ErrorInfo, RuntimeJournalEvent, RuntimeJournalPayload, ToolCallResult, ToolOutput,
    TrajectoryLane, TrajectoryRecordKind, TrajectoryRecordStatus, TrajectoryTurnId,
};

impl RuntimeObservability {
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

    pub(super) fn publish_pending_inputs(&self, sequence: u64) {
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

    pub(super) fn publish_tool_result(
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

    pub(super) fn publish_active_compaction_failure(&self, diagnostic: &ErrorInfo, sequence: u64) {
        let Some(mut record) = self.active_compaction_record() else {
            return;
        };
        record.set_summary(Some("Context checkpoint failed".to_owned()));
        record.fail(diagnostic.clone(), sequence);
        self.publish(Some(record), sequence);
    }
}
