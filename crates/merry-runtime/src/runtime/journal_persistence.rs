use super::RuntimeInner;
use crate::ArtifactContent;
use crate::events::RuntimeJournalEventBatch;
use merry_core::{ArtifactId, RuntimeJournalEvent, RuntimeJournalPayload, ToolOutput};
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
        self.project_journal_events_with_artifact_contents(events)
            .await;
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
        self.project_journal_events_with_artifact_contents(&events)
            .await;
        persist_resume_safe_savepoint_if_configured(self).await;
        permit.send(batch);
    }
}

impl RuntimeInner {
    async fn project_journal_events_with_artifact_contents(&self, events: &[RuntimeJournalEvent]) {
        let assistant_artifact_ids = events
            .iter()
            .filter_map(|event| match &event.payload {
                RuntimeJournalPayload::AssistantOutputRecorded { artifact } => {
                    Some(artifact.id().clone())
                }
                _ => None,
            })
            .collect::<Vec<ArtifactId>>();
        let tool_result_artifact_ids = events
            .iter()
            .filter_map(|event| match &event.payload {
                RuntimeJournalPayload::ToolCallResolved { result } => {
                    Some(result.artifact().id().clone())
                }
                _ => None,
            })
            .collect::<Vec<ArtifactId>>();
        let (assistant_texts, tool_outputs) =
            if assistant_artifact_ids.is_empty() && tool_result_artifact_ids.is_empty() {
                (Vec::new(), Vec::new())
            } else {
                let session = self.session.lock().await;
                let assistant_texts = assistant_artifact_ids
                    .into_iter()
                    .filter_map(|artifact_id| {
                        session
                            .read_artifact_content(&artifact_id)
                            .ok()
                            .and_then(|content| content.as_text().map(str::to_owned))
                            .map(|text| (artifact_id, text))
                    })
                    .collect::<Vec<_>>();
                let tool_outputs = tool_result_artifact_ids
                    .into_iter()
                    .filter_map(|artifact_id| {
                        session
                            .read_artifact_content(&artifact_id)
                            .ok()
                            .and_then(tool_output_from_content)
                            .map(|output| (artifact_id, output))
                    })
                    .collect::<Vec<_>>();
                (assistant_texts, tool_outputs)
            };
        for event in events {
            let assistant_text = match &event.payload {
                RuntimeJournalPayload::AssistantOutputRecorded { artifact } => assistant_texts
                    .iter()
                    .find(|(artifact_id, _)| artifact_id == artifact.id())
                    .map(|(_, text)| text.as_str()),
                _ => None,
            };
            let tool_output = match &event.payload {
                RuntimeJournalPayload::ToolCallResolved { result } => tool_outputs
                    .iter()
                    .find(|(artifact_id, _)| artifact_id == result.artifact().id())
                    .map(|(_, output)| output),
                _ => None,
            };
            self.trajectory
                .observe_journal_event_with_contents(event, assistant_text, tool_output);
        }
    }
}

fn tool_output_from_content(content: ArtifactContent) -> Option<ToolOutput> {
    match content {
        ArtifactContent::Text { content: text } => Some(ToolOutput::Text { text }),
        ArtifactContent::Json { content: json } => Some(ToolOutput::Json { json }),
        ArtifactContent::Binary { .. }
        | ArtifactContent::Image { .. }
        | ArtifactContent::Other { .. } => None,
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
