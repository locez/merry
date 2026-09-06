use super::{ids, item, seed};
use crate::memory::{
    MemoryActivationContext, MemoryActivationSource, MemoryError, MemoryScope, MemoryStore,
    StoredMemoryActivationSource,
};
use tokio_util::sync::CancellationToken;

#[test]
fn store_candidate_snapshot_is_deterministic() {
    let mut store = MemoryStore::new();
    store
        .record(item(
            "memory-b",
            MemoryScope::Session,
            &["topic"],
            0.5,
            0,
            None,
        ))
        .expect("memory b records");
    store
        .record(item(
            "memory-a",
            MemoryScope::Session,
            &["topic"],
            0.5,
            0,
            None,
        ))
        .expect("memory a records");

    let snapshot = store.candidate_snapshot();

    assert_eq!(
        snapshot
            .iter()
            .map(|memory| memory.id().as_str())
            .collect::<Vec<_>>(),
        ["memory-a", "memory-b"]
    );
}

#[test]
fn store_rejects_duplicate_memory_id() {
    let mut store = MemoryStore::new();
    store
        .record(item(
            "duplicate",
            MemoryScope::Session,
            &["topic"],
            0.5,
            0,
            None,
        ))
        .expect("first duplicate records");

    let error = store
        .record(item(
            "duplicate",
            MemoryScope::Task,
            &["topic"],
            0.5,
            1,
            None,
        ))
        .expect_err("duplicate memory id is rejected");

    assert!(matches!(
        error,
        MemoryError::DuplicateMemoryId { id } if id.as_str() == "duplicate"
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn stored_source_activates_matching_candidate() {
    let mut store = MemoryStore::new();
    store
        .record(item(
            "stored-topic",
            MemoryScope::Session,
            &["topic"],
            0.5,
            0,
            None,
        ))
        .expect("memory records");
    let source = StoredMemoryActivationSource;

    let activated = source
        .activate(
            seed("topic request", vec![MemoryScope::Session]),
            store.candidate_snapshot(),
            MemoryActivationContext::new(CancellationToken::new()),
        )
        .await
        .expect("activation succeeds");

    assert_eq!(ids(&activated), ["stored-topic"]);
}

#[tokio::test(flavor = "current_thread")]
async fn stored_source_ignores_unmatched_trigger() {
    let mut store = MemoryStore::new();
    store
        .record(item(
            "stored-other",
            MemoryScope::Session,
            &["other"],
            0.5,
            0,
            None,
        ))
        .expect("memory records");
    let source = StoredMemoryActivationSource;

    let activated = source
        .activate(
            seed("topic request", vec![MemoryScope::Session]),
            store.candidate_snapshot(),
            MemoryActivationContext::new(CancellationToken::new()),
        )
        .await
        .expect("activation succeeds");

    assert!(activated.is_empty());
}
