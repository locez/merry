//! Internal task ledger skeleton.

/// Compact record of state transitions that happened before event emission.
#[derive(Debug, Default)]
pub(crate) struct TaskLedger {
    facts: Vec<LedgerFact>,
}

impl TaskLedger {
    pub(crate) fn record(&mut self, sequence: u64, kind: LedgerFactKind) {
        self.facts.push(LedgerFact { sequence, kind });
        debug_assert!(self.last_recorded(sequence, kind));
    }

    fn last_recorded(&self, sequence: u64, kind: LedgerFactKind) -> bool {
        self.facts
            .last()
            .is_some_and(|fact| fact.sequence == sequence && fact.kind == kind)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LedgerFactKind {
    SessionStarted,
    StepStarted,
    StepCompleted,
    Cancelled,
}

#[derive(Debug)]
struct LedgerFact {
    sequence: u64,
    kind: LedgerFactKind,
}
