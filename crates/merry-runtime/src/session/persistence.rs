use super::{
    ModelTurnId, ModelTurnStatus, PreparedCompactionInstall, PromptHistoryProjection, SessionState,
    checkpoint_window::ArchivedRefManifest,
    journal::{
        JournalAppendInput, JournalCursor, JournalImageFile, PreparedJournalAppend,
        ReplayedJournal, images_dir, journal_file_has_header, prepare_journal_append,
        session_from_journal,
    },
    transcript::{
        ToolCallPromptProjection, ToolResultPromptProjection, Transcript, TranscriptItem,
    },
};
use crate::{
    FileSessionStore, RuntimeError, SessionStoreError, UserMessageInput,
    context::{CompactedCheckpoint, ContextCompiler, SessionContextSnapshot},
    plan::PlanSessionState,
    todo::TodoListState,
};
use merry_core::{PendingToolCall, SessionId, ToolCallId};
use std::collections::{BTreeMap, BTreeSet};

/// Bytes that append one durable journal commit, plus the cursor to adopt after
/// that append is durable.
#[derive(Debug)]
pub(crate) struct PersistableSessionBundle {
    pub(crate) session_id: SessionId,
    pub(crate) header_bytes: Vec<u8>,
    pub(crate) document_bytes: Vec<u8>,
    pub(crate) images: Vec<JournalImageFile>,
    pub(crate) cursor: JournalCursor,
    pub(crate) search_user_text: Option<String>,
}

struct PersistableSessionView<'a> {
    transcript: &'a Transcript,
    prompt_history_projection: PromptHistoryProjection,
    compacted_checkpoint: Option<&'a CompactedCheckpoint>,
    archived_ref_manifest: &'a ArchivedRefManifest,
    next_sequence: u64,
    session_started: bool,
    pending_tool_calls: &'a [PendingToolCall],
    resolved_tool_calls: &'a BTreeSet<ToolCallId>,
    plan_state: &'a PlanSessionState,
    todo_list: &'a TodoListState,
}

impl SessionState {
    #[cfg(test)]
    pub(crate) async fn save_to(
        &mut self,
        store: &FileSessionStore,
    ) -> Result<(), SessionStoreError> {
        // `save_to` adopts the destination, so this store becomes the tracked one.
        let has_header = journal_destination_has_header(store, &self.session_id).await?;
        let cursor = self.cursor_for_destination(true, has_header);
        let bundle = self.persistable_bundle_with_cursor(&cursor)?;
        let cursor = bundle.cursor.clone();
        if !bundle.document_bytes.is_empty() {
            store.write_bundle(bundle).await?;
        }
        self.adopt_journal_cursor(cursor);
        Ok(())
    }

    pub(crate) async fn load_from(
        store: &FileSessionStore,
        session_id: &SessionId,
    ) -> Result<Self, SessionStoreError> {
        match store.read_journal_bytes(session_id).await {
            Ok(bytes) => {
                let ReplayedJournal {
                    mut session,
                    truncate_to_len,
                } = session_from_journal(session_id, &bytes, &images_dir(store, session_id))
                    .await?;
                let mut plan_contents = BTreeMap::new();
                for summary in session.plan_state().summaries() {
                    if let Some(content) =
                        store.read_plan_file(session_id, &summary.file_name).await?
                    {
                        plan_contents.insert(summary.id.clone(), content);
                    }
                }
                session.plan_state_mut().set_contents(plan_contents);
                session.set_tracked_store_dir(store.sessions_dir().to_path_buf());
                if let Some(valid_len) = truncate_to_len
                    && let Err(error) = store.truncate_journal_tail(session_id, valid_len).await
                {
                    tracing::warn!(
                        session_id = %session_id,
                        error = %error,
                        "could not truncate incomplete trailing journal line"
                    );
                }
                return Ok(session);
            }
            Err(SessionStoreError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if let Some(bytes) = store.read_legacy_bytes(session_id, "state.json").await? {
            let actual = legacy_format_version(&bytes).unwrap_or(6);
            return Err(SessionStoreError::UnsupportedFormatVersion { actual });
        }
        Err(SessionStoreError::Io {
            path: store.journal_path(session_id),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "session journal is missing"),
        })
    }

    pub(crate) fn persistable_bundle(&self) -> Result<PersistableSessionBundle, SessionStoreError> {
        self.persistable_bundle_with_cursor(&self.journal_cursor)
    }

    /// The cursor tracks one store. A destination with no header needs a
    /// complete journal, even when this process already flushed a commit
    /// somewhere else.
    pub(crate) fn cursor_for_destination(
        &self,
        tracked_store: bool,
        destination_has_header: bool,
    ) -> JournalCursor {
        if tracked_store && destination_has_header {
            self.journal_cursor.clone()
        } else {
            JournalCursor::untracked()
        }
    }

    pub(crate) fn persistable_bundle_with_cursor(
        &self,
        cursor: &JournalCursor,
    ) -> Result<PersistableSessionBundle, SessionStoreError> {
        self.persistable_bundle_for(
            PersistableSessionView {
                transcript: &self.transcript,
                prompt_history_projection: self.prompt_history_projection,
                compacted_checkpoint: self.compacted_checkpoint.as_ref(),
                archived_ref_manifest: &self.archived_ref_manifest,
                next_sequence: self.next_sequence,
                session_started: self.session_started,
                pending_tool_calls: &self.pending_tool_calls,
                resolved_tool_calls: &self.resolved_tool_calls,
                plan_state: &self.plan_state,
                todo_list: &self.todo_list,
            },
            cursor,
        )
    }

    pub(crate) fn persistable_bundle_with_compaction(
        &self,
        prepared: &PreparedCompactionInstall,
    ) -> Result<PersistableSessionBundle, SessionStoreError> {
        self.persistable_bundle_for(
            PersistableSessionView {
                transcript: prepared.transcript(),
                prompt_history_projection: prepared.prompt_history_projection(),
                compacted_checkpoint: prepared.compacted_checkpoint(),
                archived_ref_manifest: prepared.archived_ref_manifest(),
                next_sequence: self.next_sequence,
                session_started: self.session_started,
                pending_tool_calls: &self.pending_tool_calls,
                resolved_tool_calls: &self.resolved_tool_calls,
                plan_state: &self.plan_state,
                todo_list: &self.todo_list,
            },
            &self.journal_cursor,
        )
    }

    fn persistable_bundle_for(
        &self,
        view: PersistableSessionView<'_>,
        cursor: &JournalCursor,
    ) -> Result<PersistableSessionBundle, SessionStoreError> {
        if !view.pending_tool_calls.is_empty() {
            return Err(SessionStoreError::UnsafePendingToolCalls {
                session_id: self.session_id.clone(),
                pending_count: view.pending_tool_calls.len(),
            });
        }
        self.validate_persisted_transcript_for(
            view.transcript,
            view.prompt_history_projection,
            view.compacted_checkpoint,
            view.resolved_tool_calls,
        )?;
        self.validate_persisted_context_entries_with_checkpoint(view.compacted_checkpoint)?;
        self.validate_archived_ref_manifest_for(
            view.transcript,
            view.prompt_history_projection,
            view.archived_ref_manifest,
        )
        .map_err(runtime_error_to_invalid_document)?;

        let persisted_context_entries = self
            .context_entries
            .iter()
            .filter(|entry| {
                !self
                    .construction_context_ids
                    .contains(entry.as_summary().id())
            })
            .collect::<Vec<_>>();
        let PreparedJournalAppend {
            header_bytes,
            commit_bytes,
            images,
            cursor,
            search_user_text,
        } = prepare_journal_append(JournalAppendInput {
            session_id: &self.session_id,
            cursor,
            transcript: view.transcript,
            prompt_history_projection: view.prompt_history_projection,
            compacted_checkpoint: view.compacted_checkpoint,
            next_sequence: view.next_sequence,
            session_started: view.session_started,
            plan_state: view.plan_state,
            todo_list: view.todo_list,
            usage: self.usage.as_ref(),
            request_token_calibration: self.request_token_calibration.as_ref(),
            task_anchor: self.task_anchor.as_ref(),
            context_entries: &persisted_context_entries,
            topic: self.topic.as_deref(),
            subagents: &self.persisted_subagents,
        })?;
        Ok(PersistableSessionBundle {
            session_id: self.session_id.clone(),
            header_bytes,
            document_bytes: commit_bytes,
            images,
            cursor,
            search_user_text,
        })
    }

    pub(crate) fn persistable_bundle_if_resume_safe(
        &self,
    ) -> Result<Option<PersistableSessionBundle>, SessionStoreError> {
        let has_open_turn = self
            .transcript
            .model_turn_statuses()
            .values()
            .any(|status| status.is_open());
        if self.pending_tool_calls.is_empty() && !has_open_turn {
            self.persistable_bundle().map(Some)
        } else {
            Ok(None)
        }
    }

    pub(super) fn validate_persisted_context_entries_with_checkpoint(
        &self,
        compacted_checkpoint: Option<&CompactedCheckpoint>,
    ) -> Result<(), SessionStoreError> {
        let snapshot = SessionContextSnapshot::new(
            self.context_entries.clone(),
            Vec::new(),
            compacted_checkpoint.cloned(),
        );
        ContextCompiler::new()
            .compile(&snapshot)
            .map(|_| ())
            .map_err(|_| invalid_document("stored context entry is invalid"))
    }

    pub(super) fn validate_persisted_transcript_for(
        &self,
        transcript: &Transcript,
        prompt_history_projection: PromptHistoryProjection,
        compacted_checkpoint: Option<&CompactedCheckpoint>,
        resolved_tool_calls: &BTreeSet<ToolCallId>,
    ) -> Result<(), SessionStoreError> {
        transcript
            .model_turns()
            .map_err(runtime_error_to_invalid_document)?;
        prompt_history_projection
            .validate(transcript, compacted_checkpoint)
            .map_err(runtime_error_to_invalid_document)?;
        let mut calls_by_turn =
            BTreeMap::<ModelTurnId, BTreeMap<ToolCallId, ToolCallPromptProjection>>::new();
        let mut results_by_turn =
            BTreeMap::<ModelTurnId, BTreeMap<ToolCallId, ToolResultPromptProjection>>::new();

        for item in transcript.items() {
            match item {
                TranscriptItem::UserMessage { text, images, .. } => {
                    UserMessageInput::new(text.as_ref(), images.clone()).map_err(|_| {
                        invalid_document("stored user transcript message is invalid")
                    })?;
                }
                TranscriptItem::AssistantText { .. } => {}
                TranscriptItem::ToolCall {
                    model_turn_id,
                    call,
                    prompt_projection,
                    ..
                } => {
                    calls_by_turn
                        .entry(*model_turn_id)
                        .or_default()
                        .insert(call.id().clone(), *prompt_projection);
                }
                TranscriptItem::ToolResult {
                    model_turn_id,
                    call_id,
                    result,
                    payload,
                    prompt_projection,
                    ..
                } => {
                    if result.call_id() != call_id {
                        return Err(invalid_document(
                            "stored transcript tool result identity is inconsistent",
                        ));
                    }
                    if payload.full().as_str().trim().is_empty() {
                        return Err(invalid_document(
                            "stored transcript tool result output is blank",
                        ));
                    }
                    if let Some(model) = payload.model()
                        && model.as_str().trim().is_empty()
                    {
                        return Err(invalid_document("stored model tool result output is blank"));
                    }
                    if !resolved_tool_calls.contains(call_id) {
                        return Err(invalid_document(
                            "stored transcript tool result is not marked resolved",
                        ));
                    }
                    if results_by_turn
                        .entry(*model_turn_id)
                        .or_default()
                        .insert(call_id.clone(), *prompt_projection)
                        .is_some()
                    {
                        return Err(invalid_document(
                            "stored transcript contains duplicate tool results",
                        ));
                    }
                }
            }
        }

        for (&turn_id, &status) in transcript.model_turn_statuses() {
            let calls = calls_by_turn.get(&turn_id);
            let results = results_by_turn.get(&turn_id);
            match status {
                ModelTurnStatus::InProgress | ModelTurnStatus::AwaitingToolResults => {
                    return Err(invalid_document(
                        "stored transcript contains a nonterminal model turn",
                    ));
                }
                ModelTurnStatus::Completed => {
                    let projections_are_consistent = match (calls, results) {
                        (None, None) => true,
                        (Some(calls), Some(results)) if calls.keys().eq(results.keys()) => {
                            calls.iter().all(|(call_id, call_projection)| {
                                let result_projection = results
                                    .get(call_id)
                                    .expect("completed call/result key sets were compared");
                                matches!(
                                    (call_projection, result_projection),
                                    (
                                        ToolCallPromptProjection::Full,
                                        ToolResultPromptProjection::Full
                                            | ToolResultPromptProjection::Collapsed
                                    ) | (
                                        ToolCallPromptProjection::Hidden,
                                        ToolResultPromptProjection::Hidden
                                    )
                                )
                            })
                        }
                        (Some(_), None) | (None, Some(_)) | (Some(_), Some(_)) => false,
                    };
                    if !projections_are_consistent {
                        return Err(invalid_document(
                            "stored completed model turn has unresolved calls or inconsistent projections",
                        ));
                    }
                }
                ModelTurnStatus::Aborted => {
                    if calls.is_some_and(|calls| !calls.is_empty())
                        || results.is_some_and(|results| !results.is_empty())
                    {
                        return Err(invalid_document(
                            "stored aborted model turn contains tool exchange state",
                        ));
                    }
                }
            }
        }

        Ok(())
    }

    pub(crate) fn session_id(&self) -> &SessionId {
        &self.session_id
    }
}

impl FileSessionStore {
    pub(crate) async fn stage_bundle(
        &self,
        bundle: PersistableSessionBundle,
    ) -> Result<crate::session_store::StagedSessionBundle, SessionStoreError> {
        self.stage_journal_append(
            &bundle.session_id,
            bundle.header_bytes,
            bundle.document_bytes,
            &bundle.images,
            bundle.search_user_text,
        )
        .await
    }

    pub(crate) async fn write_bundle(
        &self,
        bundle: PersistableSessionBundle,
    ) -> Result<(), SessionStoreError> {
        if bundle.document_bytes.is_empty() {
            return Ok(());
        }
        self.stage_bundle(bundle)
            .await?
            .commit()
            .await?
            .require_durable()
    }
}

pub(crate) async fn journal_destination_has_header(
    store: &FileSessionStore,
    session_id: &SessionId,
) -> Result<bool, SessionStoreError> {
    match store.read_journal_bytes(session_id).await {
        Ok(bytes) => journal_file_has_header(&bytes, session_id),
        Err(SessionStoreError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

pub(super) fn invalid_document(reason: &'static str) -> SessionStoreError {
    SessionStoreError::InvalidDocument { reason }
}

pub(super) fn runtime_error_to_invalid_document(_error: RuntimeError) -> SessionStoreError {
    invalid_document("stored transcript is invalid")
}

fn legacy_format_version(bytes: &[u8]) -> Option<u32> {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()?
        .get("format_version")?
        .as_u64()?
        .try_into()
        .ok()
}
