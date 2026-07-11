use super::{
    ModelTurnId, ModelTurnStatus, PromptHistoryProjection, SessionState,
    checkpoint_window::ArchivedRefManifest,
    transcript::{
        PersistedTranscript, PersistedTranscriptV1, ToolCallPromptProjection,
        ToolResultPromptProjection, Transcript, TranscriptItem, TranscriptV1MigrationError,
    },
};
use crate::{
    FileSessionStore, RuntimeError, SessionStoreError,
    action_audit::{ActionAuditRegistry, PersistedActionAuditRegistry},
    artifact::{ArtifactContent, ArtifactRegistry, PersistedArtifactRecord},
    checkpoint::{CheckpointRef, CheckpointRefId, CheckpointSequenceRange, CheckpointSourceKind},
    context::{
        CompactedCheckpoint, ContextCompiler, ContextEntry, ContextError, ContextEvidence,
        ContextSummary, PersistedCompactedCheckpoint, SessionContextSnapshot,
    },
    judgment::{JudgmentRegistry, PersistedJudgmentRegistry},
    ledger::{PersistedLedgerEntry, TaskLedger},
    memory::MemoryStore,
    summary_draft_promotion::{
        PersistedSummaryDraftPromotionRegistry, SummaryDraftPromotionRegistry,
    },
};
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, EvidenceRef, SessionId, SessionUsage, ToolCallId,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const LEGACY_SESSION_STATE_FORMAT_VERSION: u32 = 1;
const SESSION_STATE_FORMAT_VERSION: u32 = 2;

#[derive(Debug, Serialize, Deserialize)]
struct StoredSessionDocumentHeader {
    format_version: u32,
    session_id: SessionId,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSessionDocument<T> {
    format_version: u32,
    session_id: SessionId,
    next_sequence: u64,
    session_started: bool,
    ledger: Vec<PersistedLedgerEntry>,
    artifacts: Vec<StoredArtifact>,
    compacted_checkpoint: Option<PersistedCompactedCheckpoint>,
    #[serde(default)]
    archived_ref_manifest: Vec<StoredArchivedRef>,
    #[serde(default)]
    prompt_history_projection: Option<PromptHistoryProjection>,
    context_entries: Vec<StoredContextEntry>,
    transcript: T,
    resolved_tool_calls: Vec<ToolCallId>,
    usage: Option<SessionUsage>,
    task_anchor: Option<StoredTaskAnchor>,
    registries: StoredRegistries,
}

type StoredSessionDocumentV1 = StoredSessionDocument<PersistedTranscriptV1>;
type StoredSessionDocumentV2 = StoredSessionDocument<PersistedTranscript>;

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
    evidence: EvidenceRef,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredRegistries {
    judgments: PersistedJudgmentRegistry,
    summary_draft_promotions: PersistedSummaryDraftPromotionRegistry,
    action_audits: PersistedActionAuditRegistry,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredArtifact {
    artifact: ArtifactRef,
    content: ArtifactContent,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum StoredContextEntry {
    Summary {
        id: String,
        text: String,
        evidence: Vec<StoredContextEvidence>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredContextEvidence {
    label: String,
    reference: EvidenceRef,
}

#[derive(Debug, Clone)]
pub(crate) struct PersistableSessionBundle {
    pub(crate) session_id: SessionId,
    pub(crate) document_bytes: Vec<u8>,
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
        let bytes = store.read_state_bytes(session_id).await?;
        let header: StoredSessionDocumentHeader = serde_json::from_slice(&bytes)?;
        if !matches!(
            header.format_version,
            LEGACY_SESSION_STATE_FORMAT_VERSION | SESSION_STATE_FORMAT_VERSION
        ) {
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

        match header.format_version {
            LEGACY_SESSION_STATE_FORMAT_VERSION => {
                let document: StoredSessionDocumentV1 = serde_json::from_slice(&bytes)?;
                Self::from_stored_document_v1(document)
            }
            SESSION_STATE_FORMAT_VERSION => {
                let document: StoredSessionDocumentV2 = serde_json::from_slice(&bytes)?;
                Self::from_stored_document(document)
            }
            _ => unreachable!("supported session format version checked before body decode"),
        }
    }

    pub(crate) fn persistable_bundle(&self) -> Result<PersistableSessionBundle, SessionStoreError> {
        if !self.pending_tool_calls.is_empty() {
            return Err(SessionStoreError::UnsafePendingToolCalls {
                session_id: self.session_id.clone(),
                pending_count: self.pending_tool_calls.len(),
            });
        }
        self.validate_persisted_transcript()?;
        self.validate_persisted_context_entries()?;
        self.validate_persisted_checkpoint_evidence()?;
        self.validate_archived_ref_manifest()
            .map_err(runtime_error_to_invalid_document)?;

        let artifacts = self
            .artifacts
            .persisted_records()
            .into_iter()
            .map(StoredArtifact::from)
            .collect::<Vec<_>>();

        let document = StoredSessionDocument {
            format_version: SESSION_STATE_FORMAT_VERSION,
            session_id: self.session_id.clone(),
            next_sequence: self.next_sequence,
            session_started: self.session_started,
            ledger: self.ledger.persisted_entries(),
            artifacts,
            compacted_checkpoint: self
                .compacted_checkpoint
                .as_ref()
                .map(CompactedCheckpoint::persisted),
            archived_ref_manifest: self
                .archived_ref_manifest
                .refs()
                .iter()
                .map(StoredArchivedRef::from)
                .collect(),
            prompt_history_projection: Some(self.prompt_history_projection),
            context_entries: self
                .context_entries
                .iter()
                .map(StoredContextEntry::from)
                .collect(),
            transcript: self.transcript.persisted(),
            resolved_tool_calls: self.resolved_tool_calls.iter().cloned().collect(),
            usage: self.usage.clone(),
            task_anchor: self.task_anchor.as_ref().map(|anchor| StoredTaskAnchor {
                objective: anchor.objective().to_owned(),
            }),
            registries: StoredRegistries {
                judgments: self.judgments.persisted(),
                summary_draft_promotions: self.summary_draft_promotions.persisted(),
                action_audits: self.action_audits.persisted(),
            },
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

    fn from_stored_document(document: StoredSessionDocumentV2) -> Result<Self, SessionStoreError> {
        if document.format_version != SESSION_STATE_FORMAT_VERSION {
            return Err(SessionStoreError::UnsupportedFormatVersion {
                actual: document.format_version,
            });
        }

        let compacted_checkpoint = document
            .compacted_checkpoint
            .map(CompactedCheckpoint::from_persisted)
            .transpose()
            .map_err(|error| match error {
                ContextError::Checkpoint {
                    source: crate::CheckpointError::LegacyExcerptRefUnsupported { .. },
                } => invalid_document("legacy checkpoint excerpt refs are unsupported"),
                _ => invalid_document("stored compacted checkpoint is invalid"),
            })?;
        let prompt_history_projection = match document.prompt_history_projection {
            Some(projection) => projection,
            None if compacted_checkpoint
                .as_ref()
                .is_some_and(|checkpoint| checkpoint.citation_backed().is_some()) =>
            {
                return Err(invalid_document(
                    "stored compacted V2 session has no prompt history projection",
                ));
            }
            None => PromptHistoryProjection::default(),
        };
        let archived_ref_manifest = ArchivedRefManifest::new(
            document
                .archived_ref_manifest
                .into_iter()
                .map(CheckpointRef::try_from)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| invalid_document("stored archived ref manifest is invalid"))?,
        )
        .map_err(|_| invalid_document("stored archived ref manifest is invalid"))?;

        let artifacts = document
            .artifacts
            .into_iter()
            .map(PersistedArtifactRecord::from)
            .collect();
        let mut session = Self {
            session_id: document.session_id,
            next_sequence: document.next_sequence,
            session_started: document.session_started,
            ledger: TaskLedger::from_persisted_entries(document.ledger)
                .map_err(|_| invalid_document("stored ledger entry is invalid"))?,
            artifacts: ArtifactRegistry::from_persisted_records(artifacts)
                .map_err(|_| invalid_document("stored artifact registry is invalid"))?,
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
            judgments: JudgmentRegistry::from_persisted(document.registries.judgments)
                .map_err(|_| invalid_document("stored judgment registry is invalid"))?,
            summary_draft_promotions: SummaryDraftPromotionRegistry::from_persisted(
                document.registries.summary_draft_promotions,
            )
            .map_err(|_| invalid_document("stored summary draft promotion registry is invalid"))?,
            action_audits: ActionAuditRegistry::from_persisted(document.registries.action_audits),
            transcript: Transcript::from_persisted(document.transcript)
                .map_err(runtime_error_to_invalid_document)?,
            pending_tool_calls: Vec::new(),
            resolved_tool_calls: document
                .resolved_tool_calls
                .into_iter()
                .collect::<BTreeSet<_>>(),
            usage: document.usage,
        };

        if let Some(anchor) = document.task_anchor {
            session.set_task_anchor(
                crate::TaskAnchor::new(anchor.objective)
                    .map_err(|_| invalid_document("stored task anchor is invalid"))?,
            );
        }

        session.validate_persisted_transcript()?;
        session.validate_persisted_context_entries()?;
        session.validate_persisted_checkpoint_evidence()?;
        session
            .validate_archived_ref_manifest()
            .map_err(runtime_error_to_invalid_document)?;
        Ok(session)
    }

    fn from_stored_document_v1(
        document: StoredSessionDocumentV1,
    ) -> Result<Self, SessionStoreError> {
        if document.format_version != LEGACY_SESSION_STATE_FORMAT_VERSION {
            return Err(SessionStoreError::UnsupportedFormatVersion {
                actual: document.format_version,
            });
        }
        if document.compacted_checkpoint.is_some() {
            return Err(SessionStoreError::LegacyCompactedHistoryUnavailable {
                session_id: document.session_id,
            });
        }

        let StoredSessionDocument {
            format_version: _,
            session_id,
            next_sequence,
            session_started,
            ledger,
            artifacts,
            compacted_checkpoint: _,
            archived_ref_manifest: _,
            prompt_history_projection: _,
            context_entries,
            transcript,
            resolved_tool_calls,
            usage,
            task_anchor,
            registries,
        } = document;

        let artifacts = artifacts
            .into_iter()
            .map(PersistedArtifactRecord::from)
            .collect();
        let artifacts = ArtifactRegistry::from_persisted_records(artifacts)
            .map_err(|_| invalid_document("stored artifact registry is invalid"))?;
        let (transcript, artifacts) = Transcript::from_persisted_v1(transcript, &artifacts)
            .map_err(|error| match error {
                TranscriptV1MigrationError::Artifact(
                    crate::artifact::ArtifactError::DuplicateId { id },
                ) => SessionStoreError::LegacyUserArtifactCollision { artifact_id: id },
                TranscriptV1MigrationError::Artifact(_) => {
                    invalid_document("legacy user message artifact is invalid")
                }
                TranscriptV1MigrationError::Transcript(error) => {
                    tracing::debug!(
                        error = %error,
                        "legacy session transcript migration rejected invalid transcript"
                    );
                    invalid_document("legacy transcript is invalid")
                }
            })?;

        Self::from_stored_document(StoredSessionDocument {
            format_version: SESSION_STATE_FORMAT_VERSION,
            session_id,
            next_sequence,
            session_started,
            ledger,
            artifacts: artifacts
                .persisted_records()
                .into_iter()
                .map(StoredArtifact::from)
                .collect(),
            compacted_checkpoint: None,
            archived_ref_manifest: Vec::new(),
            prompt_history_projection: Some(PromptHistoryProjection::default()),
            context_entries,
            transcript: transcript.persisted(),
            resolved_tool_calls,
            usage,
            task_anchor,
            registries,
        })
    }

    fn validate_persisted_context_entries(&self) -> Result<(), SessionStoreError> {
        let snapshot = SessionContextSnapshot::new(
            self.context_entries.clone(),
            self.artifacts.clone(),
            Vec::new(),
            self.compacted_checkpoint.clone(),
        );
        ContextCompiler::new()
            .compile(&snapshot)
            .map(|_| ())
            .map_err(|_| invalid_document("stored context evidence is invalid"))
    }

    fn validate_persisted_checkpoint_evidence(&self) -> Result<(), SessionStoreError> {
        let Some(checkpoint) = self.compacted_checkpoint.as_ref() else {
            return Ok(());
        };
        self.validate_compacted_checkpoint_evidence(checkpoint)
            .map_err(|_| invalid_document("stored checkpoint evidence is invalid"))
    }

    fn validate_persisted_transcript(&self) -> Result<(), SessionStoreError> {
        self.transcript
            .model_turns()
            .map_err(runtime_error_to_invalid_document)?;
        self.validate_prompt_history_projection()
            .map_err(runtime_error_to_invalid_document)?;
        let mut calls_by_turn =
            BTreeMap::<ModelTurnId, BTreeMap<ToolCallId, ToolCallPromptProjection>>::new();
        let mut results_by_turn =
            BTreeMap::<ModelTurnId, BTreeMap<ToolCallId, ToolResultPromptProjection>>::new();
        let validate_text_artifact = |artifact_id: &ArtifactId| -> Result<(), SessionStoreError> {
            let artifact = self
                .artifacts
                .read_ref(artifact_id)
                .map_err(|_| invalid_document("stored transcript artifact is missing"))?;
            let content = self
                .artifacts
                .read_content(artifact_id)
                .map_err(|_| invalid_document("stored transcript artifact is missing"))?;
            if artifact.kind() != &ArtifactKind::Text || content.as_text().is_none() {
                return Err(invalid_document(
                    "stored user or assistant transcript artifact is not text",
                ));
            }
            Ok(())
        };

        for item in self.transcript.items() {
            match item {
                TranscriptItem::UserMessage {
                    id, artifact_id, ..
                } => {
                    if artifact_id != &super::artifacts::user_message_id(*id) {
                        return Err(invalid_document(
                            "stored user transcript artifact identity is inconsistent",
                        ));
                    }
                    validate_text_artifact(artifact_id)?;
                }
                TranscriptItem::AssistantText { artifact_id, .. } => {
                    validate_text_artifact(artifact_id)?;
                }
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
                    artifact_id,
                    prompt_projection,
                    ..
                } => {
                    if result.call_id() != call_id || result.artifact().id() != artifact_id {
                        return Err(invalid_document(
                            "stored transcript tool result identity is inconsistent",
                        ));
                    }
                    let artifact = self.artifacts.read_ref(artifact_id).map_err(|_| {
                        invalid_document("stored transcript tool result artifact is missing")
                    })?;
                    let content = self.artifacts.read_content(artifact_id).map_err(|_| {
                        invalid_document("stored transcript tool result artifact is missing")
                    })?;
                    if artifact != result.artifact()
                        || !matches!(artifact.kind(), ArtifactKind::Text | ArtifactKind::Json)
                        || content.as_text().is_none()
                    {
                        return Err(invalid_document(
                            "stored transcript tool result artifact is inconsistent",
                        ));
                    }
                    if !self.resolved_tool_calls.contains(call_id) {
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

        for (turn_id, status) in self.transcript.persisted().model_turns {
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
                                            | ToolResultPromptProjection::ArtifactNotice
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
    pub(crate) async fn write_bundle(
        &self,
        bundle: PersistableSessionBundle,
    ) -> Result<(), SessionStoreError> {
        self.write_state_bytes(&bundle.session_id, &bundle.document_bytes)
            .await
    }
}

impl From<PersistedArtifactRecord> for StoredArtifact {
    fn from(value: PersistedArtifactRecord) -> Self {
        Self {
            artifact: value.artifact,
            content: value.content,
        }
    }
}

impl From<StoredArtifact> for PersistedArtifactRecord {
    fn from(value: StoredArtifact) -> Self {
        Self {
            artifact: value.artifact,
            content: value.content,
        }
    }
}

impl From<&CheckpointRef> for StoredArchivedRef {
    fn from(reference: &CheckpointRef) -> Self {
        Self {
            id: reference.id().as_str().to_owned(),
            source_kind: reference.source_kind(),
            sequence_start: reference.sequence_range().start(),
            sequence_end: reference.sequence_range().end(),
            evidence: reference.evidence().clone(),
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
            reference.evidence,
        ))
    }
}

impl From<&ContextEntry> for StoredContextEntry {
    fn from(value: &ContextEntry) -> Self {
        let summary = value.as_summary();
        Self::Summary {
            id: summary.id().to_owned(),
            text: summary.text().to_owned(),
            evidence: summary
                .evidence()
                .iter()
                .map(|item| StoredContextEvidence {
                    label: item.label().to_owned(),
                    reference: item.reference().clone(),
                })
                .collect(),
        }
    }
}

impl TryFrom<StoredContextEntry> for ContextEntry {
    type Error = SessionStoreError;

    fn try_from(value: StoredContextEntry) -> Result<Self, Self::Error> {
        match value {
            StoredContextEntry::Summary { id, text, evidence } => {
                let evidence = evidence
                    .into_iter()
                    .map(|item| {
                        ContextEvidence::new(item.label, item.reference)
                            .map_err(|_| invalid_document("stored context evidence is invalid"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(ContextEntry::summary(
                    ContextSummary::new(id, text, evidence)
                        .map_err(|_| invalid_document("stored context summary is invalid"))?,
                ))
            }
        }
    }
}

fn invalid_document(reason: &'static str) -> SessionStoreError {
    SessionStoreError::InvalidDocument { reason }
}

fn runtime_error_to_invalid_document(_error: RuntimeError) -> SessionStoreError {
    invalid_document("stored transcript is invalid")
}
