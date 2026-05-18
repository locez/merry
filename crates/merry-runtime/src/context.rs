//! Deterministic context compiler skeleton.

use crate::artifact::{ArtifactError, ArtifactRegistry};
use merry_core::{ArtifactId, EvidenceLocator, EvidenceRef};
use thiserror::Error;

/// Compiles structured runtime state into a deterministic context snapshot.
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
    pub fn compile(
        &self,
        entries: Vec<ContextEntry>,
        artifacts: &ArtifactRegistry,
    ) -> Result<CompiledContext, ContextError> {
        let mut sections = Vec::with_capacity(entries.len());

        for entry in entries {
            match entry {
                ContextEntry::Summary(summary) => {
                    if summary.evidence.is_empty() {
                        return Err(ContextError::SummaryWithoutEvidence { id: summary.id });
                    }

                    let mut evidence = summary.evidence;
                    evidence.sort_by(|left, right| left.sort_key().cmp(&right.sort_key()));

                    validate_evidence(&summary.id, &evidence, artifacts)?;

                    sections.push(CompiledContextSection::Summary {
                        id: summary.id,
                        text: summary.text,
                        evidence,
                    });
                }
            }
        }

        sections.sort_by(|left, right| left.sort_key().cmp(&right.sort_key()));

        Ok(CompiledContext { sections })
    }
}

/// Structured input item for the context compiler.
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSummary {
    id: String,
    text: String,
    evidence: Vec<ContextEvidence>,
}

impl ContextSummary {
    /// Creates a validated context summary.
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
