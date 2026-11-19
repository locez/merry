use crate::{
    CitationCompactionPolicy, ContextCompiler, FileSessionStore, ProjectRules, TaskAnchor,
    runtime::{
        Runtime,
        tests::support::common::{RuntimeSessionStateTestExt, session_id},
    },
    session::SessionState,
};
use merry_core::{
    PendingToolCall, RuntimeJournalEvent, RuntimeJournalPayload, ToolCallArguments, ToolName,
};
use serde_json::json;

#[tokio::test(flavor = "current_thread")]
async fn builder_resumes_session_from_store_and_reinjects_construction_context() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let session_id = session_id("runtime-resume");
    let runtime = Runtime::builder(session_id.clone())
        .session_store(store.clone())
        .build()
        .expect("runtime builds");

    runtime.save_session().await.expect("session saves");

    let resumed = Runtime::builder(session_id.clone())
        .project_rules(ProjectRules::new("AGENTS.md", "Runtime rules").expect("project rules"))
        .task_anchor(TaskAnchor::new("continue from restored state").expect("task anchor"))
        .resume_from_store(store)
        .await
        .expect("runtime resumes");

    assert_eq!(resumed.session_id(), &session_id);
    assert!(resumed.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn resumed_runtime_reopens_persisted_trajectory_for_append() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let session_id = session_id("runtime-resume-trajectory");
    let runtime = Runtime::builder(session_id.clone())
        .session_store(store.clone())
        .build()
        .expect("runtime builds");
    let call = PendingToolCall::new(
        merry_core::ToolCallId::new("trajectory-call").expect("valid call id"),
        ToolName::new("lookup").expect("valid tool name"),
        ToolCallArguments::try_from(json!({"query": "value"})).expect("valid arguments"),
    );
    let event = RuntimeJournalEvent::new(
        session_id.clone(),
        7,
        RuntimeJournalPayload::ToolCallPending { call },
    );
    runtime
        .inner
        .project_journal_events(std::slice::from_ref(&event));
    runtime.close_trajectory();
    runtime
        .save_session()
        .await
        .expect("trajectory session saves");

    let resumed = Runtime::builder(session_id.clone())
        .resume_from_store(store)
        .await
        .expect("runtime resumes");
    let snapshot = resumed
        .trajectory_snapshot()
        .await
        .expect("trajectory snapshot reads");

    assert_eq!(snapshot.latest_sequence(), 7);
    assert!(!snapshot.is_closed());
    assert_eq!(snapshot.records().len(), 1);
    assert_eq!(snapshot.records()[0].start_sequence(), 7);

    let next_call = PendingToolCall::new(
        merry_core::ToolCallId::new("trajectory-call-after-resume").expect("valid call id"),
        ToolName::new("lookup").expect("valid tool name"),
        ToolCallArguments::try_from(json!({"query": "next"})).expect("valid arguments"),
    );
    let next_event = RuntimeJournalEvent::new(
        session_id,
        8,
        RuntimeJournalPayload::ToolCallPending { call: next_call },
    );
    resumed
        .inner
        .project_journal_events(std::slice::from_ref(&next_event));
    let appended = resumed
        .trajectory_snapshot()
        .await
        .expect("appended trajectory reads");
    assert_eq!(appended.records().len(), 2);
    assert_eq!(appended.latest_sequence(), 8);
}

#[tokio::test(flavor = "current_thread")]
async fn resumed_runtime_replaces_changed_construction_context_seed() {
    const OLD: &str = "old construction context sentinel";
    const NEW: &str = "new construction context sentinel";

    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let session_id = session_id("runtime-resume-refreshes-context-seed");
    let runtime = Runtime::builder(session_id.clone())
        .initial_context_summary("project-capabilities", OLD)
        .build()
        .expect("runtime builds with old construction context");
    runtime
        .save_session_to(store.clone())
        .await
        .expect("old session saves");

    let resumed = Runtime::builder(session_id.clone())
        .initial_context_summary("project-capabilities", NEW)
        .resume_from_store(store.clone())
        .await
        .expect("runtime resumes with current construction context");
    let snapshot = ContextCompiler::new()
        .compile(&resumed.context_snapshot().await)
        .expect("resumed context compiles")
        .to_snapshot();

    assert_eq!(snapshot.matches(OLD).count(), 0);
    assert_eq!(snapshot.matches(NEW).count(), 1);
    assert_eq!(snapshot.matches("summary:project-capabilities").count(), 1);

    resumed
        .save_session()
        .await
        .expect("refreshed session saves");
    let resumed_again = Runtime::builder(session_id)
        .initial_context_summary("project-capabilities", NEW)
        .resume_from_store(store)
        .await
        .expect("runtime resumes idempotently with unchanged construction context");
    let snapshot = ContextCompiler::new()
        .compile(&resumed_again.context_snapshot().await)
        .expect("idempotently resumed context compiles")
        .to_snapshot();

    assert_eq!(snapshot.matches(OLD).count(), 0);
    assert_eq!(snapshot.matches(NEW).count(), 1);
    assert_eq!(snapshot.matches("summary:project-capabilities").count(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn resumed_runtime_keeps_prose_checkpoint_summary() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let id = session_id("runtime-resume-checkpoint-ref");
    let mut session = SessionState::new(id.clone());
    session
        .record_test_user_message_body("exact persisted user source")
        .expect("covered user history records");
    session
        .record_test_user_message_body("retained user history")
        .expect("retained user history records");
    let policy = CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy");
    let input = session
        .build_citation_compaction_input(
            policy,
            policy.resolve(64_000).expect("test budget resolves"),
        )
        .expect("compaction input builds")
        .expect("covered user history is compressible");
    session
        .install_summary_compaction(input, "The covered turn was compacted.")
        .expect("checkpoint installs");
    session.save_to(&store).await.expect("session saves");

    let resumed = Runtime::builder(id)
        .resume_from_store(store)
        .await
        .expect("runtime resumes with the prose checkpoint");
    let snapshot = ContextCompiler::new()
        .compile(&resumed.context_snapshot().await)
        .expect("resumed context compiles")
        .to_snapshot();

    assert!(snapshot.contains("The covered turn was compacted."));
    assert!(!snapshot.contains("exact persisted user source"));
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_resume_uses_default_store_constructor_shape() {
    let session_id = session_id("runtime-resume-default-shape");
    let _resume_fn = Runtime::resume;
    let _builder = Runtime::builder(session_id);
}
