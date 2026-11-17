use crate::{
    CitationCompactionPolicy, CompactedCheckpoint,
    runtime::{
        Runtime,
        tests::support::common::{RuntimeSessionStateTestExt, session_id},
    },
};

#[tokio::test(flavor = "current_thread")]
async fn prose_bootstrap_does_not_block_compacting_new_history() {
    let runtime = Runtime::builder(session_id("builder-checkpoint-bootstrap-roll"))
        .compacted_checkpoint(
            CompactedCheckpoint::new("manual bootstrap source").expect("valid checkpoint"),
        )
        .build()
        .expect("bootstrap checkpoint should build");
    let mut session = runtime.inner.session.lock().await;
    session
        .record_test_user_message_body("new covered history")
        .expect("covered history records");
    session
        .record_test_user_message_body("new retained history")
        .expect("retained history records");

    let policy = CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy");
    let input = session
        .build_citation_compaction_input(
            policy,
            policy.resolve(64_000).expect("test budget resolves"),
        )
        .expect("rolling input builds")
        .expect("new history is compressible");
    let covered = input.render_covered_segment();
    assert!(covered.contains("new covered history"));
    assert!(!covered.contains("new retained history"));
    assert!(!covered.contains("manual bootstrap source"));
}

#[test]
fn runtime_builder_plain_checkpoint_needs_no_backing_evidence() {
    Runtime::builder(session_id("builder-plain-checkpoint"))
        .compacted_checkpoint(
            CompactedCheckpoint::new("plain checkpoint remains supported")
                .expect("valid plain checkpoint"),
        )
        .build()
        .expect("plain checkpoint should not require citation evidence");
}
