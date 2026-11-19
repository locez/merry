use super::{
    activated_memory, activated_memory_with_evidence, labeled_provenance, memory_evidence,
    memory_id, memory_selection, memory_snapshot, provenance, ranked_reasons, score,
};
use crate::{
    CompactedCheckpoint, ContextCompiler, ContextEntry, ContextError, ContextSummary,
    SessionContextSnapshot,
    memory::{
        ActivatedMemory, MemoryActivationProvenance, MemoryActivationReason,
        MemoryActivationSourceKind, MemoryItem, MemoryScope,
    },
};

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
            "memory-evidence:primary source:memory-main-source",
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
fn compacted_checkpoint_renders_before_summaries_and_memory() {
    let checkpoint = CompactedCheckpoint::new("Checkpoint context.").expect("valid");
    let memory = activated_memory(
        "memory-main",
        MemoryScope::Task,
        "Memory projection.",
        score(1, 0, 0.5),
        ranked_reasons(1, 0, 0.5),
    );
    let summary = ContextEntry::summary(
        ContextSummary::new("summary-a", "Summary projection.").expect("summary fields are valid"),
    );
    let snapshot = SessionContextSnapshot::new(vec![summary], vec![memory], Some(checkpoint));

    let compiled = ContextCompiler::new()
        .compile(&snapshot)
        .expect("context compiles")
        .to_snapshot();

    assert!(
        compiled.starts_with(
            "compacted-checkpoint:\nguidance:This summary is the record of the turns that were removed."
        ),
        "compacted checkpoint should render before summary and memory sections"
    );
    assert!(
        compiled.contains("\ntext:Checkpoint context.\nsummary:summary-a"),
        "checkpoint text should still render before summary sections"
    );
    assert!(compiled.contains("\nmemory:memory-main"));
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
        vec![memory_evidence("source", "source-z")],
        score(1, 1, 0.5),
        ranked_reasons(1, 1, 0.5),
        provenance(),
    );
    let a_evidence = activated_memory_with_evidence(
        "memory-duplicate",
        MemoryScope::Session,
        "Same text.",
        vec![memory_evidence("source", "source-a")],
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
    assert!(first.contains("memory-evidence:source:source-a"));
    assert!(!first.contains("memory-evidence:source:source-z"));

    let z_provenance = activated_memory_with_evidence(
        "memory-duplicate",
        MemoryScope::Session,
        "Same text.",
        vec![memory_evidence("source", "source-a")],
        score(1, 1, 0.5),
        ranked_reasons(1, 1, 0.5),
        labeled_provenance("Z source"),
    );
    let a_provenance = activated_memory_with_evidence(
        "memory-duplicate",
        MemoryScope::Session,
        "Same text.",
        vec![memory_evidence("source", "source-a")],
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
    let snapshot = SessionContextSnapshot::new(Vec::new(), vec![memory], None);

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
fn valid_memory_evidence_appears_in_snapshot_deterministically() {
    let memory = activated_memory_with_evidence(
        "memory-evidence",
        MemoryScope::Session,
        "Memory with sorted evidence.",
        vec![
            memory_evidence("z label", "source-b"),
            memory_evidence("a label", "source-a"),
            memory_evidence("b label", "source-a"),
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

    assert!(compiled.contains(
        "memory-evidence:a label:source-a\nmemory-evidence:b label:source-a\nmemory-evidence:z label:source-b"
    ));
}

#[test]
fn evidence_and_provenance_ordering_is_independent_of_insertion_order() {
    let first = activated_memory_with_evidence(
        "memory-ordered",
        MemoryScope::Session,
        "Memory with shuffled evidence.",
        vec![
            memory_evidence("z label", "source-z"),
            memory_evidence("a label", "source-a"),
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
            memory_evidence("a label", "source-a"),
            memory_evidence("z label", "source-z"),
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
        "memory-activation-allowed-scopes:session,step\nmemory-evidence:a label:source-a\nmemory-evidence:z label:source-z"
    ));
}

#[test]
fn summaries_and_memory_compile_together() {
    let summary = ContextEntry::summary(
        ContextSummary::new("summary-a", "Navigation.").expect("summary fields are valid"),
    );
    let memory = activated_memory(
        "memory-a",
        MemoryScope::Session,
        "Internal memory.",
        score(1, 0, 0.5),
        ranked_reasons(1, 0, 0.5),
    );
    let snapshot = SessionContextSnapshot::new(vec![summary], vec![memory], None);

    let compiled = ContextCompiler::new()
        .compile(&snapshot)
        .expect("summary and memory compile");

    assert_eq!(compiled.sections().len(), 1);
    assert_eq!(
        compiled.to_snapshot(),
        [
            "summary:summary-a",
            "text:Navigation.",
            "memory:memory-a",
            "memory-scope:session",
            "memory-text:Internal memory.",
            "memory-activation-source-kind:user_query",
            "memory-activation-source-label:user request",
            "memory-activation-query:topic",
            "memory-activation-allowed-scopes:session,task,step",
            "memory-evidence:primary source:memory-a-source",
            "memory-reason:scope_allowed",
            "memory-reason:trigger:topic",
            "memory-reason:rank:matches=1;priority=0;confidence=0.500",
        ]
        .join("\n")
    );
}
