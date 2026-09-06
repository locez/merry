use crate::checkpoint::{
    CheckpointId, CheckpointRef, CheckpointRefId, CheckpointRefManifest, CheckpointSequenceRange,
    CheckpointSourceKind, CheckpointValidationPolicy, CitationBackedCheckpoint,
    CompactedCheckpointCandidate,
};
use crate::{CompactedCheckpoint, ContextError, ProjectRules, TaskAnchor};
use merry_core::{ArtifactId, EvidenceLocator, EvidenceRef};

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
fn task_anchor_validates_objective_and_renders_dynamic_control_text() {
    let anchor =
        TaskAnchor::new("Fix the status text fixture.").expect("valid task anchor objective");

    assert_eq!(anchor.objective(), "Fix the status text fixture.");
    assert_eq!(
        anchor.to_dynamic_control_message_text(),
        "task-anchor:\nFix the status text fixture."
    );
    assert!(matches!(
        TaskAnchor::new("  "),
        Err(ContextError::BlankField {
            field: "task anchor objective"
        })
    ));
    assert!(matches!(
        TaskAnchor::new("bad\u{7}task"),
        Err(ContextError::InvalidControlCharacter {
            field: "task anchor objective"
        })
    ));
}

#[test]
fn compacted_checkpoint_validates_text() {
    let checkpoint =
        CompactedCheckpoint::new("Checkpoint text.").expect("valid compacted checkpoint");

    assert_eq!(checkpoint.text(), "Checkpoint text.");
    assert!(matches!(
        CompactedCheckpoint::new("  "),
        Err(ContextError::BlankField {
            field: "compacted checkpoint text"
        })
    ));
    assert!(matches!(
        CompactedCheckpoint::new("bad\u{7}text"),
        Err(ContextError::InvalidControlCharacter {
            field: "compacted checkpoint text"
        })
    ));
}

#[test]
fn compacted_checkpoint_can_wrap_citation_backed_checkpoint() {
    let manifest = CheckpointRefManifest::new(
        CheckpointId::new("checkpoint-context").expect("valid checkpoint id"),
        vec![CheckpointRef::new(
            CheckpointRefId::new("r1").expect("valid ref id"),
            CheckpointSourceKind::UserMessage,
            CheckpointSequenceRange::new(1, 1).expect("valid range"),
            EvidenceRef::new(
                ArtifactId::new("user-message-1").expect("valid artifact id"),
                EvidenceLocator::whole_artifact(),
            ),
        )],
    )
    .expect("valid manifest");

    let candidate = CompactedCheckpointCandidate::from_json(
        r#"{
          "confirmed_decisions": [{
            "id": "d1",
            "text": "Citation-backed checkpointing is the current direction.",
            "rationale": "It preserves exact source evidence.",
            "refs": ["r1"]
          }],
          "rejected_approaches": [],
          "constraints_preferences_boundaries": [],
          "corrected_misunderstandings": [],
          "durable_conclusions": [],
          "open_questions": [],
          "current_progress_and_next_steps": [],
          "exact_details": [],
          "handoffs": []
        }"#,
    )
    .expect("parseable candidate");
    let citation = CitationBackedCheckpoint::from_candidate(
        CheckpointId::new("checkpoint-context").expect("valid checkpoint id"),
        candidate,
        manifest,
        CheckpointValidationPolicy::default(),
    )
    .expect("valid checkpoint");

    let checkpoint = CompactedCheckpoint::from_citation_backed(citation).expect("valid checkpoint");

    assert!(checkpoint.citation_backed().is_some());
    assert!(checkpoint.text().contains("confirmed_decisions:"));
    assert_eq!(checkpoint.summary().entry_count(), 1);
}
