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
        if let Some(cursor) = write_resume_safe_savepoint(self).await {
            self.session.lock().await.adopt_journal_cursor(cursor);
        }
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
        let cursor = write_resume_safe_savepoint(self).await;
        permit.send(batch);
        if let Some(cursor) = cursor {
            self.session.lock().await.adopt_journal_cursor(cursor);
        }
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

pub(super) async fn write_resume_safe_savepoint(
    inner: &RuntimeInner,
) -> Option<crate::session::JournalCursor> {
    if inner.tool_batch_active.load(Ordering::Acquire) {
        return None;
    }
    let store = inner.session_store.clone()?;
    let prepared = {
        let session = inner.session.lock().await;
        session.persistable_bundle_if_resume_safe()
    };
    let (bundle, cursor) = match prepared {
        Ok(Some(bundle)) => {
            let cursor = bundle.cursor.clone();
            (bundle, cursor)
        }
        Ok(None) => return None,
        Err(error) => {
            tracing::warn!(
                session_id = %inner.session_id,
                error = %error,
                "automatic session resume savepoint skipped"
            );
            return None;
        }
    };
    if bundle.document_bytes.is_empty() {
        return Some(cursor);
    }
    if let Err(error) = store.write_bundle(bundle).await {
        tracing::warn!(
            session_id = %inner.session_id,
            error = %error,
            "automatic session resume savepoint failed"
        );
        return None;
    }
    Some(cursor)
}
