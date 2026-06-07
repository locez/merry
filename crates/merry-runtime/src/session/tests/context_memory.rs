use super::*;

#[test]
fn context_snapshot_is_independent_from_later_recorded_memory() {
    let mut session = SessionState::new(session_id());

    let stale = session.context_snapshot();
    let memory = activated_memory("memory-later");
    session.record_activated_memory(memory.clone());
    session
        .record_artifact_state(
            ArtifactRef::new(artifact_id("memory-later-artifact"), ArtifactKind::Text),
            ArtifactContent::text("first exact memory evidence\n"),
        )
        .expect("memory artifact records");
    let current = session.context_snapshot();
    let second_memory = activated_memory_with_details("memory-later-2", "later text", 1, 2, 0.5);
    session.record_activated_memory(second_memory);
    session
        .record_artifact_state(
            ArtifactRef::new(artifact_id("memory-later-2-artifact"), ArtifactKind::Text),
            ArtifactContent::text("second exact memory evidence\n"),
        )
        .expect("later memory artifact records");

    let stale = ContextCompiler::new()
        .compile(&stale)
        .expect("stale snapshot compiles");
    let current = ContextCompiler::new()
        .compile(&current)
        .expect("current snapshot compiles");

    assert_eq!(stale.to_snapshot(), "");
    assert!(current.to_snapshot().contains("memory:memory-later"));
    assert!(
        current
            .to_snapshot()
            .contains("memory-evidence:primary source:memory-later-artifact:whole")
    );
    assert!(
        current
            .to_snapshot()
            .contains("memory-activation-source-label:user request")
    );
    assert!(!current.to_snapshot().contains("memory-later-2"));
}

#[test]
fn record_activated_memories_appends_to_memory_projection() {
    let mut session = SessionState::new(session_id());
    let memory_a = activated_memory("memory-a");
    let memory_b = activated_memory("memory-b");
    record_memory_artifacts(&mut session, &[&memory_a, &memory_b]);
    session.record_activated_memories(vec![memory_a, memory_b]);

    let compiled = ContextCompiler::new()
        .compile(&session.context_snapshot())
        .expect("snapshot compiles");

    assert!(compiled.to_snapshot().contains("memory:memory-a"));
    assert!(compiled.to_snapshot().contains("memory:memory-b"));
}

#[test]
fn replace_activated_memories_updates_current_memory_projection() {
    let mut session = SessionState::new(session_id());
    let stale = activated_memory("memory-stale");
    let current = activated_memory("memory-current");
    record_memory_artifacts(&mut session, &[&stale, &current]);

    session.record_activated_memory(stale);
    session.replace_activated_memories(vec![current]);

    let compiled = ContextCompiler::new()
        .compile(&session.context_snapshot())
        .expect("snapshot compiles");

    assert!(!compiled.to_snapshot().contains("memory:memory-stale"));
    assert!(compiled.to_snapshot().contains("memory:memory-current"));
}

#[test]
fn replace_activated_memories_with_empty_clears_current_memory_projection() {
    let mut session = SessionState::new(session_id());
    let memory = activated_memory("memory-cleared");
    record_memory_artifacts(&mut session, &[&memory]);

    session.record_activated_memory(memory);
    session.replace_activated_memories(Vec::new());

    let compiled = ContextCompiler::new()
        .compile(&session.context_snapshot())
        .expect("snapshot compiles");

    assert_eq!(compiled.to_snapshot(), "");
}

#[test]
fn duplicate_recorded_activated_memories_compile_once_deterministically() {
    let lower_duplicate =
        activated_memory_with_details("memory-duplicate", "Lower duplicate.", 1, 0, 0.5);
    let higher_duplicate =
        activated_memory_with_details("memory-duplicate", "Higher duplicate.", 2, 0, 0.5);

    let mut first = SessionState::new(session_id());
    record_memory_artifacts(&mut first, &[&lower_duplicate, &higher_duplicate]);
    first.record_activated_memories(vec![lower_duplicate.clone(), higher_duplicate.clone()]);
    let first = ContextCompiler::new()
        .compile(&first.context_snapshot())
        .expect("first snapshot compiles")
        .to_snapshot();

    let mut second = SessionState::new(session_id());
    record_memory_artifacts(&mut second, &[&higher_duplicate, &lower_duplicate]);
    second.record_activated_memories(vec![higher_duplicate, lower_duplicate]);
    let second = ContextCompiler::new()
        .compile(&second.context_snapshot())
        .expect("second snapshot compiles")
        .to_snapshot();

    assert_eq!(first, second);
    assert_eq!(first.matches("memory:memory-duplicate").count(), 1);
    assert!(first.contains("memory-text:Higher duplicate."));
    assert!(!first.contains("memory-text:Lower duplicate."));
}
