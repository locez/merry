//! Internal memory activation data shapes.
//!
//! This module is deliberately crate-internal and not connected to runtime,
//! context compilation, providers, events, or the task ledger yet.

#![allow(dead_code)]

use std::{cmp::Ordering, collections::BTreeMap, fmt};
use thiserror::Error;

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

/// Stored memory item considered by the deterministic activator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MemoryItem {
    id: MemoryId,
    scope: MemoryScope,
    text: String,
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
        triggers: Vec<String>,
        confidence: f32,
        priority: i32,
        conflict_key: Option<String>,
    ) -> Result<Self, MemoryError> {
        let text = text.into();
        validate_non_blank("memory text", &text)?;

        for trigger in &triggers {
            validate_non_blank("memory trigger", trigger)?;
        }

        if let Some(key) = conflict_key.as_deref() {
            validate_non_blank("memory conflict key", key)?;
        }

        Ok(Self {
            id,
            scope,
            text,
            triggers,
            confidence: MemoryConfidence::new(confidence)?,
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
}

/// Query seed and scope policy used for activation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MemoryActivationSeed {
    query: String,
    allowed_scopes: Vec<MemoryScope>,
}

impl MemoryActivationSeed {
    pub(crate) fn new(query: impl Into<String>, mut allowed_scopes: Vec<MemoryScope>) -> Self {
        allowed_scopes.sort();
        allowed_scopes.dedup();

        Self {
            query: query.into(),
            allowed_scopes,
        }
    }

    #[must_use]
    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    fn allows_scope(&self, scope: MemoryScope) -> bool {
        self.allowed_scopes.binary_search(&scope).is_ok()
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
        Ok(Self::TriggerMatched(trigger))
    }

    #[must_use]
    pub(crate) fn ranked(score: MemoryActivationScore) -> Self {
        Self::Ranked { score }
    }

    pub(crate) fn conflict_winner(suppressed: Vec<MemoryId>) -> Result<Self, MemoryError> {
        if suppressed.is_empty() {
            return Err(MemoryError::BlankActivationReason {
                reason: "conflict winner requires at least one suppressed memory id",
            });
        }

        Ok(Self::ConflictWinner { suppressed })
    }
}

/// Selected memory plus score and activation reasons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivatedMemory {
    item: MemoryItem,
    score: MemoryActivationScore,
    reasons: Vec<MemoryActivationReason>,
}

impl ActivatedMemory {
    pub(crate) fn new(
        item: MemoryItem,
        score: MemoryActivationScore,
        reasons: Vec<MemoryActivationReason>,
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

    fn add_reason(&mut self, reason: MemoryActivationReason) -> Result<(), MemoryError> {
        validate_reason(&reason)?;
        self.reasons.push(reason);
        Ok(())
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

        for item in candidates {
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

            eligible.push(ActivatedMemory::new(item.clone(), score, reasons)?);
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
                vec!["topic".to_owned()],
                0.5,
                0,
                None,
            ),
            Err(MemoryError::BlankField {
                field: "memory text"
            })
        ));

        assert!(matches!(
            MemoryItem::new(
                memory_id("blank-trigger"),
                MemoryScope::Session,
                "remember this",
                vec![" ".to_owned()],
                0.5,
                0,
                None,
            ),
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
    fn validation_rejects_confidence_outside_range() {
        assert!(matches!(
            MemoryItem::new(
                memory_id("low-confidence"),
                MemoryScope::Session,
                "remember this",
                vec!["topic".to_owned()],
                -0.1,
                0,
                None,
            ),
            Err(MemoryError::ConfidenceOutOfRange { .. })
        ));

        assert!(matches!(
            MemoryItem::new(
                memory_id("high-confidence"),
                MemoryScope::Session,
                "remember this",
                vec!["topic".to_owned()],
                1.1,
                0,
                None,
            ),
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
        let seed = MemoryActivationSeed::new(
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
        let seed = MemoryActivationSeed::new("billing issue", vec![MemoryScope::Session]);

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
        let seed = MemoryActivationSeed::new("debug rust ownership", vec![MemoryScope::Session]);

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
                .contains(&MemoryActivationReason::TriggerMatched("Rust".to_owned()))
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
        let seed = MemoryActivationSeed::new("topic", vec![MemoryScope::Session]);

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
        let seed = MemoryActivationSeed::new("topic", vec![MemoryScope::Session]);

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

        let error = ActivatedMemory::new(memory, score, Vec::new()).expect_err("reasons required");

        assert!(matches!(
            error,
            MemoryError::EmptyActivationReasons { memory_id }
                if memory_id.as_str() == "empty-reasons"
        ));
    }

    fn memory_id(value: &str) -> MemoryId {
        MemoryId::new(value).expect("memory id is valid")
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
            triggers
                .iter()
                .map(|trigger| (*trigger).to_owned())
                .collect(),
            confidence,
            priority,
            conflict_key.map(str::to_owned),
        )
        .expect("memory item is valid")
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
