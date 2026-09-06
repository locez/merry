use crate::{
    FileSessionStore,
    runtime::{
        Runtime,
        tests::support::common::{RuntimeSessionStateTestExt, session_id},
    },
};
use merry_core::{PendingToolCall, ToolCallArguments, ToolCallId, ToolName};

#[tokio::test(flavor = "current_thread")]
async fn abandoning_pending_tool_calls_makes_a_stalled_session_saveable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let session_id = session_id("runtime-abandon-pending");
    let runtime = Runtime::builder(session_id.clone())
        .session_store(store.clone())
        .build()
        .expect("runtime builds");
    let call = PendingToolCall::new(
        ToolCallId::new("call-abandoned").expect("valid tool call id"),
        ToolName::new("stalled_tool").expect("valid tool name"),
        ToolCallArguments::new(Default::default()),
    );
    let call_id = call.id().clone();
    let pending_event = runtime
        .inner
        .session
        .lock()
        .await
        .record_test_tool_call_pending(call)
        .expect("pending call records");
    runtime.observe_recorded_journal_events(std::slice::from_ref(&pending_event));
    let trajectory_before_abandonment = runtime
        .trajectory_snapshot()
        .await
        .expect("trajectory snapshot reads");

    runtime
        .save_session_to(store.clone())
        .await
        .expect_err("a session holding a pending tool call must not be saved");

    assert_eq!(
        runtime
            .abandon_pending_tool_calls("the run settled before this call resolved")
            .await
            .expect("pending calls resolve"),
        1
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
    let trajectory_after_abandonment = runtime
        .trajectory_snapshot()
        .await
        .expect("trajectory snapshot reads");
    assert!(
        trajectory_after_abandonment.revision() > trajectory_before_abandonment.revision(),
        "abandonment must advance the live trajectory projection"
    );
    let abandoned_record = trajectory_after_abandonment
        .records()
        .iter()
        .find(|record| record.tool_call_id() == Some(&call_id))
        .expect("abandoned tool remains visible in the trajectory");
    assert_eq!(
        abandoned_record.status(),
        merry_core::TrajectoryRecordStatus::Failed
    );
    assert_eq!(
        abandoned_record
            .diagnostic()
            .map(|diagnostic| diagnostic.code()),
        Some("tool_abandoned_by_run_settlement")
    );
    drop(runtime);

    let resumed = Runtime::builder(session_id)
        .resume_from_store(store)
        .await
        .expect("the automatic savepoint resumes");
    assert!(resumed.pending_tool_calls().await.is_empty());
    let resumed_trajectory = resumed
        .trajectory_snapshot()
        .await
        .expect("resumed trajectory snapshot reads");
    let resumed_record = resumed_trajectory
        .records()
        .iter()
        .find(|record| record.tool_call_id() == Some(&call_id))
        .expect("the abandoned tool remains visible after resume");
    assert_eq!(
        resumed_record.status(),
        merry_core::TrajectoryRecordStatus::Failed
    );
    assert_eq!(
        resumed_record
            .diagnostic()
            .map(|diagnostic| diagnostic.code()),
        Some("tool_abandoned_by_run_settlement")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn abandoning_pending_tool_calls_is_a_no_op_without_any() {
    let runtime = Runtime::builder(session_id("runtime-abandon-none"))
        .build()
        .expect("runtime builds");

    assert_eq!(
        runtime
            .abandon_pending_tool_calls("nothing to abandon")
            .await
            .expect("an idle runtime resolves nothing"),
        0
    );
}
