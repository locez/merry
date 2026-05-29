//! Deterministic context compiler skeleton.
//!
//! The public MVP context model exposes summary sections only. A summary is
//! navigation text, not the source of truth: exact evidence must remain readable
//! from session-owned artifacts before the summary can enter compiled context.
//! Ordinary ledger facts, tool-result observations, and artifact payloads are
//! queryable runtime state; recording them is not permission to project them
//! into provider prompts. The runtime may attach crate-internal projections,
//! such as activated memory, when those projections have their own explicit
//! justification.
//!
//! [`SessionContextSnapshot`] is intentionally opaque and created by the
//! runtime session that owns both context entries and artifacts. The compiler
//! accepts snapshots rather than arbitrary caller-paired entries and registries
//! so evidence validation is tied to the owning session.
//!
use crate::artifact::{ArtifactError, ArtifactRegistry};
use crate::memory::{
    ActivatedMemory, MemoryActivationProvenance, MemoryActivationReason, MemoryActivationScore,
    MemoryEvidence, MemoryId, MemoryScope,
};
use merry_core::{ArtifactId, EvidenceLocator, EvidenceRef};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, btree_map::Entry},
};
use thiserror::Error;

const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextBudgetPolicy {
    /// Earlier checkpoint planning to reduce prompt cost.
    CostAware,
    /// Default compromise between preserving context and avoiding late compaction.
    Balanced,
    /// Use more of the available body budget before checkpoint planning.
    Capacity,
}

/// Derived context body budget and checkpoint watermarks.
///
/// The budget subtracts cacheable stable-prefix tokens and output reserve from
/// an effective model context window before calculating dynamic-body
/// watermarks. It does not perform token estimation or mutate runtime state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBudget {
    effective_window_tokens: u64,
    stable_prefix_tokens: u64,
    output_reserve_tokens: u64,
    body_budget_tokens: u64,
    soft_water_tokens: u64,
    hard_water_tokens: u64,
}

impl ContextBudget {
    /// Calculates dynamic-body budget watermarks from a resolved context window.
    pub fn from_window(
        resolved_context_window_tokens: u64,
        effective_context_window_percent: u8,
        stable_prefix_tokens: u64,
        output_reserve_tokens: u64,
        policy: ContextBudgetPolicy,
    ) -> Result<Self, ContextError> {
        if !(1..=100).contains(&effective_context_window_percent) {
            return Err(ContextError::InvalidBudget {
                reason: "effective context window percent must be between 1 and 100",
            });
        }

        let effective_window_tokens = resolved_context_window_tokens
            .checked_mul(u64::from(effective_context_window_percent))
            .and_then(|value| value.checked_div(100))
            .ok_or(ContextError::InvalidBudget {
                reason: "effective context window calculation overflowed",
            })?;
        let reserved_tokens = stable_prefix_tokens
            .checked_add(output_reserve_tokens)
            .ok_or(ContextError::InvalidBudget {
                reason: "reserved context tokens overflowed",
            })?;
        let body_budget_tokens = effective_window_tokens.checked_sub(reserved_tokens).ok_or(
            ContextError::InvalidBudget {
                reason: "effective context window must exceed stable prefix and output reserve",
            },
        )?;
        if body_budget_tokens == 0 {
            return Err(ContextError::InvalidBudget {
                reason: "body budget must be greater than zero",
            });
        }

        let (soft_percent, hard_percent) = policy.watermark_percents();
        let soft_water_tokens = body_budget_tokens
            .checked_mul(soft_percent)
            .and_then(|value| value.checked_div(100))
            .ok_or(ContextError::InvalidBudget {
                reason: "soft watermark calculation overflowed",
            })?;
        let hard_water_tokens = body_budget_tokens
            .checked_mul(hard_percent)
            .and_then(|value| value.checked_div(100))
            .ok_or(ContextError::InvalidBudget {
                reason: "hard watermark calculation overflowed",
            })?;

        if soft_water_tokens >= hard_water_tokens {
            return Err(ContextError::InvalidBudget {
                reason: "soft watermark must be below hard watermark",
            });
        }
        if hard_water_tokens > body_budget_tokens {
            return Err(ContextError::InvalidBudget {
                reason: "hard watermark must not exceed body budget",
            });
        }

        Ok(Self {
            effective_window_tokens,
            stable_prefix_tokens,
            output_reserve_tokens,
            body_budget_tokens,
            soft_water_tokens,
            hard_water_tokens,
        })
    }

    /// Context window after applying the effective window percentage.
    #[must_use]
    pub fn effective_window_tokens(&self) -> u64 {
        self.effective_window_tokens
    }

    /// Tokens reserved for cacheable stable-prefix messages and tool profile.
    #[must_use]
    pub fn stable_prefix_tokens(&self) -> u64 {
        self.stable_prefix_tokens
    }

    /// Tokens reserved for model output.
    #[must_use]
    pub fn output_reserve_tokens(&self) -> u64 {
        self.output_reserve_tokens
    }

    /// Remaining token budget for dynamic body content.
    #[must_use]
    pub fn body_budget_tokens(&self) -> u64 {
        self.body_budget_tokens
    }

    /// Dynamic-body watermark where checkpoint planning should begin.
    #[must_use]
    pub fn soft_water_tokens(&self) -> u64 {
        self.soft_water_tokens
    }

    /// Dynamic-body watermark where checkpointing should be required.
    #[must_use]
    pub fn hard_water_tokens(&self) -> u64 {
        self.hard_water_tokens
    }
}

impl ContextBudgetPolicy {
    fn watermark_percents(self) -> (u64, u64) {
        match self {
            Self::CostAware => (60, 80),
            Self::Balanced => (70, 90),
            Self::Capacity => (85, 95),
        }
    }
}

/// Source used to resolve a model context window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextWindowSource {
    /// Explicit runtime or caller override.
    ExplicitConfig,
    /// Provider-neutral model capabilities.
    ProviderCapabilities,
    /// Bundled model catalog metadata.
    BundledCatalog,
    /// Conservative fallback when no metadata is available.
    Fallback,
}

/// Resolved model context window and the metadata source that supplied it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedContextWindow {
    tokens: u64,
    source: ContextWindowSource,
}

impl ResolvedContextWindow {
    /// Resolved context window size in tokens.
    #[must_use]
    pub fn tokens(&self) -> u64 {
        self.tokens
    }

    /// Source that supplied the resolved context window.
    #[must_use]
    pub fn source(&self) -> ContextWindowSource {
        self.source
    }
}

/// Resolves context window metadata without provider probing.
pub fn resolve_context_window(
    explicit_override: Option<u64>,
    provider_capability: Option<u64>,
    bundled_catalog_value: Option<u64>,
    fallback: u64,
) -> Result<ResolvedContextWindow, ContextError> {
    let (tokens, source) = if let Some(tokens) = explicit_override {
        (tokens, ContextWindowSource::ExplicitConfig)
    } else if let Some(tokens) = provider_capability {
        (tokens, ContextWindowSource::ProviderCapabilities)
    } else if let Some(tokens) = bundled_catalog_value {
        (tokens, ContextWindowSource::BundledCatalog)
    } else {
        (fallback, ContextWindowSource::Fallback)
    };

    if tokens == 0 {
        return Err(ContextError::InvalidContextWindow {
            reason: "resolved context window must be greater than zero",
        });
    }

    Ok(ResolvedContextWindow { tokens, source })
}

/// Watermark-based checkpoint trigger decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointDecision {
    /// Dynamic body remains below the checkpoint planning watermark.
    Continue,
    /// Dynamic body reached the soft watermark; plan a checkpoint soon.
    PlanCheckpoint,
    /// Dynamic body reached the hard watermark; require checkpointing before more growth.
    RequireCheckpoint,
}

/// Decides whether dynamic body growth has reached checkpoint watermarks.
#[must_use]
pub fn decide_checkpoint(dynamic_body_tokens: u64, budget: ContextBudget) -> CheckpointDecision {
    if dynamic_body_tokens >= budget.hard_water_tokens() {
        CheckpointDecision::RequireCheckpoint
    } else if dynamic_body_tokens >= budget.soft_water_tokens() {
        CheckpointDecision::PlanCheckpoint
    } else {
        CheckpointDecision::Continue
    }
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
        )
    }
}

fn compile_entries(
    entries: &[ContextEntry],
    artifacts: &ArtifactRegistry,
    memories: &[ActivatedMemory],
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
        checkpoint: ContextCheckpointSegment,
    })
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
}

impl SessionContextSnapshot {
    pub(crate) fn new(
        entries: Vec<ContextEntry>,
        artifacts: ArtifactRegistry,
        memories: Vec<ActivatedMemory>,
    ) -> Self {
        Self {
            entries,
            artifacts,
            memories,
        }
    }

    fn entries(&self) -> &[ContextEntry] {
        &self.entries
    }

    fn artifacts(&self) -> &ArtifactRegistry {
        &self.artifacts
    }

    fn memories(&self) -> &[ActivatedMemory] {
        &self.memories
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

    pub(crate) fn to_stable_prefix_message_text(&self) -> String {
        format!(
            "project-rules-source:{}\nproject-rules-content-hash:{}\n{}",
            self.source_path, self.content_hash, self.text
        )
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

        lines.join("\n")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ContextCheckpointSegment;

impl ContextCheckpointSegment {
    fn append_prompt_lines(&self, _lines: &mut Vec<String>) {}
}

/// A section in the compiled context snapshot.
///
/// The public compiled section view is summary-only in the MVP. Crate-internal
/// projections may still be present in [`CompiledContext::to_snapshot`] for
/// runtime-owned provider request compilation.
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

/// Errors raised while constructing or compiling structured context.
///
/// These errors protect context invariants and the compile-time evidence
/// contract: public summary text and crate-internal memory projections can
/// enter compiled context only after their exact evidence resolves to readable
/// artifacts.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContextError {
    /// A required context field was blank.
    #[error("{field} must not be blank")]
    BlankField {
        /// Name of the invalid field.
        field: &'static str,
    },

    /// A context field contained unsupported control characters.
    #[error("{field} must not contain control characters other than newline or tab")]
    InvalidControlCharacter {
        /// Name of the invalid field.
        field: &'static str,
    },

    /// Context budget inputs could not produce a valid body budget.
    #[error("invalid context budget: {reason}")]
    InvalidBudget {
        /// Actionable reason the budget was rejected.
        reason: &'static str,
    },

    /// Context window metadata could not produce a valid window.
    #[error("invalid context window: {reason}")]
    InvalidContextWindow {
        /// Actionable reason the window was rejected.
        reason: &'static str,
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

    /// Internal memory text was provided without exact evidence metadata.
    #[error("memory item {memory_id} has no exact evidence references")]
    MemoryWithoutEvidence {
        /// Memory identifier that failed evidence validation.
        memory_id: String,
    },

    /// Internal memory evidence did not resolve to readable artifact content.
    #[error("memory item {memory_id} references unreadable evidence {artifact_id}: {source}")]
    UnreadableMemoryEvidence {
        /// Memory identifier that linked the unreadable evidence.
        memory_id: String,
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

fn validate_no_control_characters(field: &'static str, value: &str) -> Result<(), ContextError> {
    if value
        .chars()
        .any(|character| character.is_control() && character != '\n' && character != '\t')
    {
        return Err(ContextError::InvalidControlCharacter { field });
    }

    Ok(())
}

fn stable_content_hash(bytes: &[u8]) -> String {
    let mut hash = FNV_OFFSET_BASIS;
    for byte in bytes {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME);
    }
    format!("fnv1a64:{hash:016x}")
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

fn validate_memory_evidence(
    memories: &[ActivatedMemory],
    artifacts: &ArtifactRegistry,
) -> Result<(), ContextError> {
    for memory in memories {
        if memory.item().evidence().is_empty() {
            return Err(ContextError::MemoryWithoutEvidence {
                memory_id: memory.item().id().as_str().to_owned(),
            });
        }

        for item in memory.item().evidence() {
            artifacts
                .validate_evidence(item.reference())
                .map_err(|source| ContextError::UnreadableMemoryEvidence {
                    memory_id: memory.item().id().as_str().to_owned(),
                    artifact_id: item.reference().artifact_id.clone(),
                    source,
                })?;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        artifact::ArtifactContent,
        memory::{
            ActivatedMemory, MemoryActivationProvenance, MemoryActivationReason,
            MemoryActivationScore, MemoryActivationSourceKind, MemoryEvidence, MemoryItem,
            MemoryItemSelection,
        },
    };
    use merry_core::{ArtifactKind, ArtifactRef};

    #[test]
    fn to_snapshot_includes_activated_memory_text_and_reasons() {
        let memory = activated_memory(
            "memory-main",
            MemoryScope::Task,
            "Prefer the Rust 2024 workspace.",
            score(2, 7, 0.875),
            vec![
                MemoryActivationReason::ranked(score(2, 7, 0.875)),
                MemoryActivationReason::trigger_matched("workspace").expect("valid trigger"),
                MemoryActivationReason::conflict_winner(vec![
                    memory_id("memory-z"),
                    memory_id("memory-a"),
                ])
                .expect("valid conflict winner"),
                MemoryActivationReason::ScopeAllowed,
                MemoryActivationReason::trigger_matched("rust").expect("valid trigger"),
            ],
        );
        let snapshot = memory_snapshot(vec![memory]);

        let compiled = ContextCompiler::new()
            .compile(&snapshot)
            .expect("memory-only context compiles");

        assert_eq!(
            compiled.to_snapshot(),
            [
                "memory:memory-main",
                "memory-scope:task",
                "memory-text:Prefer the Rust 2024 workspace.",
                "memory-activation-source-kind:user_query",
                "memory-activation-source-label:user request",
                "memory-activation-query:topic",
                "memory-activation-allowed-scopes:session,task,step",
                "memory-evidence:primary source:memory-main-artifact:whole",
                "memory-reason:scope_allowed",
                "memory-reason:trigger:rust",
                "memory-reason:trigger:workspace",
                "memory-reason:rank:matches=2;priority=7;confidence=0.875",
                "memory-reason:conflict_winner:suppressed=memory-a,memory-z",
            ]
            .join("\n")
        );
    }

    #[test]
    fn project_rules_validate_fields_and_hash_text() {
        let rules =
            ProjectRules::new("AGENTS.md", "Use project rules.\n").expect("valid project rules");

        assert_eq!(rules.source_path(), "AGENTS.md");
        assert_eq!(rules.text(), "Use project rules.\n");
        assert!(rules.content_hash().starts_with("fnv1a64:"));
        assert!(
            rules
                .to_stable_prefix_message_text()
                .contains("project-rules-source:AGENTS.md")
        );
        assert!(matches!(
            ProjectRules::new("", "Use project rules."),
            Err(ContextError::BlankField {
                field: "project rules source path"
            })
        ));
        assert!(matches!(
            ProjectRules::new("AGENTS.md", "bad\u{7}rules"),
            Err(ContextError::InvalidControlCharacter {
                field: "project rules text"
            })
        ));
    }

    #[test]
    fn context_budget_balanced_uses_large_windows_without_step_count_compaction() {
        let budget = ContextBudget::from_window(
            1_000_000,
            95,
            120_000,
            32_000,
            ContextBudgetPolicy::Balanced,
        )
        .expect("budget should calculate");

        assert_eq!(budget.effective_window_tokens(), 950_000);
        assert_eq!(budget.stable_prefix_tokens(), 120_000);
        assert_eq!(budget.output_reserve_tokens(), 32_000);
        assert_eq!(budget.body_budget_tokens(), 798_000);
        assert_eq!(budget.soft_water_tokens(), 558_600);
        assert_eq!(budget.hard_water_tokens(), 718_200);
    }

    #[test]
    fn context_budget_policy_ratios_use_body_budget() {
        let cost_aware =
            ContextBudget::from_window(10_000, 100, 1_000, 1_000, ContextBudgetPolicy::CostAware)
                .expect("cost-aware budget should calculate");
        let balanced =
            ContextBudget::from_window(10_000, 100, 1_000, 1_000, ContextBudgetPolicy::Balanced)
                .expect("balanced budget should calculate");
        let capacity =
            ContextBudget::from_window(10_000, 100, 1_000, 1_000, ContextBudgetPolicy::Capacity)
                .expect("capacity budget should calculate");

        assert_eq!(cost_aware.body_budget_tokens(), 8_000);
        assert_eq!(cost_aware.soft_water_tokens(), 4_800);
        assert_eq!(cost_aware.hard_water_tokens(), 6_400);
        assert_eq!(balanced.soft_water_tokens(), 5_600);
        assert_eq!(balanced.hard_water_tokens(), 7_200);
        assert_eq!(capacity.soft_water_tokens(), 6_800);
        assert_eq!(capacity.hard_water_tokens(), 7_600);
    }

    #[test]
    fn context_budget_rejects_invalid_percent_or_reserve() {
        assert!(
            ContextBudget::from_window(1_000_000, 0, 0, 32_000, ContextBudgetPolicy::Balanced)
                .is_err()
        );
        assert!(
            ContextBudget::from_window(1_000, 95, 100, 1_000, ContextBudgetPolicy::Balanced)
                .is_err()
        );
        assert!(
            ContextBudget::from_window(1_000, 95, 950, 1, ContextBudgetPolicy::Balanced).is_err()
        );
    }

    #[test]
    fn context_window_resolver_prefers_explicit_config() {
        let resolved =
            resolve_context_window(Some(1_000_000), Some(200_000), Some(128_000), 64_000)
                .expect("window should resolve");

        assert_eq!(resolved.tokens(), 1_000_000);
        assert_eq!(resolved.source(), ContextWindowSource::ExplicitConfig);
    }

    #[test]
    fn context_window_resolver_prefers_provider_then_catalog_before_fallback() {
        let provider = resolve_context_window(None, Some(200_000), Some(128_000), 64_000)
            .expect("provider window should resolve");
        let catalog = resolve_context_window(None, None, Some(128_000), 64_000)
            .expect("catalog window should resolve");

        assert_eq!(provider.tokens(), 200_000);
        assert_eq!(provider.source(), ContextWindowSource::ProviderCapabilities);
        assert_eq!(catalog.tokens(), 128_000);
        assert_eq!(catalog.source(), ContextWindowSource::BundledCatalog);
    }

    #[test]
    fn context_window_resolver_falls_back_when_metadata_is_missing() {
        let resolved =
            resolve_context_window(None, None, None, 64_000).expect("window should resolve");

        assert_eq!(resolved.tokens(), 64_000);
        assert_eq!(resolved.source(), ContextWindowSource::Fallback);
    }

    #[test]
    fn context_window_resolver_rejects_zero_values() {
        assert!(resolve_context_window(Some(0), Some(200_000), Some(128_000), 64_000).is_err());
        assert!(resolve_context_window(None, Some(0), Some(128_000), 64_000).is_err());
        assert!(resolve_context_window(None, None, Some(0), 64_000).is_err());
        assert!(resolve_context_window(None, None, None, 0).is_err());
    }

    #[test]
    fn checkpoint_decision_uses_watermarks_not_turn_counts() {
        let budget =
            ContextBudget::from_window(100_000, 90, 8_000, 10_000, ContextBudgetPolicy::Balanced)
                .expect("budget should calculate");

        assert_eq!(decide_checkpoint(1, budget), CheckpointDecision::Continue);
        assert_eq!(
            decide_checkpoint(budget.soft_water_tokens() - 1, budget),
            CheckpointDecision::Continue
        );
        assert_eq!(
            decide_checkpoint(budget.soft_water_tokens(), budget),
            CheckpointDecision::PlanCheckpoint
        );
        assert_eq!(
            decide_checkpoint(budget.hard_water_tokens() - 1, budget),
            CheckpointDecision::PlanCheckpoint
        );
        assert_eq!(
            decide_checkpoint(budget.hard_water_tokens(), budget),
            CheckpointDecision::RequireCheckpoint
        );
    }

    #[test]
    fn memory_projection_ordering_is_independent_of_insertion_order() {
        let lower = activated_memory(
            "memory-a",
            MemoryScope::Session,
            "Lower ranked memory.",
            score(1, 1, 0.5),
            ranked_reasons(1, 1, 0.5),
        );
        let higher = activated_memory(
            "memory-b",
            MemoryScope::Session,
            "Higher ranked memory.",
            score(1, 3, 0.5),
            ranked_reasons(1, 3, 0.5),
        );

        let first = memory_snapshot(vec![lower.clone(), higher.clone()]);
        let second = memory_snapshot(vec![higher, lower]);

        let first = ContextCompiler::new()
            .compile(&first)
            .expect("first snapshot compiles")
            .to_snapshot();
        let second = ContextCompiler::new()
            .compile(&second)
            .expect("second snapshot compiles")
            .to_snapshot();

        assert_eq!(first, second);
        assert!(first.starts_with("memory:memory-b\n"));
    }

    #[test]
    fn duplicate_memory_ids_are_canonicalized_deterministically() {
        let lower_duplicate = activated_memory(
            "memory-duplicate",
            MemoryScope::Session,
            "Lower ranked duplicate.",
            score(1, 1, 0.5),
            ranked_reasons(1, 1, 0.5),
        );
        let higher_duplicate = activated_memory(
            "memory-duplicate",
            MemoryScope::Task,
            "Higher ranked duplicate.",
            score(2, 1, 0.5),
            ranked_reasons(2, 1, 0.5),
        );
        let other = activated_memory(
            "memory-other",
            MemoryScope::Session,
            "Other memory.",
            score(1, 3, 0.5),
            ranked_reasons(1, 3, 0.5),
        );

        let first = memory_snapshot(vec![
            lower_duplicate.clone(),
            other.clone(),
            higher_duplicate.clone(),
        ]);
        let second = memory_snapshot(vec![higher_duplicate, other, lower_duplicate]);

        let first = ContextCompiler::new()
            .compile(&first)
            .expect("first snapshot compiles")
            .to_snapshot();
        let second = ContextCompiler::new()
            .compile(&second)
            .expect("second snapshot compiles")
            .to_snapshot();

        assert_eq!(first, second);
        assert_eq!(first.matches("memory:memory-duplicate").count(), 1);
        assert!(first.contains("memory-text:Higher ranked duplicate."));
        assert!(!first.contains("memory-text:Lower ranked duplicate."));
    }

    #[test]
    fn duplicate_memory_id_ties_use_stable_content_ordering() {
        let z_text = activated_memory(
            "memory-duplicate",
            MemoryScope::Session,
            "Z text.",
            score(1, 1, 0.5),
            ranked_reasons(1, 1, 0.5),
        );
        let a_text = activated_memory(
            "memory-duplicate",
            MemoryScope::Session,
            "A text.",
            score(1, 1, 0.5),
            ranked_reasons(1, 1, 0.5),
        );

        let first = memory_snapshot(vec![z_text.clone(), a_text.clone()]);
        let second = memory_snapshot(vec![a_text, z_text]);

        let first = ContextCompiler::new()
            .compile(&first)
            .expect("first snapshot compiles")
            .to_snapshot();
        let second = ContextCompiler::new()
            .compile(&second)
            .expect("second snapshot compiles")
            .to_snapshot();

        assert_eq!(first, second);
        assert_eq!(first.matches("memory:memory-duplicate").count(), 1);
        assert!(first.contains("memory-text:A text."));
        assert!(!first.contains("memory-text:Z text."));
    }

    #[test]
    fn duplicate_memory_id_ties_include_evidence_and_provenance_in_canonical_key() {
        let z_evidence = activated_memory_with_evidence(
            "memory-duplicate",
            MemoryScope::Session,
            "Same text.",
            vec![memory_evidence(
                "source",
                "artifact-z",
                EvidenceLocator::whole_artifact(),
            )],
            score(1, 1, 0.5),
            ranked_reasons(1, 1, 0.5),
            provenance(),
        );
        let a_evidence = activated_memory_with_evidence(
            "memory-duplicate",
            MemoryScope::Session,
            "Same text.",
            vec![memory_evidence(
                "source",
                "artifact-a",
                EvidenceLocator::whole_artifact(),
            )],
            score(1, 1, 0.5),
            ranked_reasons(1, 1, 0.5),
            provenance(),
        );

        let first = ContextCompiler::new()
            .compile(&memory_snapshot(vec![
                z_evidence.clone(),
                a_evidence.clone(),
            ]))
            .expect("first evidence tie compiles")
            .to_snapshot();
        let second = ContextCompiler::new()
            .compile(&memory_snapshot(vec![a_evidence, z_evidence]))
            .expect("second evidence tie compiles")
            .to_snapshot();

        assert_eq!(first, second);
        assert_eq!(first.matches("memory:memory-duplicate").count(), 1);
        assert!(first.contains("memory-evidence:source:artifact-a:whole"));
        assert!(!first.contains("memory-evidence:source:artifact-z:whole"));

        let z_provenance = activated_memory_with_evidence(
            "memory-duplicate",
            MemoryScope::Session,
            "Same text.",
            vec![memory_evidence(
                "source",
                "artifact-a",
                EvidenceLocator::whole_artifact(),
            )],
            score(1, 1, 0.5),
            ranked_reasons(1, 1, 0.5),
            labeled_provenance("Z source"),
        );
        let a_provenance = activated_memory_with_evidence(
            "memory-duplicate",
            MemoryScope::Session,
            "Same text.",
            vec![memory_evidence(
                "source",
                "artifact-a",
                EvidenceLocator::whole_artifact(),
            )],
            score(1, 1, 0.5),
            ranked_reasons(1, 1, 0.5),
            labeled_provenance("A source"),
        );

        let first = ContextCompiler::new()
            .compile(&memory_snapshot(vec![
                z_provenance.clone(),
                a_provenance.clone(),
            ]))
            .expect("first provenance tie compiles")
            .to_snapshot();
        let second = ContextCompiler::new()
            .compile(&memory_snapshot(vec![a_provenance, z_provenance]))
            .expect("second provenance tie compiles")
            .to_snapshot();

        assert_eq!(first, second);
        assert_eq!(first.matches("memory:memory-duplicate").count(), 1);
        assert!(first.contains("memory-activation-source-label:A source"));
        assert!(!first.contains("memory-activation-source-label:Z source"));
    }

    #[test]
    fn sections_public_view_does_not_expose_memory_projection() {
        let memory = activated_memory(
            "memory-only",
            MemoryScope::Step,
            "Internal memory projection.",
            score(1, 0, 0.5),
            ranked_reasons(1, 0, 0.5),
        );
        let snapshot = memory_snapshot(vec![memory]);

        let compiled = ContextCompiler::new()
            .compile(&snapshot)
            .expect("memory-only context compiles");

        assert!(compiled.sections().is_empty());
        assert!(
            compiled
                .to_snapshot()
                .contains("memory-text:Internal memory projection.")
        );
    }

    #[test]
    fn memory_projection_does_not_bypass_summary_evidence_validation() {
        let memory = activated_memory(
            "memory-present",
            MemoryScope::Session,
            "Memory should not make invalid summaries compile.",
            score(1, 0, 0.5),
            ranked_reasons(1, 0, 0.5),
        );
        let summary = ContextEntry::summary(
            ContextSummary::new("summary-without-evidence", "Missing evidence.", Vec::new())
                .expect("summary fields are valid"),
        );
        let snapshot = snapshot_with_memories(vec![summary], vec![memory]);

        let error = ContextCompiler::new()
            .compile(&snapshot)
            .expect_err("summary evidence validation still applies");

        assert_eq!(
            error,
            ContextError::SummaryWithoutEvidence {
                id: "summary-without-evidence".to_owned()
            }
        );
    }

    #[test]
    fn summary_evidence_validation_still_uses_artifact_registry_with_memory_present() {
        let memory = activated_memory(
            "memory-present",
            MemoryScope::Session,
            "Memory should not make missing artifacts compile.",
            score(1, 0, 0.5),
            ranked_reasons(1, 0, 0.5),
        );
        let summary = ContextEntry::summary(
            ContextSummary::new(
                "summary-missing-artifact",
                "Missing artifact.",
                vec![
                    ContextEvidence::new(
                        "missing",
                        EvidenceRef::new(
                            artifact_id("missing-artifact"),
                            EvidenceLocator::whole_artifact(),
                        ),
                    )
                    .expect("evidence metadata is valid"),
                ],
            )
            .expect("summary fields are valid"),
        );
        let snapshot = snapshot_with_memories(vec![summary], vec![memory]);

        let error = ContextCompiler::new()
            .compile(&snapshot)
            .expect_err("missing summary evidence still fails");

        assert!(matches!(
            error,
            ContextError::UnreadableEvidence {
                summary_id,
                artifact_id,
                source: ArtifactError::MissingArtifact { .. },
            } if summary_id == "summary-missing-artifact" && artifact_id.as_str() == "missing-artifact"
        ));
    }

    #[test]
    fn compiler_rejects_memory_without_evidence() {
        let item = MemoryItem::new_unchecked_for_tests(
            memory_id("memory-without-evidence"),
            MemoryScope::Session,
            "Memory with no evidence.",
            Vec::new(),
            memory_selection(0.5, 0),
        )
        .expect("unchecked test memory is valid aside from evidence");
        let memory = ActivatedMemory::new(
            item,
            score(1, 0, 0.5),
            ranked_reasons(1, 0, 0.5),
            provenance(),
        )
        .expect("activation can expose legacy bad memory for compiler validation");
        let snapshot =
            SessionContextSnapshot::new(Vec::new(), ArtifactRegistry::default(), vec![memory]);

        let error = ContextCompiler::new()
            .compile(&snapshot)
            .expect_err("memory without evidence should fail");

        assert_eq!(
            error,
            ContextError::MemoryWithoutEvidence {
                memory_id: "memory-without-evidence".to_owned()
            }
        );
    }

    #[test]
    fn compiler_rejects_unreadable_memory_evidence_with_artifact_source() {
        let memory = activated_memory_with_evidence(
            "memory-missing-evidence",
            MemoryScope::Session,
            "Memory with missing evidence.",
            vec![memory_evidence(
                "missing",
                "missing-memory-artifact",
                EvidenceLocator::whole_artifact(),
            )],
            score(1, 0, 0.5),
            ranked_reasons(1, 0, 0.5),
            provenance(),
        );
        let snapshot =
            SessionContextSnapshot::new(Vec::new(), ArtifactRegistry::default(), vec![memory]);

        let error = ContextCompiler::new()
            .compile(&snapshot)
            .expect_err("missing memory evidence should fail");

        assert!(matches!(
            error,
            ContextError::UnreadableMemoryEvidence {
                memory_id,
                artifact_id,
                source: ArtifactError::MissingArtifact { id },
            } if memory_id == "memory-missing-evidence"
                && artifact_id.as_str() == "missing-memory-artifact"
                && id.as_str() == "missing-memory-artifact"
        ));
    }

    #[test]
    fn compiler_rejects_invalid_memory_evidence_locator_with_artifact_source() {
        let memory = activated_memory_with_evidence(
            "memory-invalid-evidence",
            MemoryScope::Session,
            "Memory with invalid evidence.",
            vec![memory_evidence(
                "invalid",
                "invalid-memory-artifact",
                EvidenceLocator::line_range(9, 10).expect("valid locator shape"),
            )],
            score(1, 0, 0.5),
            ranked_reasons(1, 0, 0.5),
            provenance(),
        );
        let mut artifacts = ArtifactRegistry::default();
        record_text_artifacts(&mut artifacts, std::slice::from_ref(&memory));
        let snapshot = SessionContextSnapshot::new(Vec::new(), artifacts, vec![memory]);

        let error = ContextCompiler::new()
            .compile(&snapshot)
            .expect_err("invalid memory evidence locator should fail");

        assert!(matches!(
            error,
            ContextError::UnreadableMemoryEvidence {
                memory_id,
                artifact_id,
                source: ArtifactError::InvalidEvidenceLocator { id, .. },
            } if memory_id == "memory-invalid-evidence"
                && artifact_id.as_str() == "invalid-memory-artifact"
                && id.as_str() == "invalid-memory-artifact"
        ));
    }

    #[test]
    fn valid_memory_evidence_appears_in_snapshot_deterministically() {
        let memory = activated_memory_with_evidence(
            "memory-evidence",
            MemoryScope::Session,
            "Memory with sorted evidence.",
            vec![
                memory_evidence(
                    "z label",
                    "artifact-b",
                    EvidenceLocator::line_range(1, 1).expect("valid line"),
                ),
                memory_evidence("a label", "artifact-a", EvidenceLocator::whole_artifact()),
                memory_evidence(
                    "b label",
                    "artifact-a",
                    EvidenceLocator::byte_range(0, 6).expect("valid byte"),
                ),
            ],
            score(1, 0, 0.5),
            ranked_reasons(1, 0, 0.5),
            provenance(),
        );
        let snapshot = memory_snapshot(vec![memory]);

        let compiled = ContextCompiler::new()
            .compile(&snapshot)
            .expect("memory evidence compiles")
            .to_snapshot();

        assert!(compiled.contains("memory-evidence:b label:artifact-a:byte:0-6\nmemory-evidence:a label:artifact-a:whole\nmemory-evidence:z label:artifact-b:line:1-1"));
    }

    #[test]
    fn evidence_and_provenance_ordering_is_independent_of_insertion_order() {
        let first = activated_memory_with_evidence(
            "memory-ordered",
            MemoryScope::Session,
            "Memory with shuffled evidence.",
            vec![
                memory_evidence("z label", "artifact-z", EvidenceLocator::whole_artifact()),
                memory_evidence("a label", "artifact-a", EvidenceLocator::whole_artifact()),
            ],
            score(1, 0, 0.5),
            ranked_reasons(1, 0, 0.5),
            MemoryActivationProvenance::new(
                "Topic",
                vec![MemoryScope::Step, MemoryScope::Session],
                MemoryActivationSourceKind::UserQuery,
                "User request",
            )
            .expect("provenance is valid"),
        );
        let second = activated_memory_with_evidence(
            "memory-ordered",
            MemoryScope::Session,
            "Memory with shuffled evidence.",
            vec![
                memory_evidence("a label", "artifact-a", EvidenceLocator::whole_artifact()),
                memory_evidence("z label", "artifact-z", EvidenceLocator::whole_artifact()),
            ],
            score(1, 0, 0.5),
            ranked_reasons(1, 0, 0.5),
            MemoryActivationProvenance::new(
                "  topic  ",
                vec![MemoryScope::Session, MemoryScope::Step, MemoryScope::Step],
                MemoryActivationSourceKind::UserQuery,
                "User   request",
            )
            .expect("provenance is valid"),
        );

        let first = ContextCompiler::new()
            .compile(&memory_snapshot(vec![first]))
            .expect("first compiles")
            .to_snapshot();
        let second = ContextCompiler::new()
            .compile(&memory_snapshot(vec![second]))
            .expect("second compiles")
            .to_snapshot();

        assert_eq!(first, second);
        assert!(first.contains(
            "memory-activation-allowed-scopes:session,step\nmemory-evidence:a label:artifact-a:whole\nmemory-evidence:z label:artifact-z:whole"
        ));
    }

    #[test]
    fn summaries_and_memory_compile_together() {
        let mut artifacts = ArtifactRegistry::default();
        artifacts
            .record(
                ArtifactRef::new(artifact_id("artifact-a"), ArtifactKind::Text),
                ArtifactContent::text("exact evidence\n"),
            )
            .expect("artifact records");
        let summary = ContextEntry::summary(
            ContextSummary::new(
                "summary-a",
                "Navigation.",
                vec![
                    ContextEvidence::new(
                        "whole artifact",
                        EvidenceRef::new(
                            artifact_id("artifact-a"),
                            EvidenceLocator::whole_artifact(),
                        ),
                    )
                    .expect("evidence metadata is valid"),
                ],
            )
            .expect("summary fields are valid"),
        );
        let memory = activated_memory(
            "memory-a",
            MemoryScope::Session,
            "Internal memory.",
            score(1, 0, 0.5),
            ranked_reasons(1, 0, 0.5),
        );
        record_text_artifacts(&mut artifacts, std::slice::from_ref(&memory));
        let snapshot = SessionContextSnapshot::new(vec![summary], artifacts, vec![memory]);

        let compiled = ContextCompiler::new()
            .compile(&snapshot)
            .expect("summary and memory compile");

        assert_eq!(compiled.sections().len(), 1);
        assert_eq!(
            compiled.to_snapshot(),
            [
                "summary:summary-a",
                "text:Navigation.",
                "evidence:whole artifact:artifact-a:whole",
                "memory:memory-a",
                "memory-scope:session",
                "memory-text:Internal memory.",
                "memory-activation-source-kind:user_query",
                "memory-activation-source-label:user request",
                "memory-activation-query:topic",
                "memory-activation-allowed-scopes:session,task,step",
                "memory-evidence:primary source:memory-a-artifact:whole",
                "memory-reason:scope_allowed",
                "memory-reason:trigger:topic",
                "memory-reason:rank:matches=1;priority=0;confidence=0.500",
            ]
            .join("\n")
        );
    }

    fn activated_memory(
        id: &str,
        scope: MemoryScope,
        text: &str,
        score: MemoryActivationScore,
        reasons: Vec<MemoryActivationReason>,
    ) -> ActivatedMemory {
        activated_memory_with_evidence(
            id,
            scope,
            text,
            vec![memory_evidence(
                "primary source",
                &format!("{id}-artifact"),
                EvidenceLocator::whole_artifact(),
            )],
            score,
            reasons,
            provenance(),
        )
    }

    fn activated_memory_with_evidence(
        id: &str,
        scope: MemoryScope,
        text: &str,
        evidence: Vec<MemoryEvidence>,
        score: MemoryActivationScore,
        reasons: Vec<MemoryActivationReason>,
        provenance: MemoryActivationProvenance,
    ) -> ActivatedMemory {
        let item = MemoryItem::new(
            memory_id(id),
            scope,
            text,
            evidence,
            memory_selection(score.confidence().as_f32(), score.priority()),
        )
        .expect("memory item is valid");
        ActivatedMemory::new(item, score, reasons, provenance).expect("activated memory is valid")
    }

    fn ranked_reasons(
        matches: usize,
        priority: i32,
        confidence: f32,
    ) -> Vec<MemoryActivationReason> {
        vec![
            MemoryActivationReason::ScopeAllowed,
            MemoryActivationReason::trigger_matched("topic").expect("valid trigger"),
            MemoryActivationReason::ranked(score(matches, priority, confidence)),
        ]
    }

    fn score(matches: usize, priority: i32, confidence: f32) -> MemoryActivationScore {
        MemoryActivationScore::new(matches, priority, confidence).expect("score is valid")
    }

    fn provenance() -> MemoryActivationProvenance {
        labeled_provenance("user request")
    }

    fn labeled_provenance(label: &str) -> MemoryActivationProvenance {
        MemoryActivationProvenance::new(
            "topic",
            vec![MemoryScope::Session, MemoryScope::Task, MemoryScope::Step],
            MemoryActivationSourceKind::UserQuery,
            label,
        )
        .expect("provenance is valid")
    }

    fn memory_id(value: &str) -> MemoryId {
        MemoryId::new(value).expect("memory id is valid")
    }

    fn artifact_id(value: &str) -> ArtifactId {
        ArtifactId::new(value).expect("artifact id is valid")
    }

    fn memory_evidence(label: &str, artifact: &str, locator: EvidenceLocator) -> MemoryEvidence {
        MemoryEvidence::new(label, EvidenceRef::new(artifact_id(artifact), locator))
            .expect("memory evidence is valid")
    }

    fn memory_selection(confidence: f32, priority: i32) -> MemoryItemSelection {
        MemoryItemSelection::new(vec!["topic".to_owned()], confidence, priority, None)
            .expect("memory selection is valid")
    }

    fn memory_snapshot(memories: Vec<ActivatedMemory>) -> SessionContextSnapshot {
        snapshot_with_memories(Vec::new(), memories)
    }

    fn snapshot_with_memories(
        entries: Vec<ContextEntry>,
        memories: Vec<ActivatedMemory>,
    ) -> SessionContextSnapshot {
        let mut artifacts = ArtifactRegistry::default();
        record_text_artifacts(&mut artifacts, &memories);
        SessionContextSnapshot::new(entries, artifacts, memories)
    }

    fn record_text_artifacts(artifacts: &mut ArtifactRegistry, memories: &[ActivatedMemory]) {
        let mut seen = std::collections::BTreeSet::new();

        for memory in memories {
            for evidence in memory.item().evidence() {
                if !seen.insert(evidence.reference().artifact_id.clone()) {
                    continue;
                }

                artifacts
                    .record(
                        ArtifactRef::new(
                            evidence.reference().artifact_id.clone(),
                            ArtifactKind::Text,
                        ),
                        ArtifactContent::text(format!(
                            "evidence for {}\n{}",
                            memory.item().id(),
                            memory.item().text()
                        )),
                    )
                    .expect("memory artifact records");
            }
        }
    }
}
