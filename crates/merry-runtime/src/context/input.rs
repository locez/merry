use super::{ContextError, evidence::format_locator};
use crate::{
    artifact::ArtifactRegistry,
    checkpoint::{CheckpointId, CitationBackedCheckpoint, PersistedCitationBackedCheckpoint},
    memory::ActivatedMemory,
};
use merry_core::EvidenceRef;

const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

fn validate_non_blank(field: &'static str, value: &str) -> Result<(), ContextError> {
    if value.trim().is_empty() {
        return Err(ContextError::BlankField { field });
    }

    Ok(())
}

fn validate_no_control_characters(field: &'static str, value: &str) -> Result<(), ContextError> {
    if value
        .chars()
        .any(|character| character.is_control() && character != '\n' && character != '\t')
    {
        return Err(ContextError::InvalidControlCharacter { field });
    }

    Ok(())
}

pub(crate) fn stable_content_hash(bytes: &[u8]) -> String {
    let mut hash = FNV_OFFSET_BASIS;
    for byte in bytes {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME);
    }
    format!("fnv1a64:{hash:016x}")
}
/// Session-owned context state, matching artifact view, and internal projections.
///
/// The fields are private so public callers can compile only snapshots created
/// by the runtime session that owns summaries, artifact state, and any
/// crate-internal context projections.
///
/// Treat this as an opaque view of session state. It is cloneable for
/// deterministic compilation and tests, but external callers should not depend
/// on its internal storage shape.
///
/// ```compile_fail
/// use merry_runtime::{ArtifactRegistry, ContextEntry, SessionContextSnapshot};
///
/// let entries: Vec<ContextEntry> = Vec::new();
/// let artifacts = ArtifactRegistry::default();
///
/// let _ = SessionContextSnapshot { entries, artifacts };
/// ```
#[derive(Debug, Clone)]
pub struct SessionContextSnapshot {
    entries: Vec<ContextEntry>,
    artifacts: ArtifactRegistry,
    memories: Vec<ActivatedMemory>,
    compacted_checkpoint: Option<CompactedCheckpoint>,
}

impl SessionContextSnapshot {
    pub(crate) fn new(
        entries: Vec<ContextEntry>,
        artifacts: ArtifactRegistry,
        memories: Vec<ActivatedMemory>,
        compacted_checkpoint: Option<CompactedCheckpoint>,
    ) -> Self {
        Self {
            entries,
            artifacts,
            memories,
            compacted_checkpoint,
        }
    }

    pub(super) fn entries(&self) -> &[ContextEntry] {
        &self.entries
    }

    pub(super) fn artifacts(&self) -> &ArtifactRegistry {
        &self.artifacts
    }

    pub(super) fn memories(&self) -> &[ActivatedMemory] {
        &self.memories
    }

    pub(super) fn compacted_checkpoint(&self) -> Option<&CompactedCheckpoint> {
        self.compacted_checkpoint.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn compacted_checkpoint_for_tests(&self) -> Option<&CompactedCheckpoint> {
        self.compacted_checkpoint.as_ref()
    }
}

/// Structured input item for the public context compiler view.
///
/// The MVP public view has only summary entries. Crate-internal projections,
/// including activated memory, are carried by [`SessionContextSnapshot`] rather
/// than exposed as public entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextEntry {
    /// A compact navigation summary backed by exact evidence references.
    Summary(ContextSummary),
}

impl ContextEntry {
    /// Creates a summary context entry.
    #[must_use]
    pub fn summary(summary: ContextSummary) -> Self {
        Self::Summary(summary)
    }

    pub(crate) fn as_summary(&self) -> &ContextSummary {
        match self {
            Self::Summary(summary) => summary,
        }
    }
}

/// Navigation text that must remain tied to exact retrievable evidence.
///
/// The text is a compact guide for context assembly. It must not replace the
/// artifact-backed evidence that supports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSummary {
    id: String,
    text: String,
    evidence: Vec<ContextEvidence>,
}

impl ContextSummary {
    /// Creates a validated context summary.
    ///
    /// Evidence presence is checked during compilation so construction can
    /// remain focused on field validity.
    pub fn new(
        id: impl Into<String>,
        text: impl Into<String>,
        evidence: Vec<ContextEvidence>,
    ) -> Result<Self, ContextError> {
        let id = id.into();
        validate_non_blank("context summary id", &id)?;

        let text = text.into();
        validate_non_blank("context summary text", &text)?;

        Ok(Self { id, text, evidence })
    }

    /// Stable summary identifier used for deterministic ordering.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Navigation text for this summary.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Exact evidence references linked to this summary.
    #[must_use]
    pub fn evidence(&self) -> &[ContextEvidence] {
        &self.evidence
    }
}

/// Stable project instructions explicitly loaded by runtime construction.
///
/// Project rules are durable prompt policy such as `AGENTS.md`. They are not
/// ledger projection, context summaries, or tool-result observations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRules {
    source_path: String,
    text: String,
    content_hash: String,
}

impl ProjectRules {
    /// Creates validated project rules for the stable request prefix.
    pub fn new(
        source_path: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<Self, ContextError> {
        let source_path = source_path.into();
        validate_non_blank("project rules source path", &source_path)?;
        validate_no_control_characters("project rules source path", &source_path)?;

        let text = text.into();
        validate_non_blank("project rules text", &text)?;
        validate_no_control_characters("project rules text", &text)?;

        let content_hash = stable_content_hash(text.as_bytes());
        Ok(Self {
            source_path,
            text,
            content_hash,
        })
    }

    /// Project-relative source path or label for these rules.
    #[must_use]
    pub fn source_path(&self) -> &str {
        &self.source_path
    }

    /// Exact project rules text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Stable non-cryptographic fingerprint of the rules text.
    #[must_use]
    pub fn content_hash(&self) -> &str {
        &self.content_hash
    }

    /// Renders the exact provider-neutral project-rules stable-prefix block.
    #[must_use]
    pub fn to_stable_prefix_message_text(&self) -> String {
        format!(
            "project-rules-source:{}\nproject-rules-content-hash:{}\n{}",
            self.source_path, self.content_hash, self.text
        )
    }
}

/// Current task objective pinned by a future `/task`-style control command.
///
/// A task anchor is session control-plane context. It is neither durable
/// project policy nor ordinary ordered transcript history, so request compilation
/// renders it outside the stable prefix and before checkpoint/context/body
/// projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskAnchor {
    objective: String,
}

impl TaskAnchor {
    /// Creates a validated task anchor objective.
    pub fn new(objective: impl Into<String>) -> Result<Self, ContextError> {
        let objective = objective.into();
        validate_non_blank("task anchor objective", &objective)?;
        validate_no_control_characters("task anchor objective", &objective)?;
        Ok(Self { objective })
    }

    /// Current pinned task objective.
    #[must_use]
    pub fn objective(&self) -> &str {
        &self.objective
    }

    pub(crate) fn to_dynamic_control_message_text(&self) -> String {
        format!("task-anchor:\n{}", self.objective)
    }
}

/// Runtime-owned checkpoint left after compacting earlier dynamic state.
///
/// This prompt-facing context is intentionally narrower than arbitrary context
/// projection: ordinary ledger facts, artifact payloads, and tool-result
/// observations must not enter through this path until a checkpoint/compaction
/// boundary has selected and compacted them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactedCheckpoint {
    text: String,
    citation_backed: Option<CitationBackedCheckpoint>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedCompactedCheckpoint {
    text: String,
    citation_backed: Option<PersistedCitationBackedCheckpoint>,
}

/// Payload-free checkpoint status for diagnostics and smoke reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactedCheckpointSummary {
    checkpoint_id: Option<CheckpointId>,
    citation_backed: bool,
    entry_count: usize,
    ref_count: usize,
}

impl CompactedCheckpointSummary {
    /// Citation-backed checkpoint id, when the checkpoint came from structured compaction.
    #[must_use]
    pub fn checkpoint_id(&self) -> Option<&CheckpointId> {
        self.checkpoint_id.as_ref()
    }

    /// Whether the installed checkpoint has structured citation metadata.
    #[must_use]
    pub fn citation_backed(&self) -> bool {
        self.citation_backed
    }

    /// Number of entries in the structured checkpoint, or zero for plain checkpoints.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.entry_count
    }

    /// Number of local refs in the structured checkpoint manifest, or zero for plain checkpoints.
    #[must_use]
    pub fn ref_count(&self) -> usize {
        self.ref_count
    }
}

impl CompactedCheckpoint {
    /// Creates validated compacted checkpoint text.
    pub fn new(text: impl Into<String>) -> Result<Self, ContextError> {
        let text = text.into();
        validate_non_blank("compacted checkpoint text", &text)?;
        validate_no_control_characters("compacted checkpoint text", &text)?;

        Ok(Self {
            text,
            citation_backed: None,
        })
    }

    /// Creates compacted checkpoint text from a validated citation-backed checkpoint.
    pub fn from_citation_backed(
        checkpoint: CitationBackedCheckpoint,
    ) -> Result<Self, ContextError> {
        let text = checkpoint.render_prompt_text();
        validate_non_blank("compacted checkpoint text", &text)?;
        validate_no_control_characters("compacted checkpoint text", &text)?;

        Ok(Self {
            text,
            citation_backed: Some(checkpoint),
        })
    }

    /// Compacted checkpoint text selected by a checkpoint/compaction boundary.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Structured citation-backed checkpoint state, when this checkpoint came from compaction.
    #[must_use]
    pub fn citation_backed(&self) -> Option<&CitationBackedCheckpoint> {
        self.citation_backed.as_ref()
    }

    /// Payload-free summary for diagnostics. This excludes entry text and evidence content.
    #[must_use]
    pub fn summary(&self) -> CompactedCheckpointSummary {
        match &self.citation_backed {
            Some(checkpoint) => CompactedCheckpointSummary {
                checkpoint_id: Some(checkpoint.id().clone()),
                citation_backed: true,
                entry_count: checkpoint.sections().entry_count(),
                ref_count: checkpoint.manifest().refs().len(),
            },
            None => CompactedCheckpointSummary {
                checkpoint_id: None,
                citation_backed: false,
                entry_count: 0,
                ref_count: 0,
            },
        }
    }

    pub(crate) fn persisted(&self) -> PersistedCompactedCheckpoint {
        PersistedCompactedCheckpoint {
            text: self.text.clone(),
            citation_backed: self
                .citation_backed
                .as_ref()
                .map(CitationBackedCheckpoint::persisted),
        }
    }

    pub(crate) fn from_persisted(
        persisted: PersistedCompactedCheckpoint,
    ) -> Result<Self, ContextError> {
        match persisted.citation_backed {
            Some(checkpoint) => {
                Self::from_citation_backed(CitationBackedCheckpoint::from_persisted(checkpoint)?)
            }
            None => Self::new(persisted.text),
        }
    }
}

/// Exact evidence metadata linked from compiled context.
///
/// Evidence metadata keeps the compiled context connected to exact artifact
/// locations. Labels explain why a reference was selected; they are not a
/// substitute for readable evidence content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextEvidence {
    label: String,
    reference: EvidenceRef,
}

impl ContextEvidence {
    /// Creates labeled evidence metadata for compiled context.
    pub fn new(label: impl Into<String>, reference: EvidenceRef) -> Result<Self, ContextError> {
        let label = label.into();
        validate_non_blank("context evidence label", &label)?;

        Ok(Self { label, reference })
    }

    /// Human-readable reason this evidence was selected.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Exact artifact location for the evidence.
    #[must_use]
    pub fn reference(&self) -> &EvidenceRef {
        &self.reference
    }

    pub(super) fn sort_key(&self) -> (&str, String, &str) {
        (
            self.reference.artifact_id.as_str(),
            format_locator(&self.reference.locator),
            self.label.as_str(),
        )
    }
}
