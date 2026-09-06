use super::evidence::{format_locator, validate_evidence, validate_memory_evidence};
use super::{
    CompactedCheckpoint, ContextEntry, ContextError, ContextEvidence, SessionContextSnapshot,
};
use crate::{
    artifact::ArtifactRegistry,
    memory::{
        ActivatedMemory, MemoryActivationProvenance, MemoryActivationReason, MemoryActivationScore,
        MemoryEvidence, MemoryId, MemoryScope,
    },
    token_estimate::estimate_text_tokens,
};
use merry_core::EvidenceRef;
use std::{
    cmp::Ordering,
    collections::{BTreeMap, btree_map::Entry},
};

fn format_memory_scope(scope: MemoryScope) -> &'static str {
    match scope {
        MemoryScope::Session => "session",
        MemoryScope::Task => "task",
        MemoryScope::Step => "step",
    }
}

fn format_memory_scopes(scopes: &[MemoryScope]) -> String {
    scopes
        .iter()
        .map(|scope| format_memory_scope(*scope))
        .collect::<Vec<_>>()
        .join(",")
}

fn canonicalize_memory_reason_text(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Compiles allowlisted structured runtime state into a deterministic context snapshot.
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
    /// Reducers must not use ordinary `ContextSummary` entries as a default
    /// channel for projecting tool-result summaries, ledger observations, or
    /// artifact payloads into prompts.
    ///
    /// Output ordering is deterministic for a given snapshot. The resulting
    /// [`CompiledContext`] is a runtime-owned intermediate, not a stable prompt
    /// format for provider adapters.
    pub fn compile(
        &self,
        snapshot: &SessionContextSnapshot,
    ) -> Result<CompiledContext, ContextError> {
        compile_entries(
            snapshot.entries(),
            snapshot.artifacts(),
            snapshot.memories(),
            snapshot.compacted_checkpoint(),
        )
    }

    pub(crate) fn compile_without_compacted_checkpoint(
        &self,
        snapshot: &SessionContextSnapshot,
    ) -> Result<CompiledContext, ContextError> {
        compile_entries(
            snapshot.entries(),
            snapshot.artifacts(),
            snapshot.memories(),
            None,
        )
    }
}

fn compile_entries(
    entries: &[ContextEntry],
    artifacts: &ArtifactRegistry,
    memories: &[ActivatedMemory],
    compacted_checkpoint: Option<&CompactedCheckpoint>,
) -> Result<CompiledContext, ContextError> {
    let mut sections = Vec::with_capacity(entries.len());

    for entry in entries {
        match entry {
            ContextEntry::Summary(summary) => {
                if summary.evidence().is_empty() {
                    return Err(ContextError::SummaryWithoutEvidence {
                        id: summary.id().to_owned(),
                    });
                }

                let mut evidence = summary.evidence().to_vec();
                evidence.sort_by(|left, right| left.sort_key().cmp(&right.sort_key()));

                validate_evidence(summary.id(), &evidence, artifacts)?;

                sections.push(CompiledContextSection::Summary {
                    id: summary.id().to_owned(),
                    text: summary.text().to_owned(),
                    evidence,
                });
            }
        }
    }

    sections.sort_by(|left, right| left.sort_key().cmp(&right.sort_key()));

    validate_memory_evidence(memories, artifacts)?;
    let mut memory_projection = canonical_memory_projection(memories);
    memory_projection.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.id.cmp(&right.id))
    });

    Ok(CompiledContext {
        sections,
        memory_projection,
        checkpoint: ContextCheckpointSegment::new(compacted_checkpoint.cloned()),
    })
}

/// Reproducible compiled context snapshot.
///
/// This is a deterministic runtime intermediate for MVP request compilation.
/// It is not a stable serialized prompt contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledContext {
    sections: Vec<CompiledContextSection>,
    memory_projection: Vec<CompiledMemory>,
    checkpoint: ContextCheckpointSegment,
}

impl CompiledContext {
    /// Ordered public compiled context sections.
    ///
    /// This summary-only view excludes crate-internal projections such as
    /// activated memory.
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

        self.checkpoint.append_prompt_lines(&mut lines);
        self.append_body_prompt_lines(&mut lines);

        lines.join("\n")
    }

    pub(crate) fn checkpoint_snapshot(&self) -> String {
        let mut lines = Vec::new();
        self.checkpoint.append_prompt_lines(&mut lines);
        lines.join("\n")
    }

    pub(crate) fn body_snapshot(&self) -> String {
        let mut lines = Vec::new();
        self.append_body_prompt_lines(&mut lines);
        lines.join("\n")
    }

    fn append_body_prompt_lines(&self, lines: &mut Vec<String>) {
        for section in &self.sections {
            match section {
                CompiledContextSection::Summary { id, text, evidence } => {
                    lines.push(format!("summary:{id}"));
                    lines.push(format!("text:{text}"));
                    for item in evidence {
                        lines.push(format!(
                            "evidence:{}:{}:{}",
                            item.label(),
                            item.reference().artifact_id,
                            format_locator(&item.reference().locator)
                        ));
                    }
                }
            }
        }

        for memory in &self.memory_projection {
            lines.push(format!("memory:{}", memory.id));
            lines.push(format!(
                "memory-scope:{}",
                format_memory_scope(memory.scope)
            ));
            lines.push(format!("memory-text:{}", memory.text));
            lines.push(format!(
                "memory-activation-source-kind:{}",
                memory.provenance.source_kind().as_str()
            ));
            lines.push(format!(
                "memory-activation-source-label:{}",
                memory.provenance.source_label()
            ));
            lines.push(format!(
                "memory-activation-query:{}",
                memory.provenance.canonical_query()
            ));
            lines.push(format!(
                "memory-activation-allowed-scopes:{}",
                format_memory_scopes(memory.provenance.allowed_scopes())
            ));
            for item in &memory.evidence {
                lines.push(format!(
                    "memory-evidence:{}:{}:{}",
                    item.label,
                    item.reference.artifact_id,
                    format_locator(&item.reference.locator)
                ));
            }
            for reason in &memory.reasons {
                match reason {
                    CompiledMemoryReason::ScopeAllowed => {
                        lines.push("memory-reason:scope_allowed".to_owned());
                    }
                    CompiledMemoryReason::TriggerMatched(trigger) => {
                        lines.push(format!("memory-reason:trigger:{trigger}"));
                    }
                    CompiledMemoryReason::Ranked { score } => {
                        lines.push(format!(
                            "memory-reason:rank:matches={};priority={};confidence={:.3}",
                            score.trigger_matches(),
                            score.priority(),
                            score.confidence().as_f32()
                        ));
                    }
                    CompiledMemoryReason::ConflictWinner { suppressed } => {
                        lines.push(format!(
                            "memory-reason:conflict_winner:suppressed={}",
                            suppressed
                                .iter()
                                .map(MemoryId::as_str)
                                .collect::<Vec<_>>()
                                .join(",")
                        ));
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ContextCheckpointSegment {
    checkpoint: Option<CompactedCheckpoint>,
}

const COMPACTED_CHECKPOINT_HEADER: &str = "compacted-checkpoint:";
const COMPACTED_CHECKPOINT_GUIDANCE: &str = "guidance:Compacted checkpoint text is navigation, not exact evidence. Re-read cited artifacts or current workspace files before editing, verifying, or relying on summarized details.";
const COMPACTED_CHECKPOINT_TEXT_PREFIX: &str = "text:";

pub(crate) fn compacted_checkpoint_wrapper_token_ceiling() -> u64 {
    let wrapper = format!(
        "{COMPACTED_CHECKPOINT_HEADER}\n{COMPACTED_CHECKPOINT_GUIDANCE}\n{COMPACTED_CHECKPOINT_TEXT_PREFIX}"
    );
    estimate_text_tokens(&wrapper).saturating_add(1)
}

impl ContextCheckpointSegment {
    fn new(checkpoint: Option<CompactedCheckpoint>) -> Self {
        Self { checkpoint }
    }

    fn append_prompt_lines(&self, lines: &mut Vec<String>) {
        if let Some(checkpoint) = &self.checkpoint {
            lines.push(COMPACTED_CHECKPOINT_HEADER.to_owned());
            lines.push(COMPACTED_CHECKPOINT_GUIDANCE.to_owned());
            lines.push(format!(
                "{COMPACTED_CHECKPOINT_TEXT_PREFIX}{}",
                checkpoint.text()
            ));
        }
    }
}

/// A section in the compiled context snapshot.
///
/// The public compiled section view is summary-only in the MVP. Crate-internal
/// projections remain available to runtime-owned provider request compilation
/// and in [`CompiledContext::to_snapshot`].
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompiledMemory {
    id: MemoryId,
    scope: MemoryScope,
    text: String,
    evidence: Vec<CompiledMemoryEvidence>,
    score: MemoryActivationScore,
    provenance: MemoryActivationProvenance,
    reasons: Vec<CompiledMemoryReason>,
}

impl CompiledMemory {
    fn from_activation(memory: &ActivatedMemory) -> Self {
        let mut evidence = memory
            .item()
            .evidence()
            .iter()
            .map(CompiledMemoryEvidence::from_evidence)
            .collect::<Vec<_>>();
        evidence.sort();
        evidence.dedup();

        let mut reasons = memory
            .reasons()
            .iter()
            .map(CompiledMemoryReason::from_reason)
            .collect::<Vec<_>>();
        reasons.sort();
        reasons.dedup();

        Self {
            id: memory.item().id().clone(),
            scope: memory.item().scope(),
            text: memory.item().text().to_owned(),
            evidence,
            score: memory.score(),
            provenance: memory.provenance().clone(),
            reasons,
        }
    }
}

fn canonical_memory_projection(memories: &[ActivatedMemory]) -> Vec<CompiledMemory> {
    let mut by_id = BTreeMap::<MemoryId, CompiledMemory>::new();

    for memory in memories.iter().map(CompiledMemory::from_activation) {
        match by_id.entry(memory.id.clone()) {
            Entry::Occupied(mut entry) => {
                if memory.canonical_key() < entry.get().canonical_key() {
                    entry.insert(memory);
                }
            }
            Entry::Vacant(entry) => {
                entry.insert(memory);
            }
        }
    }

    by_id.into_values().collect()
}

impl CompiledMemory {
    fn canonical_key(&self) -> CompiledMemoryCanonicalKey<'_> {
        (
            std::cmp::Reverse(self.score),
            self.scope,
            self.text.as_str(),
            self.evidence.as_slice(),
            &self.provenance,
            self.reasons.as_slice(),
        )
    }
}

type CompiledMemoryCanonicalKey<'a> = (
    std::cmp::Reverse<MemoryActivationScore>,
    MemoryScope,
    &'a str,
    &'a [CompiledMemoryEvidence],
    &'a MemoryActivationProvenance,
    &'a [CompiledMemoryReason],
);

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompiledMemoryEvidence {
    label: String,
    reference: EvidenceRef,
}

impl CompiledMemoryEvidence {
    fn from_evidence(evidence: &MemoryEvidence) -> Self {
        Self {
            label: evidence.label().to_owned(),
            reference: evidence.reference().clone(),
        }
    }

    fn sort_key(&self) -> (&str, String, &str) {
        (
            self.reference.artifact_id.as_str(),
            format_locator(&self.reference.locator),
            self.label.as_str(),
        )
    }
}

impl Ord for CompiledMemoryEvidence {
    fn cmp(&self, other: &Self) -> Ordering {
        self.sort_key().cmp(&other.sort_key())
    }
}

impl PartialOrd for CompiledMemoryEvidence {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum CompiledMemoryReason {
    ScopeAllowed,
    TriggerMatched(String),
    Ranked { score: MemoryActivationScore },
    ConflictWinner { suppressed: Vec<MemoryId> },
}

impl CompiledMemoryReason {
    fn from_reason(reason: &MemoryActivationReason) -> Self {
        match reason {
            MemoryActivationReason::ScopeAllowed => Self::ScopeAllowed,
            MemoryActivationReason::TriggerMatched(trigger) => {
                Self::TriggerMatched(canonicalize_memory_reason_text(trigger))
            }
            MemoryActivationReason::Ranked { score } => Self::Ranked { score: *score },
            MemoryActivationReason::ConflictWinner { suppressed } => {
                let mut suppressed = suppressed.clone();
                suppressed.sort();
                suppressed.dedup();
                Self::ConflictWinner { suppressed }
            }
        }
    }
}

impl CompiledContextSection {
    fn sort_key(&self) -> (&str, &str) {
        match self {
            Self::Summary { id, .. } => ("summary", id.as_str()),
        }
    }
}
