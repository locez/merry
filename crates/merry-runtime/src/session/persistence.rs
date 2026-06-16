use super::{
    SessionState,
    transcript::{PersistedTranscript, Transcript},
};
use crate::{
    FileSessionStore, RuntimeError, SessionStoreError,
    action_audit::{ActionAuditRegistry, PersistedActionAuditRegistry},
    artifact::{ArtifactContent, ArtifactRegistry, PersistedArtifactRecord},
    context::{
        CompactedCheckpoint, ContextCompiler, ContextEntry, ContextEvidence, ContextSummary,
        PersistedCompactedCheckpoint, SessionContextSnapshot,
    },
    judgment::{JudgmentRegistry, PersistedJudgmentRegistry},
    ledger::{PersistedLedgerEntry, TaskLedger},
    memory::MemoryStore,
    summary_draft_promotion::{
        PersistedSummaryDraftPromotionRegistry, SummaryDraftPromotionRegistry,
    },
};
use merry_core::{ArtifactRef, EvidenceRef, SessionId, SessionUsage, ToolCallId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const SESSION_STATE_FORMAT_VERSION: u32 = 1;

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
    artifacts: Vec<StoredArtifact>,
    compacted_checkpoint: Option<PersistedCompactedCheckpoint>,
    context_entries: Vec<StoredContextEntry>,
    transcript: PersistedTranscript,
    resolved_tool_calls: Vec<ToolCallId>,
    usage: Option<SessionUsage>,
    task_anchor: Option<StoredTaskAnchor>,
    registries: StoredRegistries,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredTaskAnchor {
    objective: String,
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
        if header.format_version != SESSION_STATE_FORMAT_VERSION {
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
        Self::from_stored_document(document)
    }

    pub(crate) fn persistable_bundle(&self) -> Result<PersistableSessionBundle, SessionStoreError> {
        if !self.pending_tool_calls.is_empty() {
            return Err(SessionStoreError::UnsafePendingToolCalls {
                session_id: self.session_id.clone(),
                pending_count: self.pending_tool_calls.len(),
            });
        }
        self.validate_persisted_context_entries()?;

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

    fn from_stored_document(document: StoredSessionDocument) -> Result<Self, SessionStoreError> {
        if document.format_version != SESSION_STATE_FORMAT_VERSION {
            return Err(SessionStoreError::UnsupportedFormatVersion {
                actual: document.format_version,
            });
        }

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
            compacted_checkpoint: document
                .compacted_checkpoint
                .map(CompactedCheckpoint::from_persisted)
                .transpose()
                .map_err(|_| invalid_document("stored compacted checkpoint is invalid"))?,
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

        session.validate_persisted_context_entries()?;
        Ok(session)
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
