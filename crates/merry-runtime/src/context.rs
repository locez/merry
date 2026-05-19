//! Deterministic context compiler skeleton.
//!
//! The MVP context model is summary-only. A summary is navigation text, not the
//! source of truth: exact evidence must remain readable from session-owned
//! artifacts before the summary can enter compiled context.
//!
//! [`SessionContextSnapshot`] is intentionally opaque and created by the
//! runtime session that owns both context entries and artifacts. The compiler
//! accepts snapshots rather than arbitrary caller-paired entries and registries
//! so evidence validation is tied to the owning session.
//!
//! The shapes in this module are current MVP contracts. [`ContextEntry`] and
//! [`CompiledContextSection`] may gain variants as Memory Activation and richer
//! context assembly are introduced.

use crate::artifact::{ArtifactError, ArtifactRegistry};
use merry_core::{ArtifactId, EvidenceLocator, EvidenceRef};
use thiserror::Error;

/// Compiles structured runtime state into a deterministic context snapshot.
///
/// Public callers must compile from a session-owned snapshot, not from an
/// arbitrary entry list paired with an arbitrary artifact registry.
///
/// ```compile_fail
/// use merry_runtime::{ArtifactRegistry, ContextCompiler, ContextEntry};
///
/// let compiler = ContextCompiler::new();
/// let entries: Vec<ContextEntry> = Vec::new();
/// let artifacts = ArtifactRegistry::default();
///
/// let _ = compiler.compile(entries, &artifacts);
/// ```
#[derive(Debug, Default)]
pub struct ContextCompiler;

impl ContextCompiler {
    /// Creates a context compiler with default deterministic ordering.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Compiles structured context entries after proving linked evidence is readable.
    ///
    /// This enforces the runtime rule that summaries are navigation only:
    /// every linked exact evidence reference must resolve through recorded
    /// artifact content before summary text enters the compiled context.
    ///
    /// Output ordering is deterministic for a given snapshot. The resulting
    /// [`CompiledContext`] is a runtime-owned intermediate, not a stable prompt
    /// format for provider adapters.
    pub fn compile(
        &self,
        snapshot: &SessionContextSnapshot,
    ) -> Result<CompiledContext, ContextError> {
        compile_entries(snapshot.entries(), snapshot.artifacts())
    }
}

fn compile_entries(
    entries: &[ContextEntry],
    artifacts: &ArtifactRegistry,
) -> Result<CompiledContext, ContextError> {
    let mut sections = Vec::with_capacity(entries.len());

    for entry in entries {
        match entry {
            ContextEntry::Summary(summary) => {
                if summary.evidence.is_empty() {
                    return Err(ContextError::SummaryWithoutEvidence {
                        id: summary.id.clone(),
                    });
                }

                let mut evidence = summary.evidence.clone();
                evidence.sort_by(|left, right| left.sort_key().cmp(&right.sort_key()));

                validate_evidence(&summary.id, &evidence, artifacts)?;

                sections.push(CompiledContextSection::Summary {
                    id: summary.id.clone(),
                    text: summary.text.clone(),
                    evidence,
                });
            }
        }
    }

    sections.sort_by(|left, right| left.sort_key().cmp(&right.sort_key()));

    Ok(CompiledContext { sections })
}

/// Session-owned context state and matching artifact view.
///
/// The fields are private so public callers can compile only snapshots created
/// by the runtime session that owns both summaries and artifact state.
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
}

impl SessionContextSnapshot {
    pub(crate) fn new(entries: Vec<ContextEntry>, artifacts: ArtifactRegistry) -> Self {
        Self { entries, artifacts }
    }

    fn entries(&self) -> &[ContextEntry] {
        &self.entries
    }

    fn artifacts(&self) -> &ArtifactRegistry {
        &self.artifacts
    }
}

/// Structured input item for the context compiler.
///
/// The MVP has only summary entries. Additional variants may be added when the
/// runtime records Memory Activation or other structured context sources.
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

    fn sort_key(&self) -> (&str, String, &str) {
        (
            self.reference.artifact_id.as_str(),
            format_locator(&self.reference.locator),
            self.label.as_str(),
        )
    }
}

/// Reproducible compiled context snapshot.
///
/// This is a deterministic runtime intermediate for MVP request compilation.
/// It is not a stable serialized prompt contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledContext {
    sections: Vec<CompiledContextSection>,
}

impl CompiledContext {
    /// Ordered compiled context sections.
    #[must_use]
    pub fn sections(&self) -> &[CompiledContextSection] {
        &self.sections
    }

    /// Stable text snapshot for tests and future adapter work.
    ///
    /// The string is deterministic for a compiled snapshot, but it is a helper
    /// representation rather than a stable provider prompt format.
    #[must_use]
    pub fn to_snapshot(&self) -> String {
        let mut lines = Vec::new();

        for section in &self.sections {
            match section {
                CompiledContextSection::Summary { id, text, evidence } => {
                    lines.push(format!("summary:{id}"));
                    lines.push(format!("text:{text}"));
                    for item in evidence {
                        lines.push(format!(
                            "evidence:{}:{}:{}",
                            item.label,
                            item.reference.artifact_id,
                            format_locator(&item.reference.locator)
                        ));
                    }
                }
            }
        }

        lines.join("\n")
    }
}

/// A section in the compiled context snapshot.
///
/// The enum may grow as the runtime adds Memory Activation and other structured
/// context sources. Match exhaustively only inside this crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompiledContextSection {
    /// Navigation summary plus exact retrievable evidence references.
    Summary {
        /// Stable summary identifier.
        id: String,
        /// Summary text used for navigation.
        text: String,
        /// Exact evidence metadata that preserves source access.
        evidence: Vec<ContextEvidence>,
    },
}

impl CompiledContextSection {
    fn sort_key(&self) -> (&str, &str) {
        match self {
            Self::Summary { id, .. } => ("summary", id.as_str()),
        }
    }
}

/// Errors raised while constructing or compiling structured context.
///
/// These errors protect the MVP contract that summary text cannot enter
/// compiled context unless its exact evidence is readable.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContextError {
    /// A required context field was blank.
    #[error("{field} must not be blank")]
    BlankField {
        /// Name of the invalid field.
        field: &'static str,
    },

    /// Summary text was provided without exact evidence metadata.
    #[error("context summary {id} has no exact evidence references")]
    SummaryWithoutEvidence {
        /// Summary identifier that failed evidence validation.
        id: String,
    },

    /// Summary evidence did not resolve to readable artifact content.
    #[error("context summary {summary_id} references unreadable evidence {artifact_id}: {source}")]
    UnreadableEvidence {
        /// Summary identifier that linked the unreadable evidence.
        summary_id: String,
        /// Evidence artifact identifier.
        artifact_id: ArtifactId,
        /// Artifact registry read error.
        #[source]
        source: ArtifactError,
    },
}

fn validate_non_blank(field: &'static str, value: &str) -> Result<(), ContextError> {
    if value.trim().is_empty() {
        return Err(ContextError::BlankField { field });
    }

    Ok(())
}

fn validate_evidence(
    summary_id: &str,
    evidence: &[ContextEvidence],
    artifacts: &ArtifactRegistry,
) -> Result<(), ContextError> {
    for item in evidence {
        artifacts
            .validate_evidence(item.reference())
            .map_err(|source| ContextError::UnreadableEvidence {
                summary_id: summary_id.to_owned(),
                artifact_id: item.reference.artifact_id.clone(),
                source,
            })?;
    }

    Ok(())
}

fn format_locator(locator: &EvidenceLocator) -> String {
    if locator.is_whole_artifact() {
        return "whole".to_owned();
    }

    if let Some((start, end)) = locator.as_line_range() {
        return format!("line:{start}-{end}");
    }

    if let Some((start, end)) = locator.as_byte_range() {
        return format!("byte:{start}-{end}");
    }

    if let Some(pointer) = locator.as_json_pointer() {
        return format!("json:{pointer}");
    }

    if let Some(name) = locator.as_named_section() {
        return format!("section:{name}");
    }

    unreachable!("all evidence locator variants are covered by public accessors")
}
