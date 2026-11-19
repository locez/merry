use crate::{
    CitationCompactionPolicy, ContextCompiler, FileSessionStore, ProjectRules,
    SessionTranscriptItem, TaskAnchor,
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
    {
        let mut session = runtime.inner.session.lock().await;
        let turn_id = session.begin_model_turn().expect("turn begins");
        session
            .record_user_message_body(turn_id, "resume trajectory text")
            .expect("user message records");
        session
            .close_model_response(turn_id, false)
            .expect("turn closes");
    }
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

    assert!(
        snapshot.records().iter().any(|record| {
            record
                .summary()
                .is_some_and(|summary| summary.contains("resume trajectory text"))
        }),
        "resume seeds the in-memory trajectory from the transcript"
    );
    assert!(!snapshot.is_closed());
    let seeded_len = snapshot.records().len();
    assert!(seeded_len >= 1);

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
    assert_eq!(appended.records().len(), seeded_len + 1);
    assert!(appended.latest_sequence() > snapshot.latest_sequence());
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

#[tokio::test(flavor = "current_thread")]
async fn save_into_a_fresh_store_rewrites_a_complete_journal() {
    let origin = tempfile::tempdir().expect("tempdir");
    let origin_store = FileSessionStore::new(origin.path());
    let session_id = session_id("runtime-cross-store-journal");
    let runtime = Runtime::builder(session_id.clone())
        .session_store(origin_store.clone())
        .build()
        .expect("runtime builds");
    {
        let mut session = runtime.inner.session.lock().await;
        session.record_session_started_if_needed();
        session
            .record_test_user_message_body("keep this turn")
            .expect("user message records");
    }
    runtime.save_session().await.expect("origin saves");

    let copy = tempfile::tempdir().expect("tempdir");
    let copy_store = FileSessionStore::new(copy.path());
    runtime
        .save_session_to(copy_store.clone())
        .await
        .expect("copy saves");
    let resumed = Runtime::builder(session_id.clone())
        .resume_from_store(copy_store)
        .await
        .expect("copy resumes");
    assert_eq!(
        resumed.session_transcript().await.expect("copy transcript"),
        vec![SessionTranscriptItem::UserMessage {
            text: "keep this turn".to_owned(),
            images: Vec::new(),
        }]
    );

    {
        let mut session = runtime.inner.session.lock().await;
        session
            .record_test_user_message_body("second turn")
            .expect("second user message records");
    }
    runtime
        .save_session()
        .await
        .expect("origin accepts the next delta");
    let second_copy = tempfile::tempdir().expect("tempdir");
    let second_store = FileSessionStore::new(second_copy.path());
    runtime
        .save_session_to(second_store.clone())
        .await
        .expect("second copy saves");
    let resumed = Runtime::builder(session_id)
        .resume_from_store(second_store)
        .await
        .expect("second copy resumes");
    assert_eq!(
        resumed
            .session_transcript()
            .await
            .expect("second copy transcript"),
        vec![
            SessionTranscriptItem::UserMessage {
                text: "keep this turn".to_owned(),
                images: Vec::new(),
            },
            SessionTranscriptItem::UserMessage {
                text: "second turn".to_owned(),
                images: Vec::new(),
            },
        ]
    );
}
