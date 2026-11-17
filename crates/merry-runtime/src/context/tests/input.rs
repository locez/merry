use crate::{CompactedCheckpoint, ContextError, ProjectRules, TaskAnchor};

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
