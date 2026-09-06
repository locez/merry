use super::{ids, ids_from_memory_ids, item, seed};
use crate::memory::{
    MemoryActivationReason, MemoryActivationSeed, MemoryActivationSourceKind, MemoryActivator,
    MemoryError, MemoryScope,
};

#[test]
fn activation_output_is_independent_of_candidate_order() {
    let first = item("first", MemoryScope::Session, &["alpha"], 0.8, 3, None);
    let second = item("second", MemoryScope::Session, &["alpha"], 0.9, 3, None);
    let third = item("third", MemoryScope::Task, &["alpha"], 0.4, 7, None);
    let seed = seed(
        "ALPHA request",
        vec![MemoryScope::Session, MemoryScope::Task],
    );

    let ordered = MemoryActivator::activate(&seed, &[first.clone(), second.clone(), third.clone()])
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
