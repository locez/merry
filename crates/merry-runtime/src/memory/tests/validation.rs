use super::{evidence, evidence_ref, item, memory_id, provenance, selection};
use crate::memory::{
    ActivatedMemory, MemoryActivationReason, MemoryActivationScore, MemoryActivationSeed,
    MemoryActivationSourceKind, MemoryError, MemoryEvidence, MemoryId, MemoryItem,
    MemoryItemSelection, MemoryScope,
};

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
