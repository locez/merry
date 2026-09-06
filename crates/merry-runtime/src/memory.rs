//! Internal memory activation data shapes.
//!
//! This module is deliberately crate-internal. Activated memory is projected
//! into session-owned context snapshots for compiler use, but it is not part of
//! the provider, event, ledger, or public runtime surface.

// Staged internal activation types are compiled before every call path is wired.
#![cfg_attr(not(test), allow(dead_code))]

use merry_core::EvidenceRef;
use std::{cmp::Ordering, fmt};
use thiserror::Error;

mod activation;
mod source;

pub(crate) use activation::MemoryActivator;
#[cfg(test)]
pub(crate) use source::MemoryActivationFuture;
pub(crate) use source::{
    MemoryActivationContext, MemoryActivationSource, MemoryStore, StoredMemoryActivationSource,
};

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

#[cfg(test)]
mod tests;
