//! Internal memory activation data shapes.
//!
//! This module is deliberately crate-internal. Activated memory is projected
//! into session-owned context snapshots for compiler use, but it is not part of
//! the provider, event, ledger, or public runtime surface.

// Staged internal activation types are compiled before every call path is wired.
#![cfg_attr(not(test), allow(dead_code))]

use merry_core::EvidenceRef;
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    fmt,
    future::Future,
    pin::Pin,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Validated internal memory identifier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct MemoryId(String);

impl MemoryId {
    pub(crate) fn new(value: &str) -> Result<Self, MemoryError> {
        validate_non_blank("memory id", value)?;
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MemoryId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Scope where a memory item applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MemoryScope {
    /// Session-wide memory.
    Session,
    /// Task-scoped memory.
    Task,
    /// Step-scoped memory.
    Step,
}

/// Validated confidence in the inclusive 0.0..=1.0 range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MemoryConfidence(f32);

impl MemoryConfidence {
    pub(crate) fn new(value: f32) -> Result<Self, MemoryError> {
        if !(0.0..=1.0).contains(&value) {
            return Err(MemoryError::ConfidenceOutOfRange { value });
        }

        let canonical = if value == 0.0 { 0.0 } else { value };
        Ok(Self(canonical))
    }

    #[must_use]
    pub(crate) fn as_f32(self) -> f32 {
        self.0
    }
}

impl Eq for MemoryConfidence {}

impl Ord for MemoryConfidence {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl PartialOrd for MemoryConfidence {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Exact evidence supporting an internal memory item's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MemoryEvidence {
    label: String,
    reference: EvidenceRef,
}

impl MemoryEvidence {
    pub(crate) fn new(
        label: impl Into<String>,
        reference: EvidenceRef,
    ) -> Result<Self, MemoryError> {
        let label = label.into();
        validate_non_blank("memory evidence label", &label)?;

        Ok(Self { label, reference })
    }

    #[must_use]
    pub(crate) fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub(crate) fn reference(&self) -> &EvidenceRef {
        &self.reference
    }
}

/// Selection metadata used to rank and deduplicate a memory item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MemoryItemSelection {
    triggers: Vec<String>,
    confidence: MemoryConfidence,
    priority: i32,
    conflict_key: Option<String>,
}

impl MemoryItemSelection {
    pub(crate) fn new(
        triggers: Vec<String>,
        confidence: f32,
        priority: i32,
        conflict_key: Option<String>,
    ) -> Result<Self, MemoryError> {
        let mut triggers = triggers
            .into_iter()
            .map(|trigger| {
                validate_non_blank("memory trigger", &trigger)?;
                Ok(canonicalize_match_text(&trigger))
            })
            .collect::<Result<Vec<_>, MemoryError>>()?;
        triggers.sort();
        triggers.dedup();

        let conflict_key = conflict_key
            .map(|key| {
                validate_non_blank("memory conflict key", &key)?;
                Ok(canonicalize_match_text(&key))
            })
            .transpose()?;

        Ok(Self {
            triggers,
            confidence: MemoryConfidence::new(confidence)?,
            priority,
            conflict_key,
        })
    }

    fn into_parts(self) -> (Vec<String>, MemoryConfidence, i32, Option<String>) {
        (
            self.triggers,
            self.confidence,
            self.priority,
            self.conflict_key,
        )
    }
}

/// Stored memory item considered by the deterministic activator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MemoryItem {
    id: MemoryId,
    scope: MemoryScope,
    text: String,
    evidence: Vec<MemoryEvidence>,
    triggers: Vec<String>,
    confidence: MemoryConfidence,
    priority: i32,
    conflict_key: Option<String>,
}

impl MemoryItem {
    pub(crate) fn new(
        id: MemoryId,
        scope: MemoryScope,
        text: impl Into<String>,
        evidence: Vec<MemoryEvidence>,
        selection: MemoryItemSelection,
    ) -> Result<Self, MemoryError> {
        let text = text.into();
        validate_non_blank("memory text", &text)?;

        if evidence.is_empty() {
            return Err(MemoryError::EmptyMemoryEvidence { memory_id: id });
        }

        for item in &evidence {
            validate_non_blank("memory evidence label", item.label())?;
        }

        let (triggers, confidence, priority, conflict_key) = selection.into_parts();

        Ok(Self {
            id,
            scope,
            text,
            evidence,
            triggers,
            confidence,
            priority,
            conflict_key,
        })
    }

    #[must_use]
    pub(crate) fn id(&self) -> &MemoryId {
        &self.id
    }

    #[must_use]
    pub(crate) fn scope(&self) -> MemoryScope {
        self.scope
    }

    #[must_use]
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub(crate) fn evidence(&self) -> &[MemoryEvidence] {
        &self.evidence
    }

    #[must_use]
    pub(crate) fn triggers(&self) -> &[String] {
        &self.triggers
    }

    #[must_use]
    pub(crate) fn confidence(&self) -> MemoryConfidence {
        self.confidence
    }

    #[must_use]
    pub(crate) fn priority(&self) -> i32 {
        self.priority
    }

    #[must_use]
    pub(crate) fn conflict_key(&self) -> Option<&str> {
        self.conflict_key.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn new_unchecked_for_tests(
        id: MemoryId,
        scope: MemoryScope,
        text: impl Into<String>,
        evidence: Vec<MemoryEvidence>,
        selection: MemoryItemSelection,
    ) -> Result<Self, MemoryError> {
        let text = text.into();
        validate_non_blank("memory text", &text)?;

        let (triggers, confidence, priority, conflict_key) = selection.into_parts();

        Ok(Self {
            id,
            scope,
            text,
            evidence,
            triggers,
            confidence,
            priority,
            conflict_key,
        })
    }
}

/// Deterministic in-memory candidate store owned by a session.
#[derive(Debug, Clone, Default)]
pub(crate) struct MemoryStore {
    candidates: BTreeMap<MemoryId, MemoryItem>,
}

impl MemoryStore {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn record(&mut self, item: MemoryItem) -> Result<(), MemoryError> {
        let id = item.id().clone();
        match self.candidates.entry(id.clone()) {
            Entry::Occupied(_) => Err(MemoryError::DuplicateMemoryId { id }),
            Entry::Vacant(entry) => {
                entry.insert(item);
                Ok(())
            }
        }
    }

    #[must_use]
    pub(crate) fn candidate_snapshot(&self) -> Vec<MemoryItem> {
        self.candidates.values().cloned().collect()
    }
}

/// Provider-neutral source category for an activation seed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MemoryActivationSourceKind {
    /// The activation was seeded from user-visible task input.
    UserQuery,
    /// The activation was seeded from runtime-owned instructions or state.
    RuntimeInstruction,
}

impl MemoryActivationSourceKind {
    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::UserQuery => "user_query",
            Self::RuntimeInstruction => "runtime_instruction",
        }
    }
}

/// Seed metadata recorded separately from per-memory activation reasons.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct MemoryActivationProvenance {
    canonical_query: String,
    allowed_scopes: Vec<MemoryScope>,
    source_kind: MemoryActivationSourceKind,
    source_label: String,
}

impl MemoryActivationProvenance {
    pub(crate) fn new(
        query: impl Into<String>,
        mut allowed_scopes: Vec<MemoryScope>,
        source_kind: MemoryActivationSourceKind,
        source_label: impl Into<String>,
    ) -> Result<Self, MemoryError> {
        let query = query.into();
        validate_non_blank("memory activation query", &query)?;

        if allowed_scopes.is_empty() {
            return Err(MemoryError::EmptyAllowedScopes);
        }
        allowed_scopes.sort();
        allowed_scopes.dedup();

        let source_label = source_label.into();
        validate_non_blank("memory activation source label", &source_label)?;

        Ok(Self {
            canonical_query: canonicalize_match_text(&query),
            allowed_scopes,
            source_kind,
            source_label: canonicalize_label_text(&source_label),
        })
    }

    #[must_use]
    pub(crate) fn canonical_query(&self) -> &str {
        &self.canonical_query
    }

    #[must_use]
    pub(crate) fn allowed_scopes(&self) -> &[MemoryScope] {
        &self.allowed_scopes
    }

    #[must_use]
    pub(crate) fn source_kind(&self) -> MemoryActivationSourceKind {
        self.source_kind
    }

    #[must_use]
    pub(crate) fn source_label(&self) -> &str {
        &self.source_label
    }

    fn allows_scope(&self, scope: MemoryScope) -> bool {
        self.allowed_scopes.binary_search(&scope).is_ok()
    }
}

/// Query seed and scope policy used for activation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MemoryActivationSeed {
    provenance: MemoryActivationProvenance,
}

impl MemoryActivationSeed {
    pub(crate) fn new(
        query: impl Into<String>,
        allowed_scopes: Vec<MemoryScope>,
        source_kind: MemoryActivationSourceKind,
        source_label: impl Into<String>,
    ) -> Result<Self, MemoryError> {
        Ok(Self {
            provenance: MemoryActivationProvenance::new(
                query,
                allowed_scopes,
                source_kind,
                source_label,
            )?,
        })
    }

    #[must_use]
    pub(crate) fn query(&self) -> &str {
        self.provenance.canonical_query()
    }

    #[must_use]
    pub(crate) fn provenance(&self) -> &MemoryActivationProvenance {
        &self.provenance
    }

    fn allows_scope(&self, scope: MemoryScope) -> bool {
        self.provenance.allows_scope(scope)
    }
}

/// Deterministic activation score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MemoryActivationScore {
    trigger_matches: usize,
    priority: i32,
    confidence: MemoryConfidence,
}

impl MemoryActivationScore {
    pub(crate) fn new(
        trigger_matches: usize,
        priority: i32,
        confidence: f32,
    ) -> Result<Self, MemoryError> {
        Ok(Self {
            trigger_matches,
            priority,
            confidence: MemoryConfidence::new(confidence)?,
        })
    }

    #[must_use]
    pub(crate) fn trigger_matches(self) -> usize {
        self.trigger_matches
    }

    #[must_use]
    pub(crate) fn priority(self) -> i32 {
        self.priority
    }

    #[must_use]
    pub(crate) fn confidence(self) -> MemoryConfidence {
        self.confidence
    }
}

impl Ord for MemoryActivationScore {
    fn cmp(&self, other: &Self) -> Ordering {
        self.trigger_matches
            .cmp(&other.trigger_matches)
            .then_with(|| self.priority.cmp(&other.priority))
            .then_with(|| self.confidence.cmp(&other.confidence))
    }
}

impl PartialOrd for MemoryActivationScore {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Reasons an internal memory was activated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MemoryActivationReason {
    /// The memory item scope was allowed by the activation seed.
    ScopeAllowed,
    /// A trigger matched the activation seed query.
    TriggerMatched(String),
    /// The item was ranked with a deterministic score.
    Ranked { score: MemoryActivationScore },
    /// This item won a conflict group and suppressed lower-ranked items.
    ConflictWinner { suppressed: Vec<MemoryId> },
}

impl MemoryActivationReason {
    pub(crate) fn trigger_matched(trigger: impl Into<String>) -> Result<Self, MemoryError> {
        let trigger = trigger.into();
        validate_non_blank("memory activation trigger reason", &trigger)?;
        Ok(Self::TriggerMatched(canonicalize_match_text(&trigger)))
    }

    #[must_use]
    pub(crate) fn ranked(score: MemoryActivationScore) -> Self {
        Self::Ranked { score }
    }

    pub(crate) fn conflict_winner(mut suppressed: Vec<MemoryId>) -> Result<Self, MemoryError> {
        if suppressed.is_empty() {
            return Err(MemoryError::BlankActivationReason {
                reason: "conflict winner requires at least one suppressed memory id",
            });
        }

        suppressed.sort();
        suppressed.dedup();
        Ok(Self::ConflictWinner { suppressed })
    }
}

/// Selected memory plus score and activation reasons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivatedMemory {
    item: MemoryItem,
    score: MemoryActivationScore,
    reasons: Vec<MemoryActivationReason>,
    provenance: MemoryActivationProvenance,
}

impl ActivatedMemory {
    pub(crate) fn new(
        item: MemoryItem,
        score: MemoryActivationScore,
        reasons: Vec<MemoryActivationReason>,
        provenance: MemoryActivationProvenance,
    ) -> Result<Self, MemoryError> {
        if reasons.is_empty() {
            return Err(MemoryError::EmptyActivationReasons {
                memory_id: item.id.clone(),
            });
        }

        for reason in &reasons {
            validate_reason(reason)?;
        }

        Ok(Self {
            item,
            score,
            reasons,
            provenance,
        })
    }

    #[must_use]
    pub(crate) fn item(&self) -> &MemoryItem {
        &self.item
    }

    #[must_use]
    pub(crate) fn score(&self) -> MemoryActivationScore {
        self.score
    }

    #[must_use]
    pub(crate) fn reasons(&self) -> &[MemoryActivationReason] {
        &self.reasons
    }

    #[must_use]
    pub(crate) fn provenance(&self) -> &MemoryActivationProvenance {
        &self.provenance
    }

    fn add_reason(&mut self, reason: MemoryActivationReason) -> Result<(), MemoryError> {
        validate_reason(&reason)?;
        self.reasons.push(reason);
        Ok(())
    }
}

/// Result returned by a crate-internal memory activation source.
pub(crate) type MemoryActivationResult = Result<Vec<ActivatedMemory>, MemoryError>;

/// Boxed memory activation future used for object-safe async boundaries.
pub(crate) type MemoryActivationFuture<'a> =
    Pin<Box<dyn Future<Output = MemoryActivationResult> + Send + 'a>>;

/// Context passed to a memory activation source.
#[derive(Debug, Clone)]
pub(crate) struct MemoryActivationContext {
    cancellation_token: CancellationToken,
}

impl MemoryActivationContext {
    #[must_use]
    pub(crate) fn new(cancellation_token: CancellationToken) -> Self {
        Self { cancellation_token }
    }

    #[must_use]
    pub(crate) fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation_token
    }
}

/// Crate-internal source for the current provider request's memory projection.
pub(crate) trait MemoryActivationSource: Send + Sync {
    fn activate<'a>(
        &'a self,
        seed: MemoryActivationSeed,
        candidates: Vec<MemoryItem>,
        context: MemoryActivationContext,
    ) -> MemoryActivationFuture<'a>;
}

/// Production MVP source backed by the session-owned in-memory store.
#[derive(Debug, Default)]
pub(crate) struct StoredMemoryActivationSource;

impl MemoryActivationSource for StoredMemoryActivationSource {
    fn activate<'a>(
        &'a self,
        seed: MemoryActivationSeed,
        candidates: Vec<MemoryItem>,
        _context: MemoryActivationContext,
    ) -> MemoryActivationFuture<'a> {
        Box::pin(async move { MemoryActivator::activate(&seed, &candidates) })
    }
}

/// Pure deterministic internal memory activator.
#[derive(Debug, Default)]
pub(crate) struct MemoryActivator;

impl MemoryActivator {
    pub(crate) fn activate(
        seed: &MemoryActivationSeed,
        candidates: &[MemoryItem],
    ) -> Result<Vec<ActivatedMemory>, MemoryError> {
        let query = seed.query().to_lowercase();
        let mut eligible = Vec::new();
        let mut seen_ids = BTreeSet::new();

        for item in candidates {
            if !seen_ids.insert(item.id().clone()) {
                return Err(MemoryError::DuplicateMemoryId {
                    id: item.id().clone(),
                });
            }

            if !seed.allows_scope(item.scope()) {
                continue;
            }

            let matched_triggers = matched_triggers(&query, item.triggers());
            if matched_triggers.is_empty() {
                continue;
            }

            let score = MemoryActivationScore {
                trigger_matches: matched_triggers.len(),
                priority: item.priority(),
                confidence: item.confidence(),
            };

            let mut reasons = Vec::with_capacity(matched_triggers.len() + 2);
            reasons.push(MemoryActivationReason::ScopeAllowed);
            for trigger in matched_triggers {
                reasons.push(MemoryActivationReason::trigger_matched(trigger)?);
            }
            reasons.push(MemoryActivationReason::ranked(score));

            eligible.push(ActivatedMemory::new(
                item.clone(),
                score,
                reasons,
                seed.provenance().clone(),
            )?);
        }

        eligible.sort_by(|left, right| {
            right
                .score()
                .cmp(&left.score())
                .then_with(|| left.item().id().cmp(right.item().id()))
        });

        resolve_conflicts(eligible)
    }
}

/// Errors raised while constructing or activating internal memory.
#[derive(Debug, Clone, PartialEq, Error)]
pub(crate) enum MemoryError {
    /// A required field was blank.
    #[error("{field} must not be blank")]
    BlankField {
        /// Name of the invalid field.
        field: &'static str,
    },

    /// Confidence was outside the valid range.
    #[error("memory confidence {value} is outside the inclusive 0.0..=1.0 range")]
    ConfidenceOutOfRange {
        /// Rejected confidence value.
        value: f32,
    },

    /// A candidate set or memory store contained the same memory id more than once.
    #[error("memory id {id} appears more than once in memory candidates")]
    DuplicateMemoryId {
        /// Duplicate memory identifier.
        id: MemoryId,
    },

    /// Activation seed did not allow any memory scopes.
    #[error("memory activation seed must allow at least one scope")]
    EmptyAllowedScopes,

    /// Memory text must have at least one exact evidence reference.
    #[error("memory item {memory_id} must have at least one exact evidence reference")]
    EmptyMemoryEvidence {
        /// Memory id that was created without evidence.
        memory_id: MemoryId,
    },

    /// Activated memory requires at least one reason.
    #[error("activated memory {memory_id} must have at least one activation reason")]
    EmptyActivationReasons {
        /// Memory id that was activated without reasons.
        memory_id: MemoryId,
    },

    /// A reason payload was structurally empty.
    #[error("invalid memory activation reason: {reason}")]
    BlankActivationReason {
        /// Actionable reason validation detail.
        reason: &'static str,
    },
}

fn validate_non_blank(field: &'static str, value: &str) -> Result<(), MemoryError> {
    if value.trim().is_empty() {
        return Err(MemoryError::BlankField { field });
    }

    Ok(())
}

fn canonicalize_match_text(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn canonicalize_label_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn validate_reason(reason: &MemoryActivationReason) -> Result<(), MemoryError> {
    match reason {
        MemoryActivationReason::ScopeAllowed | MemoryActivationReason::Ranked { .. } => Ok(()),
        MemoryActivationReason::TriggerMatched(trigger) => {
            validate_non_blank("memory activation trigger reason", trigger)
        }
        MemoryActivationReason::ConflictWinner { suppressed } => {
            if suppressed.is_empty() {
                return Err(MemoryError::BlankActivationReason {
                    reason: "conflict winner requires at least one suppressed memory id",
                });
            }

            Ok(())
        }
    }
}

fn matched_triggers(query_lowercase: &str, triggers: &[String]) -> Vec<String> {
    triggers
        .iter()
        .filter(|trigger| query_lowercase.contains(&trigger.to_lowercase()))
        .cloned()
        .collect()
}

fn resolve_conflicts(
    activations: Vec<ActivatedMemory>,
) -> Result<Vec<ActivatedMemory>, MemoryError> {
    let mut selected = Vec::with_capacity(activations.len());
    let mut conflict_winners: BTreeMap<String, usize> = BTreeMap::new();
    let mut suppressed_by_winner: BTreeMap<usize, Vec<MemoryId>> = BTreeMap::new();

    for activation in activations {
        if let Some(conflict_key) = activation.item().conflict_key() {
            let conflict_key = conflict_key.to_owned();
            if let Some(winner_index) = conflict_winners.get(&conflict_key) {
                suppressed_by_winner
                    .entry(*winner_index)
                    .or_default()
                    .push(activation.item().id().clone());
                continue;
            }

            conflict_winners.insert(conflict_key, selected.len());
        }

        selected.push(activation);
    }

    for (winner_index, suppressed) in suppressed_by_winner {
        selected[winner_index].add_reason(MemoryActivationReason::conflict_winner(suppressed)?)?;
    }

    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use merry_core::{ArtifactId, EvidenceLocator, EvidenceRef};

    #[test]
    fn validation_rejects_blank_id_text_trigger_and_reason() {
        assert!(matches!(
            MemoryId::new(" "),
            Err(MemoryError::BlankField { field: "memory id" })
        ));

        assert!(matches!(
            MemoryItem::new(
                memory_id("blank-text"),
                MemoryScope::Session,
                " ",
                vec![evidence("source")],
                selection(&["topic"], 0.5, 0, None),
            ),
            Err(MemoryError::BlankField {
                field: "memory text"
            })
        ));

        assert!(matches!(
            MemoryItemSelection::new(vec![" ".to_owned()], 0.5, 0, None),
            Err(MemoryError::BlankField {
                field: "memory trigger"
            })
        ));

        assert!(matches!(
            MemoryActivationReason::trigger_matched(" "),
            Err(MemoryError::BlankField {
                field: "memory activation trigger reason"
            })
        ));
    }

    #[test]
    fn memory_item_rejects_empty_evidence_and_blank_evidence_label() {
        assert!(matches!(
            MemoryItem::new(
                memory_id("without-evidence"),
                MemoryScope::Session,
                "remember this",
                Vec::new(),
                selection(&["topic"], 0.5, 0, None),
            ),
            Err(MemoryError::EmptyMemoryEvidence { memory_id })
                if memory_id.as_str() == "without-evidence"
        ));

        assert!(matches!(
            MemoryEvidence::new(" ", evidence_ref("artifact-blank-label")),
            Err(MemoryError::BlankField {
                field: "memory evidence label"
            })
        ));

        assert!(matches!(
            MemoryItem::new(
                memory_id("blank-evidence-label"),
                MemoryScope::Session,
                "remember this",
                vec![MemoryEvidence {
                    label: " ".to_owned(),
                    reference: evidence_ref("artifact-blank-label"),
                }],
                selection(&["topic"], 0.5, 0, None),
            ),
            Err(MemoryError::BlankField {
                field: "memory evidence label"
            })
        ));
    }

    #[test]
    fn activation_seed_rejects_blank_query_and_empty_scopes() {
        assert!(matches!(
            MemoryActivationSeed::new(
                " ",
                vec![MemoryScope::Session],
                MemoryActivationSourceKind::UserQuery,
                "user request",
            ),
            Err(MemoryError::BlankField {
                field: "memory activation query"
            })
        ));

        assert!(matches!(
            MemoryActivationSeed::new(
                "topic",
                Vec::new(),
                MemoryActivationSourceKind::UserQuery,
                "user request",
            ),
            Err(MemoryError::EmptyAllowedScopes)
        ));

        assert!(matches!(
            MemoryActivationSeed::new(
                "topic",
                vec![MemoryScope::Session],
                MemoryActivationSourceKind::UserQuery,
                " ",
            ),
            Err(MemoryError::BlankField {
                field: "memory activation source label"
            })
        ));
    }

    #[test]
    fn validation_rejects_confidence_outside_range() {
        assert!(matches!(
            MemoryItemSelection::new(vec!["topic".to_owned()], -0.1, 0, None),
            Err(MemoryError::ConfidenceOutOfRange { .. })
        ));

        assert!(matches!(
            MemoryItemSelection::new(vec!["topic".to_owned()], 1.1, 0, None),
            Err(MemoryError::ConfidenceOutOfRange { .. })
        ));

        assert!(matches!(
            MemoryActivationScore::new(1, 0, f32::NAN),
            Err(MemoryError::ConfidenceOutOfRange { .. })
        ));
    }

    #[test]
    fn activation_output_is_independent_of_candidate_order() {
        let first = item("first", MemoryScope::Session, &["alpha"], 0.8, 3, None);
        let second = item("second", MemoryScope::Session, &["alpha"], 0.9, 3, None);
        let third = item("third", MemoryScope::Task, &["alpha"], 0.4, 7, None);
        let seed = seed(
            "ALPHA request",
            vec![MemoryScope::Session, MemoryScope::Task],
        );

        let ordered =
            MemoryActivator::activate(&seed, &[first.clone(), second.clone(), third.clone()])
                .expect("activation succeeds");
        let shuffled =
            MemoryActivator::activate(&seed, &[third, first, second]).expect("activation succeeds");

        assert_eq!(ordered, shuffled);
        assert_eq!(ids(&ordered), ["third", "second", "first"]);
    }

    #[test]
    fn activation_filters_disallowed_scopes() {
        let session = item("session", MemoryScope::Session, &["billing"], 0.5, 0, None);
        let task = item("task", MemoryScope::Task, &["billing"], 0.5, 0, None);
        let step = item("step", MemoryScope::Step, &["billing"], 0.5, 0, None);
        let seed = seed("billing issue", vec![MemoryScope::Session]);

        let activated =
            MemoryActivator::activate(&seed, &[task, step, session]).expect("activation succeeds");

        assert_eq!(ids(&activated), ["session"]);
    }

    #[test]
    fn activation_records_trigger_reason() {
        let memory = item(
            "ownership",
            MemoryScope::Session,
            &["Rust", "Python"],
            0.7,
            0,
            None,
        );
        let seed = seed("debug rust ownership", vec![MemoryScope::Session]);

        let activated = MemoryActivator::activate(&seed, &[memory]).expect("activation succeeds");

        assert_eq!(activated.len(), 1);
        assert!(
            activated[0]
                .reasons()
                .contains(&MemoryActivationReason::ScopeAllowed)
        );
        assert!(
            activated[0]
                .reasons()
                .contains(&MemoryActivationReason::TriggerMatched("rust".to_owned()))
        );
        assert!(activated[0].reasons().iter().any(|reason| matches!(
            reason,
            MemoryActivationReason::Ranked { score }
                if score.trigger_matches() == 1
                    && score.priority() == 0
                    && score.confidence().as_f32() == 0.7
        )));
    }

    #[test]
    fn activation_records_seed_provenance_separate_from_per_memory_reasons() {
        let memory = item("provenance", MemoryScope::Session, &["Rust"], 0.7, 0, None);
        let seed = MemoryActivationSeed::new(
            "  Debug   RUST ownership  ",
            vec![
                MemoryScope::Step,
                MemoryScope::Session,
                MemoryScope::Session,
            ],
            MemoryActivationSourceKind::RuntimeInstruction,
            "  Step   planner  ",
        )
        .expect("seed is valid");

        let activated = MemoryActivator::activate(&seed, &[memory]).expect("activation succeeds");

        assert_eq!(activated.len(), 1);
        let provenance = activated[0].provenance();
        assert_eq!(provenance.canonical_query(), "debug rust ownership");
        assert_eq!(
            provenance.allowed_scopes(),
            &[MemoryScope::Session, MemoryScope::Step]
        );
        assert_eq!(
            provenance.source_kind(),
            MemoryActivationSourceKind::RuntimeInstruction
        );
        assert_eq!(provenance.source_label(), "Step planner");
        assert!(
            activated[0]
                .reasons()
                .contains(&MemoryActivationReason::ScopeAllowed)
        );
        assert!(activated[0].reasons().iter().any(|reason| matches!(
            reason,
            MemoryActivationReason::TriggerMatched(trigger) if trigger == "rust"
        )));
        assert!(
            activated[0]
                .reasons()
                .iter()
                .any(|reason| matches!(reason, MemoryActivationReason::Ranked { .. }))
        );
    }

    #[test]
    fn activation_sorts_by_priority_confidence_and_id() {
        let candidates = vec![
            item("id-b", MemoryScope::Session, &["topic"], 0.5, 1, None),
            item(
                "confidence-low",
                MemoryScope::Session,
                &["topic"],
                0.1,
                5,
                None,
            ),
            item("priority", MemoryScope::Session, &["topic"], 0.1, 10, None),
            item("id-a", MemoryScope::Session, &["topic"], 0.5, 1, None),
            item(
                "confidence-high",
                MemoryScope::Session,
                &["topic"],
                0.9,
                5,
                None,
            ),
        ];
        let seed = seed("topic", vec![MemoryScope::Session]);

        let activated = MemoryActivator::activate(&seed, &candidates).expect("activation succeeds");

        assert_eq!(
            ids(&activated),
            [
                "priority",
                "confidence-high",
                "confidence-low",
                "id-a",
                "id-b"
            ]
        );
    }

    #[test]
    fn conflict_winner_reason_lists_suppressed_ids() {
        let candidates = vec![
            item(
                "suppressed-b",
                MemoryScope::Session,
                &["topic"],
                0.7,
                1,
                Some("shared"),
            ),
            item(
                "winner",
                MemoryScope::Session,
                &["topic"],
                0.7,
                10,
                Some("shared"),
            ),
            item(
                "independent",
                MemoryScope::Session,
                &["topic"],
                0.7,
                0,
                None,
            ),
            item(
                "suppressed-a",
                MemoryScope::Session,
                &["topic"],
                0.7,
                5,
                Some("shared"),
            ),
        ];
        let seed = seed("topic", vec![MemoryScope::Session]);

        let activated = MemoryActivator::activate(&seed, &candidates).expect("activation succeeds");

        assert_eq!(ids(&activated), ["winner", "independent"]);

        let winner = &activated[0];
        assert!(winner.reasons().iter().any(|reason| matches!(
            reason,
            MemoryActivationReason::ConflictWinner { suppressed }
                if ids_from_memory_ids(suppressed) == ["suppressed-a", "suppressed-b"]
        )));
    }

    #[test]
    fn duplicate_triggers_do_not_duplicate_reasons_or_inflate_score() {
        let memory = item(
            "deduped",
            MemoryScope::Session,
            &["Rust", " rust ", "RUST"],
            0.5,
            0,
            None,
        );
        let seed = seed("debug rust ownership", vec![MemoryScope::Session]);

        let activated = MemoryActivator::activate(&seed, &[memory]).expect("activation succeeds");

        let trigger_reasons = activated[0]
            .reasons()
            .iter()
            .filter(|reason| matches!(reason, MemoryActivationReason::TriggerMatched(_)))
            .count();
        assert_eq!(trigger_reasons, 1);
        assert!(activated[0].reasons().iter().any(|reason| matches!(
            reason,
            MemoryActivationReason::Ranked { score } if score.trigger_matches() == 1
        )));
    }

    #[test]
    fn conflict_key_canonicalization_groups_trimmed_case_variants() {
        let candidates = vec![
            item(
                "winner",
                MemoryScope::Session,
                &["topic"],
                0.7,
                10,
                Some(" Shared "),
            ),
            item(
                "suppressed",
                MemoryScope::Session,
                &["topic"],
                0.7,
                1,
                Some("shared"),
            ),
        ];
        let seed = seed("topic", vec![MemoryScope::Session]);

        let activated = MemoryActivator::activate(&seed, &candidates).expect("activation succeeds");

        assert_eq!(ids(&activated), ["winner"]);
        assert!(activated[0].reasons().iter().any(|reason| matches!(
            reason,
            MemoryActivationReason::ConflictWinner { suppressed }
                if ids_from_memory_ids(suppressed) == ["suppressed"]
        )));
    }

    #[test]
    fn activation_rejects_duplicate_memory_ids() {
        let candidates = vec![
            item("duplicate", MemoryScope::Session, &["topic"], 0.7, 10, None),
            item("duplicate", MemoryScope::Task, &["topic"], 0.7, 1, None),
        ];
        let seed = seed("topic", vec![MemoryScope::Session, MemoryScope::Task]);

        let error = MemoryActivator::activate(&seed, &candidates)
            .expect_err("duplicate ids should be rejected");

        assert!(matches!(
            error,
            MemoryError::DuplicateMemoryId { id } if id.as_str() == "duplicate"
        ));
    }

    #[test]
    fn store_candidate_snapshot_is_deterministic() {
        let mut store = MemoryStore::new();
        store
            .record(item(
                "memory-b",
                MemoryScope::Session,
                &["topic"],
                0.5,
                0,
                None,
            ))
            .expect("memory b records");
        store
            .record(item(
                "memory-a",
                MemoryScope::Session,
                &["topic"],
                0.5,
                0,
                None,
            ))
            .expect("memory a records");

        let snapshot = store.candidate_snapshot();

        assert_eq!(
            snapshot
                .iter()
                .map(|memory| memory.id().as_str())
                .collect::<Vec<_>>(),
            ["memory-a", "memory-b"]
        );
    }

    #[test]
    fn store_rejects_duplicate_memory_id() {
        let mut store = MemoryStore::new();
        store
            .record(item(
                "duplicate",
                MemoryScope::Session,
                &["topic"],
                0.5,
                0,
                None,
            ))
            .expect("first duplicate records");

        let error = store
            .record(item(
                "duplicate",
                MemoryScope::Task,
                &["topic"],
                0.5,
                1,
                None,
            ))
            .expect_err("duplicate memory id is rejected");

        assert!(matches!(
            error,
            MemoryError::DuplicateMemoryId { id } if id.as_str() == "duplicate"
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stored_source_activates_matching_candidate() {
        let mut store = MemoryStore::new();
        store
            .record(item(
                "stored-topic",
                MemoryScope::Session,
                &["topic"],
                0.5,
                0,
                None,
            ))
            .expect("memory records");
        let source = StoredMemoryActivationSource;

        let activated = source
            .activate(
                seed("topic request", vec![MemoryScope::Session]),
                store.candidate_snapshot(),
                MemoryActivationContext::new(CancellationToken::new()),
            )
            .await
            .expect("activation succeeds");

        assert_eq!(ids(&activated), ["stored-topic"]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stored_source_ignores_unmatched_trigger() {
        let mut store = MemoryStore::new();
        store
            .record(item(
                "stored-other",
                MemoryScope::Session,
                &["other"],
                0.5,
                0,
                None,
            ))
            .expect("memory records");
        let source = StoredMemoryActivationSource;

        let activated = source
            .activate(
                seed("topic request", vec![MemoryScope::Session]),
                store.candidate_snapshot(),
                MemoryActivationContext::new(CancellationToken::new()),
            )
            .await
            .expect("activation succeeds");

        assert!(activated.is_empty());
    }

    #[test]
    fn activated_memory_rejects_empty_reasons() {
        let memory = item(
            "empty-reasons",
            MemoryScope::Session,
            &["topic"],
            0.5,
            0,
            None,
        );
        let score = MemoryActivationScore::new(1, 0, 0.5).expect("score is valid");

        let error = ActivatedMemory::new(memory, score, Vec::new(), provenance())
            .expect_err("reasons required");

        assert!(matches!(
            error,
            MemoryError::EmptyActivationReasons { memory_id }
                if memory_id.as_str() == "empty-reasons"
        ));
    }

    fn memory_id(value: &str) -> MemoryId {
        MemoryId::new(value).expect("memory id is valid")
    }

    fn artifact_id(value: &str) -> ArtifactId {
        ArtifactId::new(value).expect("artifact id is valid")
    }

    fn evidence_ref(id: &str) -> EvidenceRef {
        EvidenceRef::new(artifact_id(id), EvidenceLocator::whole_artifact())
    }

    fn evidence(label: &str) -> MemoryEvidence {
        MemoryEvidence::new(label, evidence_ref(&format!("artifact-{label}")))
            .expect("memory evidence is valid")
    }

    fn provenance() -> MemoryActivationProvenance {
        MemoryActivationProvenance::new(
            "topic",
            vec![MemoryScope::Session],
            MemoryActivationSourceKind::UserQuery,
            "user request",
        )
        .expect("provenance is valid")
    }

    fn seed(query: &str, scopes: Vec<MemoryScope>) -> MemoryActivationSeed {
        MemoryActivationSeed::new(
            query,
            scopes,
            MemoryActivationSourceKind::UserQuery,
            "user request",
        )
        .expect("seed is valid")
    }

    fn item(
        id: &str,
        scope: MemoryScope,
        triggers: &[&str],
        confidence: f32,
        priority: i32,
        conflict_key: Option<&str>,
    ) -> MemoryItem {
        MemoryItem::new(
            memory_id(id),
            scope,
            format!("{id} text"),
            vec![evidence("source")],
            selection(triggers, confidence, priority, conflict_key),
        )
        .expect("memory item is valid")
    }

    fn selection(
        triggers: &[&str],
        confidence: f32,
        priority: i32,
        conflict_key: Option<&str>,
    ) -> MemoryItemSelection {
        MemoryItemSelection::new(
            triggers
                .iter()
                .map(|trigger| (*trigger).to_owned())
                .collect(),
            confidence,
            priority,
            conflict_key.map(str::to_owned),
        )
        .expect("memory item selection is valid")
    }

    fn ids(activated: &[ActivatedMemory]) -> Vec<&str> {
        activated
            .iter()
            .map(|memory| memory.item().id().as_str())
            .collect()
    }

    fn ids_from_memory_ids(ids: &[MemoryId]) -> Vec<&str> {
        ids.iter().map(MemoryId::as_str).collect()
    }
}
