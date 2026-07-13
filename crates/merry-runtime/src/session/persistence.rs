use super::{
    ModelTurnId, ModelTurnStatus, PreparedCompactionInstall, PreparedPlanToolCommit,
    PromptHistoryProjection, SessionState,
    checkpoint_window::ArchivedRefManifest,
    plan_persistence::validate_plan_snapshot_refs,
    transcript::{
        PersistedTranscript, PersistedTranscriptV1, ToolCallPromptProjection,
        ToolResultPromptProjection, Transcript, TranscriptItem, TranscriptV1MigrationError,
    },
};
use crate::{
    FileSessionStore, RuntimeError, SessionStoreError, UserImageInput, UserMessageInput,
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
    plan::{PersistedPlanState, PlanState},
    summary_draft_promotion::{
        PersistedSummaryDraftPromotionRegistry, SummaryDraftPromotionRegistry,
    },
};
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, EvidenceRef, PendingToolCall, PlanSnapshot, SessionId,
    SessionUsage, ToolCallId,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

const LEGACY_SESSION_STATE_FORMAT_VERSION: u32 = 1;
const PRE_PLAN_SESSION_STATE_FORMAT_VERSION: u32 = 2;
const SESSION_STATE_FORMAT_VERSION: u32 = 3;

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
    #[serde(default)]
    active_plan: Option<PersistedPlanState>,
    #[serde(default)]
    terminal_plans: Vec<PlanSnapshot>,
}

type StoredSessionDocumentV1 = StoredSessionDocument<PersistedTranscriptV1>;
type StoredSessionDocumentV2 = StoredSessionDocument<PersistedTranscript>;
type StoredSessionDocumentV3 = StoredSessionDocument<PersistedTranscript>;

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

struct PersistableSessionView<'a> {
    transcript: &'a Transcript,
    prompt_history_projection: PromptHistoryProjection,
    compacted_checkpoint: Option<&'a CompactedCheckpoint>,
    archived_ref_manifest: &'a ArchivedRefManifest,
    next_sequence: u64,
    session_started: bool,
    ledger: &'a TaskLedger,
    artifacts: &'a ArtifactRegistry,
    pending_tool_calls: &'a [PendingToolCall],
    resolved_tool_calls: &'a BTreeSet<ToolCallId>,
    active_plan: Option<&'a PlanState>,
    terminal_plans: &'a [PlanSnapshot],
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
        if !matches!(
            header.format_version,
            LEGACY_SESSION_STATE_FORMAT_VERSION
                | PRE_PLAN_SESSION_STATE_FORMAT_VERSION
                | SESSION_STATE_FORMAT_VERSION
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

        let mut session = match header.format_version {
            LEGACY_SESSION_STATE_FORMAT_VERSION => {
                let document: StoredSessionDocumentV1 = serde_json::from_slice(&bytes)?;
                Self::from_stored_document_v1(document)
            }
            PRE_PLAN_SESSION_STATE_FORMAT_VERSION => {
                let document: StoredSessionDocumentV2 = serde_json::from_slice(&bytes)?;
                Self::from_stored_document(document, PRE_PLAN_SESSION_STATE_FORMAT_VERSION)
            }
            SESSION_STATE_FORMAT_VERSION => {
                let document: StoredSessionDocumentV3 = serde_json::from_slice(&bytes)?;
                Self::from_stored_document(document, SESSION_STATE_FORMAT_VERSION)
            }
            _ => unreachable!("supported session format version checked before body decode"),
        }?;
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
            artifacts: &self.artifacts,
            pending_tool_calls: &self.pending_tool_calls,
            resolved_tool_calls: &self.resolved_tool_calls,
            active_plan: self.active_plan.as_ref(),
            terminal_plans: &self.terminal_plans,
        })
    }

    pub(crate) fn persistable_bundle_with_compaction(
        &self,
        prepared: &PreparedCompactionInstall,
    ) -> Result<PersistableSessionBundle, SessionStoreError> {
        self.persistable_bundle_for(PersistableSessionView {
            transcript: prepared.transcript(),
            prompt_history_projection: prepared.prompt_history_projection(),
            compacted_checkpoint: prepared.compacted_checkpoint(),
            archived_ref_manifest: prepared.archived_ref_manifest(),
            next_sequence: self.next_sequence,
            session_started: self.session_started,
            ledger: &self.ledger,
            artifacts: &self.artifacts,
            pending_tool_calls: &self.pending_tool_calls,
            resolved_tool_calls: &self.resolved_tool_calls,
            active_plan: self.active_plan.as_ref(),
            terminal_plans: &self.terminal_plans,
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
            artifacts: prepared.artifacts(),
            pending_tool_calls: prepared.pending_tool_calls(),
            resolved_tool_calls: prepared.resolved_tool_calls(),
            active_plan: Some(prepared.active_plan()),
            terminal_plans: prepared.terminal_plans(),
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
            view.artifacts,
            view.resolved_tool_calls,
        )?;
        self.validate_persisted_context_entries_with_checkpoint(view.compacted_checkpoint)?;
        self.validate_persisted_checkpoint_evidence_for(view.compacted_checkpoint)?;
        self.validate_archived_ref_manifest_for(
            view.transcript,
            view.prompt_history_projection,
            view.archived_ref_manifest,
        )
        .map_err(runtime_error_to_invalid_document)?;
        if let Some(active_plan) = view.active_plan {
            validate_plan_snapshot_refs(view.artifacts, active_plan.snapshot())?;
        }
        for terminal in view.terminal_plans {
            validate_plan_snapshot_refs(view.artifacts, terminal)?;
        }

        let artifacts = view
            .artifacts
            .persisted_records()
            .into_iter()
            .map(StoredArtifact::from)
            .collect::<Vec<_>>();

        let document = StoredSessionDocument {
            format_version: SESSION_STATE_FORMAT_VERSION,
            session_id: self.session_id.clone(),
            next_sequence: view.next_sequence,
            session_started: view.session_started,
            ledger: view.ledger.persisted_entries(),
            artifacts,
            compacted_checkpoint: view
                .compacted_checkpoint
                .map(CompactedCheckpoint::persisted),
            archived_ref_manifest: view
                .archived_ref_manifest
                .refs()
                .iter()
                .map(StoredArchivedRef::from)
                .collect(),
            prompt_history_projection: Some(view.prompt_history_projection),
            context_entries: self
                .context_entries
                .iter()
                .map(StoredContextEntry::from)
                .collect(),
            transcript: view.transcript.persisted(),
            resolved_tool_calls: view.resolved_tool_calls.iter().cloned().collect(),
            usage: self.usage.clone(),
            task_anchor: self.task_anchor.as_ref().map(|anchor| StoredTaskAnchor {
                objective: anchor.objective().to_owned(),
            }),
            registries: StoredRegistries {
                judgments: self.judgments.persisted(),
                summary_draft_promotions: self.summary_draft_promotions.persisted(),
                action_audits: self.action_audits.persisted(),
            },
            active_plan: view.active_plan.map(PlanState::persisted),
            terminal_plans: view.terminal_plans.to_vec(),
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

    fn from_stored_document(
        document: StoredSessionDocumentV3,
        expected_format_version: u32,
    ) -> Result<Self, SessionStoreError> {
        if document.format_version != expected_format_version {
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

        let active_plan = document
            .active_plan
            .map(PlanState::from_persisted)
            .transpose()
            .map_err(|_| invalid_document("stored active plan is invalid"))?;
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
            &session.artifacts,
            &session.resolved_tool_calls,
        )?;
        session.validate_persisted_context_entries_with_checkpoint(
            session.compacted_checkpoint.as_ref(),
        )?;
        session
            .validate_persisted_checkpoint_evidence_for(session.compacted_checkpoint.as_ref())?;
        session
            .validate_archived_ref_manifest()
            .map_err(runtime_error_to_invalid_document)?;
        if let Some(active_plan) = session.active_plan.as_ref() {
            validate_plan_snapshot_refs(&session.artifacts, active_plan.snapshot())?;
        }
        for terminal in &session.terminal_plans {
            validate_plan_snapshot_refs(&session.artifacts, terminal)?;
        }
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
            active_plan: _,
            terminal_plans: _,
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

        Self::from_stored_document(
            StoredSessionDocument {
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
                active_plan: None,
                terminal_plans: Vec::new(),
            },
            SESSION_STATE_FORMAT_VERSION,
        )
    }

    fn validate_persisted_context_entries_with_checkpoint(
        &self,
        compacted_checkpoint: Option<&CompactedCheckpoint>,
    ) -> Result<(), SessionStoreError> {
        let snapshot = SessionContextSnapshot::new(
            self.context_entries.clone(),
            self.artifacts.clone(),
            Vec::new(),
            compacted_checkpoint.cloned(),
        );
        ContextCompiler::new()
            .compile(&snapshot)
            .map(|_| ())
            .map_err(|_| invalid_document("stored context evidence is invalid"))
    }

    fn validate_persisted_checkpoint_evidence_for(
        &self,
        compacted_checkpoint: Option<&CompactedCheckpoint>,
    ) -> Result<(), SessionStoreError> {
        let Some(checkpoint) = compacted_checkpoint else {
            return Ok(());
        };
        self.validate_compacted_checkpoint_evidence(checkpoint)
            .map_err(|_| invalid_document("stored checkpoint evidence is invalid"))
    }

    fn validate_persisted_transcript_for(
        &self,
        transcript: &Transcript,
        prompt_history_projection: PromptHistoryProjection,
        compacted_checkpoint: Option<&CompactedCheckpoint>,
        artifacts: &ArtifactRegistry,
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
        let validate_text_artifact = |artifact_id: &ArtifactId| -> Result<(), SessionStoreError> {
            let artifact = artifacts
                .read_ref(artifact_id)
                .map_err(|_| invalid_document("stored transcript artifact is missing"))?;
            let content = artifacts
                .read_content(artifact_id)
                .map_err(|_| invalid_document("stored transcript artifact is missing"))?;
            if artifact.kind() != &ArtifactKind::Text || content.as_text().is_none() {
                return Err(invalid_document(
                    "stored user or assistant transcript artifact is not text",
                ));
            }
            Ok(())
        };
        let validate_user_image_artifact =
            |artifact_id: &ArtifactId| -> Result<UserImageInput, SessionStoreError> {
                let artifact = artifacts.read_ref(artifact_id).map_err(|_| {
                    invalid_document("stored user image transcript artifact is missing")
                })?;
                let content = artifacts.read_content(artifact_id).map_err(|_| {
                    invalid_document("stored user image transcript artifact is missing")
                })?;
                if artifact.kind() != &ArtifactKind::Image {
                    return Err(invalid_document(
                        "stored user image transcript artifact is not an image",
                    ));
                }
                let Some(label) = artifact.label() else {
                    return Err(invalid_document(
                        "stored user image transcript artifact has no label",
                    ));
                };
                let ArtifactContent::Image { bytes, metadata } = content else {
                    return Err(invalid_document(
                        "stored user image transcript content is not an image",
                    ));
                };
                let Some(metadata) = metadata else {
                    return Err(invalid_document(
                        "stored user image transcript artifact has no image metadata",
                    ));
                };
                if metadata.media_type() != "image/png" {
                    return Err(invalid_document(
                        "stored user image transcript artifact is not normalized PNG",
                    ));
                }
                UserImageInput::png(
                    label,
                    Arc::clone(bytes),
                    metadata.width(),
                    metadata.height(),
                )
                .map_err(|_| {
                    invalid_document("stored user image transcript artifact metadata is invalid")
                })
            };

        for item in transcript.items() {
            match item {
                TranscriptItem::UserMessage {
                    id,
                    artifact_id,
                    image_artifact_ids,
                    ..
                } => {
                    if artifact_id != &super::artifacts::user_message_id(*id) {
                        return Err(invalid_document(
                            "stored user transcript artifact identity is inconsistent",
                        ));
                    }
                    validate_text_artifact(artifact_id)?;
                    let text = artifacts
                        .read_content(artifact_id)
                        .expect("validated user text artifact remains readable")
                        .as_text()
                        .expect("validated user text artifact remains textual");
                    let images = image_artifact_ids
                        .iter()
                        .enumerate()
                        .map(|(offset, image_artifact_id)| {
                            if image_artifact_id
                                != &super::artifacts::user_message_image_id(*id, offset + 1)
                            {
                                return Err(invalid_document(
                                    "stored user image transcript artifact identity is inconsistent",
                                ));
                            }
                            validate_user_image_artifact(image_artifact_id)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    UserMessageInput::new(text, images).map_err(|_| {
                        invalid_document("stored user image transcript message is invalid")
                    })?;
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
                    let artifact = artifacts.read_ref(artifact_id).map_err(|_| {
                        invalid_document("stored transcript tool result artifact is missing")
                    })?;
                    let content = artifacts.read_content(artifact_id).map_err(|_| {
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
