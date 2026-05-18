//! In-memory task ledger update primitives.

use std::fmt;
use thiserror::Error;

/// Compact record of state transitions that happened before event emission.
#[derive(Debug, Default)]
pub struct TaskLedger {
    entries: Vec<LedgerEntryRef>,
    updates: Vec<LedgerUpdate>,
    lifecycle_facts: Vec<LifecycleFact>,
    next_sequence: u64,
    next_order: u64,
}

impl TaskLedger {
    /// Appends a compact ledger update and returns the recorded entry.
    pub fn append(&mut self, kind: LedgerUpdateKind) -> &LedgerUpdate {
        let update = LedgerUpdate {
            sequence: self.next_sequence,
            order: self.next_order,
            kind,
        };
        self.next_sequence += 1;
        self.next_order += 1;
        self.updates.push(update);
        self.entries
            .push(LedgerEntryRef::Update(self.updates.len() - 1));
        self.updates
            .last()
            .expect("ledger update was just appended")
    }

    pub(crate) fn record(&mut self, sequence: u64, kind: LedgerFactKind) {
        self.record_lifecycle(sequence, kind);
    }

    /// Records an event lifecycle fact that was durably written before event emission.
    pub fn record_lifecycle(&mut self, sequence: u64, kind: LedgerFactKind) -> &LifecycleFact {
        let fact = LifecycleFact {
            sequence,
            order: self.next_order,
            kind,
        };
        self.next_sequence = self.next_sequence.max(sequence.saturating_add(1));
        self.next_order += 1;
        self.lifecycle_facts.push(fact);
        self.entries
            .push(LedgerEntryRef::Lifecycle(self.lifecycle_facts.len() - 1));
        self.lifecycle_facts
            .last()
            .expect("lifecycle fact was just appended")
    }

    /// Returns compact updates in append order.
    #[must_use]
    pub fn updates(&self) -> &[LedgerUpdate] {
        &self.updates
    }

    /// Returns lifecycle facts in append order.
    #[must_use]
    pub fn lifecycle_facts(&self) -> &[LifecycleFact] {
        &self.lifecycle_facts
    }

    /// Builds a deterministic projection suitable for future context compilation.
    #[must_use]
    pub fn project(&self) -> LedgerProjectionSnapshot {
        let entries = self
            .entries
            .iter()
            .map(|entry| match entry {
                LedgerEntryRef::Update(index) => {
                    let update = self
                        .updates
                        .get(*index)
                        .expect("ledger entry references an existing update");
                    match update.kind() {
                        LedgerUpdateKind::Observation { scope, summary } => {
                            LedgerProjection::Fact {
                                sequence: update.sequence(),
                                order: update.order(),
                                scope: *scope,
                                text: summary.as_str().to_owned(),
                            }
                        }
                    }
                }
                LedgerEntryRef::Lifecycle(index) => {
                    let fact = self
                        .lifecycle_facts
                        .get(*index)
                        .expect("ledger entry references an existing lifecycle fact");
                    LedgerProjection::Lifecycle {
                        sequence: fact.sequence(),
                        order: fact.order(),
                        kind: fact.kind(),
                    }
                }
            })
            .collect();

        LedgerProjectionSnapshot { entries }
    }
}

/// Scope for a compact ledger update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerScope {
    /// A fact that applies to the current runtime session.
    Session,
    /// A fact that applies to the current task.
    Task,
    /// A fact that applies to one runtime step.
    Step,
}

/// Compact, validated text for ledger facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactLedgerText(String);

impl CompactLedgerText {
    /// Returns the validated ledger text as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for CompactLedgerText {
    type Error = LedgerValidationError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(LedgerValidationError::EmptyText);
        }

        Ok(Self(trimmed.to_owned()))
    }
}

impl TryFrom<String> for CompactLedgerText {
    type Error = LedgerValidationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_from(value.as_str())
    }
}

impl fmt::Display for CompactLedgerText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Errors raised while constructing ledger updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum LedgerValidationError {
    /// Ledger text was empty or only whitespace.
    #[error("ledger update text must not be empty")]
    EmptyText,
}

/// A typed compact update recorded in the task ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerUpdate {
    sequence: u64,
    order: u64,
    kind: LedgerUpdateKind,
}

impl LedgerUpdate {
    /// Returns the replay sequence assigned to this update.
    #[must_use]
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns append order for deterministic replay when sequences are externally assigned.
    #[must_use]
    pub fn order(&self) -> u64 {
        self.order
    }

    /// Returns the typed update payload.
    #[must_use]
    pub fn kind(&self) -> &LedgerUpdateKind {
        &self.kind
    }
}

/// Compact facts suitable for deterministic context compilation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerUpdateKind {
    /// A compact factual observation from a step, session, or task boundary.
    Observation {
        /// Runtime scope for this observation.
        scope: LedgerScope,
        /// Compact factual text.
        summary: CompactLedgerText,
    },
}

/// Event lifecycle fact recorded before the corresponding event is observable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleFact {
    sequence: u64,
    order: u64,
    kind: LedgerFactKind,
}

impl LifecycleFact {
    /// Returns the runtime event sequence for this lifecycle fact.
    #[must_use]
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns append order for deterministic replay when sequences are externally assigned.
    #[must_use]
    pub fn order(&self) -> u64 {
        self.order
    }

    /// Returns the lifecycle fact kind.
    #[must_use]
    pub fn kind(&self) -> LedgerFactKind {
        self.kind
    }
}

/// Existing event lifecycle fact kinds recorded by the runtime session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerFactKind {
    /// Session start has been recorded.
    SessionStarted,
    /// Artifact state has been recorded.
    ArtifactRecorded,
    /// Step start has been recorded.
    StepStarted,
    /// Step completion has been recorded.
    StepCompleted,
    /// A model-requested tool call has been recorded as pending.
    ToolCallPending,
    /// Step cancellation has been recorded.
    Cancelled,
    /// Step failure has been recorded.
    Failed,
}

/// Owned deterministic view of the ledger for context compilation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerProjectionSnapshot {
    entries: Vec<LedgerProjection>,
}

impl LedgerProjectionSnapshot {
    /// Returns projected entries in deterministic append order.
    #[must_use]
    pub fn entries(&self) -> &[LedgerProjection] {
        &self.entries
    }
}

/// Deterministic ledger projection entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerProjection {
    /// Compact fact projected from a typed ledger update.
    Fact {
        /// Replay sequence.
        sequence: u64,
        /// Append order.
        order: u64,
        /// Fact scope.
        scope: LedgerScope,
        /// Compact fact text.
        text: String,
    },
    /// Runtime lifecycle fact.
    Lifecycle {
        /// Runtime event sequence.
        sequence: u64,
        /// Append order.
        order: u64,
        /// Lifecycle kind.
        kind: LedgerFactKind,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LedgerEntryRef {
    Update(usize),
    Lifecycle(usize),
}
