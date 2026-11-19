use super::RuntimeInner;
use crate::events::RuntimeJournalEventBatch;
use crate::trajectory::TRAJECTORY_TOOL_PREVIEW_BYTES;
use merry_core::{RuntimeJournalEvent, RuntimeJournalPayload, ToolCallId};
use std::sync::atomic::Ordering;
use tokio::sync::mpsc;

impl RuntimeInner {
    /// Projects recorded journal events into the in-memory trajectory.
    pub(super) fn project_journal_events(&self, events: &[RuntimeJournalEvent]) {
        for event in events {
            self.trajectory.observe_journal_event(event);
        }
    }

    /// Projects events and refreshes the resume savepoint after session state is recorded.
    pub(super) async fn commit_journal_events(&self, events: &[RuntimeJournalEvent]) {
        self.project_journal_events_with_tool_outputs(events).await;
        persist_resume_safe_savepoint_if_configured(self).await;
    }

    pub(super) fn project_journal_batch(&self, batch: &RuntimeJournalEventBatch) {
        batch.for_each(|event| self.trajectory.observe_journal_event(event));
    }

    pub(super) fn emit_journal_batch(
        &self,
        permit: mpsc::Permit<'_, RuntimeJournalEventBatch>,
        batch: RuntimeJournalEventBatch,
    ) {
        self.project_journal_batch(&batch);
        permit.send(batch);
    }

    pub(super) async fn emit_journal_batch_after_savepoint(
        &self,
        permit: mpsc::Permit<'_, RuntimeJournalEventBatch>,
        batch: RuntimeJournalEventBatch,
    ) {
        let mut events = Vec::new();
        batch.for_each(|event| events.push(event.clone()));
        self.project_journal_events_with_tool_outputs(&events).await;
        persist_resume_safe_savepoint_if_configured(self).await;
        permit.send(batch);
    }
}

impl RuntimeInner {
    async fn project_journal_events_with_tool_outputs(&self, events: &[RuntimeJournalEvent]) {
        let tool_call_ids = events
            .iter()
            .filter_map(|event| match &event.payload {
                RuntimeJournalPayload::ToolCallResolved { result } => {
                    Some(result.call_id().clone())
                }
                _ => None,
            })
            .collect::<Vec<ToolCallId>>();
        let tool_outputs = if tool_call_ids.is_empty() {
            Vec::new()
        } else {
            let session = self.session.lock().await;
            tool_call_ids
                .into_iter()
                .filter_map(|call_id| {
                    session
                        .read_model_tool_preview(&call_id, TRAJECTORY_TOOL_PREVIEW_BYTES)
                        .map(|preview| (call_id, preview))
                })
                .collect::<Vec<_>>()
        };
        for event in events {
            let assistant_text = match &event.payload {
                RuntimeJournalPayload::AssistantOutputRecorded { text } => Some(text.as_str()),
                _ => None,
            };
            let tool_output = match &event.payload {
                RuntimeJournalPayload::ToolCallResolved { result } => tool_outputs
                    .iter()
                    .find(|(call_id, _)| call_id == result.call_id())
                    .map(|(_, output)| output),
                _ => None,
            };
            self.trajectory
                .observe_journal_event_with_contents(event, assistant_text, tool_output);
        }
    }
}

async fn persist_resume_safe_savepoint_if_configured(inner: &RuntimeInner) {
    if inner.tool_batch_active.load(Ordering::Acquire) {
        return;
    }
    let Some(store) = inner.session_store.clone() else {
        return;
    };
    let trajectory = inner.trajectory.snapshot();
    let bundle = {
        let mut session = inner.session.lock().await;
        session.set_trajectory_snapshot(trajectory);
        session.persistable_bundle_if_resume_safe()
    };
    let bundle = match bundle {
        Ok(Some(bundle)) => bundle,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(
                session_id = %inner.session_id,
                error = %error,
                "automatic session resume savepoint skipped"
            );
            return;
        }
    };
    if let Err(error) = store.write_bundle(bundle).await {
        tracing::warn!(
            session_id = %inner.session_id,
            error = %error,
            "automatic session resume savepoint failed"
        );
    }
}
