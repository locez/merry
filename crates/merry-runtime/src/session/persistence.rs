use super::{
    ModelTurnId, ModelTurnStatus, PreparedCompactionInstall, PreparedPlanToolCommit,
    PromptHistoryProjection, SessionState,
    checkpoint_window::ArchivedRefManifest,
    transcript::{
        PersistedTranscript, ToolCallPromptProjection, ToolResultPromptProjection, Transcript,
        TranscriptItem,
    },
};
use crate::{
    FileSessionStore, PlanPersistenceLocation, RuntimeError, SessionStoreError, UserMessageInput,
    action_audit::{ActionAuditRegistry, PersistedActionAuditRegistry},
    checkpoint::{CheckpointRef, CheckpointRefId, CheckpointSequenceRange, CheckpointSourceKind},
    context::{
        CompactedCheckpoint, ContextCompiler, ContextEntry, ContextSummary,
        PersistedCompactedCheckpoint, SessionContextSnapshot,
    },
    ledger::{PersistedLedgerEntry, TaskLedger},
    memory::MemoryStore,
    plan::{PersistedPlanState, PlanState},
};
use merry_core::{
    PendingToolCall, PlanSnapshot, SessionId, SessionUsage, ToolCallId, TrajectoryRecordDetails,
    TrajectorySnapshot,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Wire version of the persisted session document.
///
/// Format 6 removed the global artifact registry and stores message/tool
/// payloads directly inside the transcript. Older documents are rejected with
/// [`SessionStoreError::UnsupportedFormatVersion`] instead of migrated.
const CURRENT_SESSION_STATE_FORMAT_VERSION: u32 = 6;

#[derive(Debug, Serialize, Deserialize)]
struct StoredSessionDocumentHeader {
    format_version: u32,
    session_id: SessionId,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSessionDocument {
    format_version: u32,
    session_id: SessionId,
    next_sequence: u64,
    session_started: bool,
    ledger: Vec<PersistedLedgerEntry>,
    compacted_checkpoint: Option<PersistedCompactedCheckpoint>,
    archived_ref_manifest: Vec<StoredArchivedRef>,
    prompt_history_projection: PromptHistoryProjection,
    context_entries: Vec<StoredContextEntry>,
    transcript: PersistedTranscript,
    resolved_tool_calls: Vec<ToolCallId>,
    usage: Option<SessionUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request_token_calibration: Option<crate::token_estimate::RequestTokenCalibration>,
    task_anchor: Option<StoredTaskAnchor>,
    registries: StoredRegistries,
    active_plan: Option<PersistedPlanState>,
    terminal_plans: Vec<PlanSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trajectory_snapshot: Option<TrajectorySnapshot>,
    external_tool_catalog: merry_core::SessionToolCatalog,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredTaskAnchor {
    objective: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredArchivedRef {
    id: String,
    source_kind: CheckpointSourceKind,
    sequence_start: u64,
    sequence_end: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredRegistries {
    action_audits: PersistedActionAuditRegistry,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum StoredContextEntry {
    Summary { id: String, text: String },
}

#[derive(Debug, Clone)]
pub(crate) struct PersistableSessionBundle {
    pub(crate) session_id: SessionId,
    pub(crate) document_bytes: Vec<u8>,
}

struct PersistableSessionView<'a> {
    transcript: &'a Transcript,
    prompt_history_projection: PromptHistoryProjection,
    compacted_checkpoint: Option<&'a CompactedCheckpoint>,
    archived_ref_manifest: &'a ArchivedRefManifest,
    next_sequence: u64,
    session_started: bool,
    ledger: &'a TaskLedger,
    pending_tool_calls: &'a [PendingToolCall],
    resolved_tool_calls: &'a BTreeSet<ToolCallId>,
    active_plan: Option<&'a PlanState>,
    terminal_plans: &'a [PlanSnapshot],
    trajectory_snapshot: Option<&'a TrajectorySnapshot>,
}

impl SessionState {
    #[allow(dead_code)]
    pub(crate) async fn save_to(&self, store: &FileSessionStore) -> Result<(), SessionStoreError> {
        let bundle = self.persistable_bundle()?;
        store.write_bundle(bundle).await
    }

    pub(crate) async fn load_from(
        store: &FileSessionStore,
        session_id: &SessionId,
    ) -> Result<Self, SessionStoreError> {
        let overlay_bytes = store.read_plan_overlay_bytes(session_id).await?;
        let bytes = match store.read_state_bytes(session_id).await {
            Ok(bytes) => bytes,
            Err(SessionStoreError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                let overlay_bytes = overlay_bytes.ok_or_else(|| SessionStoreError::Io {
                    path: store.state_path(session_id),
                    source,
                })?;
                let mut session = Self::new(session_id.clone());
                session.apply_plan_overlay(&overlay_bytes, true)?;
                return Ok(session);
            }
            Err(error) => return Err(error),
        };
        let header: StoredSessionDocumentHeader = serde_json::from_slice(&bytes)?;
        if header.format_version != CURRENT_SESSION_STATE_FORMAT_VERSION {
            return Err(SessionStoreError::UnsupportedFormatVersion {
                actual: header.format_version,
            });
        }
        if &header.session_id != session_id {
            return Err(SessionStoreError::SessionIdMismatch {
                requested: session_id.clone(),
                actual: header.session_id,
            });
        }

        let document: StoredSessionDocument = serde_json::from_slice(&bytes)?;
        let mut session = Self::from_stored_document(document)?;
        if let Some(overlay_bytes) = overlay_bytes {
            session.apply_plan_overlay(&overlay_bytes, false)?;
        }
        Ok(session)
    }

    pub(crate) fn persistable_bundle(&self) -> Result<PersistableSessionBundle, SessionStoreError> {
        self.persistable_bundle_for(PersistableSessionView {
            transcript: &self.transcript,
            prompt_history_projection: self.prompt_history_projection,
            compacted_checkpoint: self.compacted_checkpoint.as_ref(),
            archived_ref_manifest: &self.archived_ref_manifest,
            next_sequence: self.next_sequence,
            session_started: self.session_started,
            ledger: &self.ledger,
            pending_tool_calls: &self.pending_tool_calls,
            resolved_tool_calls: &self.resolved_tool_calls,
            active_plan: self.active_plan.as_ref(),
            terminal_plans: &self.terminal_plans,
            trajectory_snapshot: self.trajectory_snapshot.as_ref(),
        })
    }

    pub(crate) fn persistable_bundle_with_compaction(
        &self,
        prepared: &PreparedCompactionInstall,
        trajectory_snapshot: &TrajectorySnapshot,
    ) -> Result<PersistableSessionBundle, SessionStoreError> {
        self.persistable_bundle_for(PersistableSessionView {
            transcript: prepared.transcript(),
            prompt_history_projection: prepared.prompt_history_projection(),
            compacted_checkpoint: prepared.compacted_checkpoint(),
            archived_ref_manifest: prepared.archived_ref_manifest(),
            next_sequence: self.next_sequence,
            session_started: self.session_started,
            ledger: &self.ledger,
            pending_tool_calls: &self.pending_tool_calls,
            resolved_tool_calls: &self.resolved_tool_calls,
            active_plan: self.active_plan.as_ref(),
            terminal_plans: &self.terminal_plans,
            trajectory_snapshot: Some(trajectory_snapshot),
        })
    }

    pub(crate) fn persistable_bundle_with_plan_tool_commit(
        &self,
        prepared: &PreparedPlanToolCommit,
    ) -> Result<PersistableSessionBundle, SessionStoreError> {
        self.persistable_bundle_for(PersistableSessionView {
            transcript: prepared.transcript(),
            prompt_history_projection: self.prompt_history_projection,
            compacted_checkpoint: self.compacted_checkpoint.as_ref(),
            archived_ref_manifest: &self.archived_ref_manifest,
            next_sequence: prepared.next_sequence(),
            session_started: prepared.session_started(),
            ledger: prepared.ledger(),
            pending_tool_calls: prepared.pending_tool_calls(),
            resolved_tool_calls: prepared.resolved_tool_calls(),
            active_plan: Some(prepared.active_plan()),
            terminal_plans: prepared.terminal_plans(),
            trajectory_snapshot: self.trajectory_snapshot.as_ref(),
        })
    }

    fn persistable_bundle_for(
        &self,
        view: PersistableSessionView<'_>,
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
        if let Some(active_plan) = view.active_plan {
            crate::session_store::validate_plan_snapshot(
                active_plan.snapshot(),
                PlanPersistenceLocation::Active,
            )?;
        }
        for (index, terminal) in view.terminal_plans.iter().enumerate() {
            crate::session_store::validate_plan_snapshot(
                terminal,
                PlanPersistenceLocation::Terminal { index },
            )?;
        }

        let trajectory_snapshot = view.trajectory_snapshot.map(|snapshot| {
            strip_redundant_tool_outputs_for_persistence(snapshot, view.transcript)
        });

        let document = StoredSessionDocument {
            format_version: CURRENT_SESSION_STATE_FORMAT_VERSION,
            session_id: self.session_id.clone(),
            next_sequence: view.next_sequence,
            session_started: view.session_started,
            ledger: view.ledger.persisted_entries(),
            compacted_checkpoint: view
                .compacted_checkpoint
                .map(CompactedCheckpoint::persisted),
            archived_ref_manifest: view
                .archived_ref_manifest
                .refs()
                .iter()
                .map(StoredArchivedRef::from)
                .collect(),
            prompt_history_projection: view.prompt_history_projection,
            context_entries: self
                .context_entries
                .iter()
                .map(StoredContextEntry::from)
                .collect(),
            transcript: view.transcript.persisted(),
            resolved_tool_calls: view.resolved_tool_calls.iter().cloned().collect(),
            usage: self.usage.clone(),
            request_token_calibration: self.request_token_calibration.clone(),
            task_anchor: self.task_anchor.as_ref().map(|anchor| StoredTaskAnchor {
                objective: anchor.objective().to_owned(),
            }),
            registries: StoredRegistries {
                action_audits: self.action_audits.persisted(),
            },
            active_plan: view.active_plan.map(PlanState::persisted),
            terminal_plans: view.terminal_plans.to_vec(),
            trajectory_snapshot,
            external_tool_catalog: self.external_tool_catalog.clone(),
        };
        let document_bytes = serde_json::to_vec_pretty(&document)?;
        Ok(PersistableSessionBundle {
            session_id: self.session_id.clone(),
            document_bytes,
        })
    }

    pub(crate) fn persistable_bundle_if_resume_safe(
        &self,
    ) -> Result<Option<PersistableSessionBundle>, SessionStoreError> {
        if self.pending_tool_calls.is_empty() {
            self.persistable_bundle().map(Some)
        } else {
            Ok(None)
        }
    }

    fn from_stored_document(document: StoredSessionDocument) -> Result<Self, SessionStoreError> {
        if document.format_version != CURRENT_SESSION_STATE_FORMAT_VERSION {
            return Err(SessionStoreError::UnsupportedFormatVersion {
                actual: document.format_version,
            });
        }

        let compacted_checkpoint = document
            .compacted_checkpoint
            .map(CompactedCheckpoint::from_persisted)
            .transpose()
            .map_err(|_| invalid_document("stored compacted checkpoint is invalid"))?;
        let prompt_history_projection = document.prompt_history_projection;
        let archived_ref_manifest = ArchivedRefManifest::new(
            document
                .archived_ref_manifest
                .into_iter()
                .map(CheckpointRef::try_from)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| invalid_document("stored archived ref manifest is invalid"))?,
        )
        .map_err(|_| invalid_document("stored archived ref manifest is invalid"))?;

        let active_plan = document
            .active_plan
            .map(|persisted| {
                PlanState::from_persisted(persisted).map_err(|source| {
                    crate::session_store::invalid_plan(PlanPersistenceLocation::Active, source)
                })
            })
            .transpose()?;
        let mut session = Self {
            session_id: document.session_id,
            next_sequence: document.next_sequence,
            session_started: document.session_started,
            ledger: TaskLedger::from_persisted_entries(document.ledger)
                .map_err(|_| invalid_document("stored ledger entry is invalid"))?,
            memory_store: MemoryStore::new(),
            skill_catalog: None,
            project_rules: None,
            task_anchor: None,
            compacted_checkpoint,
            archived_ref_manifest,
            prompt_history_projection,
            context_entries: document
                .context_entries
                .into_iter()
                .map(ContextEntry::try_from)
                .collect::<Result<Vec<_>, _>>()?,
            activated_memories: Vec::new(),
            action_audits: ActionAuditRegistry::from_persisted(document.registries.action_audits),
            active_plan,
            terminal_plans: document.terminal_plans,
            transcript: Transcript::from_persisted(document.transcript)
                .map_err(runtime_error_to_invalid_document)?,
            pending_tool_calls: Vec::new(),
            resolved_tool_calls: document
                .resolved_tool_calls
                .into_iter()
                .collect::<BTreeSet<_>>(),
            usage: document.usage,
            request_token_calibration: document.request_token_calibration,
            trajectory_snapshot: document.trajectory_snapshot,
            external_tool_catalog: document.external_tool_catalog,
        };

        if let Some(anchor) = document.task_anchor {
            session.set_task_anchor(
                crate::TaskAnchor::new(anchor.objective)
                    .map_err(|_| invalid_document("stored task anchor is invalid"))?,
            );
        }

        session.validate_persisted_transcript_for(
            &session.transcript,
            session.prompt_history_projection,
            session.compacted_checkpoint.as_ref(),
            &session.resolved_tool_calls,
        )?;
        session.validate_persisted_context_entries_with_checkpoint(
            session.compacted_checkpoint.as_ref(),
        )?;
        session
            .validate_archived_ref_manifest()
            .map_err(runtime_error_to_invalid_document)?;
        for (index, terminal) in session.terminal_plans.iter().enumerate() {
            crate::session_store::validate_plan_snapshot(
                terminal,
                PlanPersistenceLocation::Terminal { index },
            )?;
        }
        Ok(session)
    }

    fn validate_persisted_context_entries_with_checkpoint(
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

    fn validate_persisted_transcript_for(
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

        for (turn_id, status) in transcript.persisted().model_turns {
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

/// Drops duplicated tool outputs from persisted trajectory snapshots whenever the
/// same output is already stored in `transcript`, avoiding double persistence.
fn strip_redundant_tool_outputs_for_persistence(
    snapshot: &TrajectorySnapshot,
    transcript: &Transcript,
) -> TrajectorySnapshot {
    let mut stripped = snapshot.clone();
    for mut record in stripped.records().to_vec() {
        let Some(call_id) = record.tool_call_id().cloned() else {
            continue;
        };
        if matches!(record.details(), TrajectoryRecordDetails::Tool { tool } if tool.output().is_some())
            && transcript.tool_result_payload(&call_id).is_some()
        {
            record.set_tool_output(None);
            stripped.upsert_record(record);
        }
    }
    stripped
}

impl FileSessionStore {
    pub(crate) async fn stage_bundle(
        &self,
        bundle: PersistableSessionBundle,
    ) -> Result<crate::session_store::StagedSessionBundle, SessionStoreError> {
        self.stage_state_bytes(&bundle.session_id, &bundle.document_bytes)
            .await
    }

    pub(crate) async fn write_bundle(
        &self,
        bundle: PersistableSessionBundle,
    ) -> Result<(), SessionStoreError> {
        self.stage_bundle(bundle)
            .await?
            .commit()
            .await?
            .require_durable()
    }
}

impl From<&CheckpointRef> for StoredArchivedRef {
    fn from(reference: &CheckpointRef) -> Self {
        Self {
            id: reference.id().as_str().to_owned(),
            source_kind: reference.source_kind(),
            sequence_start: reference.sequence_range().start(),
            sequence_end: reference.sequence_range().end(),
        }
    }
}

impl TryFrom<StoredArchivedRef> for CheckpointRef {
    type Error = crate::CheckpointError;

    fn try_from(reference: StoredArchivedRef) -> Result<Self, Self::Error> {
        Ok(Self::new(
            CheckpointRefId::new(&reference.id)?,
            reference.source_kind,
            CheckpointSequenceRange::new(reference.sequence_start, reference.sequence_end)?,
        ))
    }
}

impl From<&ContextEntry> for StoredContextEntry {
    fn from(value: &ContextEntry) -> Self {
        let summary = value.as_summary();
        Self::Summary {
            id: summary.id().to_owned(),
            text: summary.text().to_owned(),
        }
    }
}

impl TryFrom<StoredContextEntry> for ContextEntry {
    type Error = SessionStoreError;

    fn try_from(value: StoredContextEntry) -> Result<Self, Self::Error> {
        match value {
            StoredContextEntry::Summary { id, text } => Ok(ContextEntry::summary(
                ContextSummary::new(id, text)
                    .map_err(|_| invalid_document("stored context summary is invalid"))?,
            )),
        }
    }
}

fn invalid_document(reason: &'static str) -> SessionStoreError {
    SessionStoreError::InvalidDocument { reason }
}

fn runtime_error_to_invalid_document(_error: RuntimeError) -> SessionStoreError {
    invalid_document("stored transcript is invalid")
}
