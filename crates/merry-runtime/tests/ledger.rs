use merry_runtime::{
    CompactLedgerText, LedgerFactKind, LedgerProjection, LedgerScope, LedgerUpdate,
    LedgerUpdateKind, TaskLedger,
};

fn update(scope: LedgerScope, summary: &str) -> LedgerUpdateKind {
    LedgerUpdateKind::Observation {
        scope,
        summary: summary
            .try_into()
            .expect("test summaries are valid compact ledger text"),
    }
}

#[test]
fn appends_updates_in_sequence_order() {
    let mut ledger = TaskLedger::default();

    let first_sequence = ledger
        .append(update(LedgerScope::Step, "validated prompt input"))
        .sequence();
    let second_sequence = ledger
        .append(update(LedgerScope::Task, "resolved user request"))
        .sequence();

    assert_eq!(first_sequence, 0);
    assert_eq!(second_sequence, 1);

    let updates: Vec<_> = ledger
        .updates()
        .iter()
        .map(LedgerUpdate::sequence)
        .collect();
    assert_eq!(updates, vec![0, 1]);
}

#[test]
fn rejects_empty_compact_update_summary() {
    let err = CompactLedgerText::try_from("").expect_err("empty summary is invalid");

    assert_eq!(err.to_string(), "ledger update text must not be empty");
}

#[test]
fn deterministic_projection_preserves_ordered_compact_facts() {
    let mut ledger = TaskLedger::default();

    ledger.append(update(LedgerScope::Session, "activated relevant memory"));
    ledger.append(update(LedgerScope::Step, "tool output stored as artifact"));
    ledger.record_lifecycle(7, LedgerFactKind::StepCompleted);

    let first = ledger.project();
    let second = ledger.project();

    assert_eq!(first, second);
    assert_eq!(
        first.entries(),
        [
            LedgerProjection::Fact {
                sequence: 0,
                order: 0,
                scope: LedgerScope::Session,
                text: "activated relevant memory".to_owned()
            },
            LedgerProjection::Fact {
                sequence: 1,
                order: 1,
                scope: LedgerScope::Step,
                text: "tool output stored as artifact".to_owned()
            },
            LedgerProjection::Lifecycle {
                sequence: 7,
                order: 2,
                kind: LedgerFactKind::StepCompleted
            },
        ]
    );
}

#[test]
fn records_existing_event_lifecycle_facts() {
    let mut ledger = TaskLedger::default();

    ledger.record_lifecycle(0, LedgerFactKind::SessionStarted);
    ledger.record_lifecycle(1, LedgerFactKind::ArtifactRecorded);
    ledger.record_lifecycle(2, LedgerFactKind::StepStarted);
    ledger.record_lifecycle(3, LedgerFactKind::StepCompleted);
    ledger.record_lifecycle(4, LedgerFactKind::ToolCallPending);

    let lifecycle: Vec<_> = ledger
        .lifecycle_facts()
        .iter()
        .map(|fact| (fact.sequence(), fact.kind()))
        .collect();

    assert_eq!(
        lifecycle,
        [
            (0, LedgerFactKind::SessionStarted),
            (1, LedgerFactKind::ArtifactRecorded),
            (2, LedgerFactKind::StepStarted),
            (3, LedgerFactKind::StepCompleted),
            (4, LedgerFactKind::ToolCallPending),
        ]
    );
}
