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
    let raw_journal =
        tokio::fs::read_to_string(temp.path().join(session_id.as_str()).join("journal.jsonl"))
            .await
            .expect("journal reads");
    assert!(
        !raw_journal.contains(OLD),
        "construction-time context seeds must not be persisted into journal.jsonl"
    );
    assert!(
        !raw_journal.contains("context_entries"),
        "journal should omit empty context_entries when only construction seeds exist"
    );

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

#[tokio::test(flavor = "current_thread")]
async fn save_session_to_without_configured_store_adopts_cursor_and_skips_noop_saves() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let id = session_id("runtime-unconfigured-store-incremental");
    let runtime = Runtime::builder(id.clone())
        .build()
        .expect("runtime builds without configured store");
    {
        let mut session = runtime.inner.session.lock().await;
        session.record_session_started_if_needed();
        session
            .record_test_user_message_body("first turn message")
            .expect("first turn records");
    }

    runtime
        .save_session_to(store.clone())
        .await
        .expect("first save writes header and commit");
    let journal_path = temp.path().join(id.as_str()).join("journal.jsonl");
    let after_first = tokio::fs::read_to_string(&journal_path)
        .await
        .expect("journal reads");
    assert_eq!(after_first.lines().count(), 2, "header + first commit");

    // Repeated no-op save (e.g. save on exit after turn save) must not append a duplicate line.
    runtime
        .save_session_to(store.clone())
        .await
        .expect("no-op save succeeds");
    let after_noop = tokio::fs::read_to_string(&journal_path)
        .await
        .expect("journal reads");
    assert_eq!(
        after_noop, after_first,
        "unchanged save_session_to must not rewrite or append anything"
    );

    // Next turn must append only the new delta without repeating turn 1.
    {
        let mut session = runtime.inner.session.lock().await;
        session
            .record_test_user_message_body("second turn message")
            .expect("second turn records");
    }
    runtime
        .save_session_to(store.clone())
        .await
        .expect("second save appends delta");
    let after_second = tokio::fs::read_to_string(&journal_path)
        .await
        .expect("journal reads");
    let lines = after_second.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 3, "header + 2 incremental commits");
    assert!(
        !lines[2].contains("first turn message"),
        "second commit must not repeat already-flushed turn 1 items"
    );
    assert!(lines[2].contains("second turn message"));

    // Saving to a different copy directory must still produce a complete standalone journal.
    let copy_dir = tempfile::tempdir().expect("copy tempdir");
    let copy_store = FileSessionStore::new(copy_dir.path());
    runtime
        .save_session_to(copy_store.clone())
        .await
        .expect("copy save writes full journal");
    let resumed_copy = Runtime::builder(id)
        .resume_from_store(copy_store)
        .await
        .expect("copy resumes");
    assert_eq!(
        resumed_copy
            .session_transcript()
            .await
            .expect("copy transcript"),
        vec![
            SessionTranscriptItem::UserMessage {
                text: "first turn message".to_owned(),
                images: Vec::new(),
            },
            SessionTranscriptItem::UserMessage {
                text: "second turn message".to_owned(),
                images: Vec::new(),
            },
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_session_lock_enforces_exclusivity_and_close_lifecycle() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let id = session_id("runtime-session-lock");

    let first = Runtime::builder(id.clone())
        .session_store(store.clone())
        .session_lock(true)
        .build()
        .expect("first locked runtime builds");
    assert!(!first.is_closed());

    // A second locked build fails fast with `session_already_reserved`.
    let conflict = Runtime::builder(id.clone())
        .session_store(store.clone())
        .session_lock(true)
        .build()
        .err()
        .expect("second locked runtime must fail");
    assert_eq!(conflict.diagnostic_code(), "session_already_reserved");

    // Closing while an active step holds the permit is rejected and keeps the lock held.
    let permit = first
        .acquire_active_step_permit()
        .expect("permit should be acquired");
    let active_close_error = first
        .close()
        .await
        .expect_err("close must fail while a step permit is active");
    assert_eq!(active_close_error.diagnostic_code(), "step_already_active");
    assert!(!first.is_closed());
    drop(permit);

    // Persisting and closing releases the lock and prevents subsequent mutations.
    first.save_session().await.expect("session saves");
    first.close().await.expect("first runtime closes");
    assert!(first.is_closed());
    // Repeated close is idempotent.
    first.close().await.expect("repeated close is idempotent");

    let closed_error = first
        .save_session()
        .await
        .expect_err("closed runtime rejects save_session");
    assert_eq!(closed_error.diagnostic_code(), "session_closed");

    // After close, resuming with `session_lock(true)` acquires the lock cleanly.
    let resumed = Runtime::builder(id.clone())
        .session_lock(true)
        .resume_from_store(store.clone())
        .await
        .expect("resumed runtime acquires lock after close");

    let resume_conflict = Runtime::builder(id.clone())
        .session_lock(true)
        .resume_from_store(store.clone())
        .await
        .err()
        .expect("concurrent locked resume must fail");
    assert_eq!(
        resume_conflict.diagnostic_code(),
        "session_already_reserved"
    );

    resumed.close().await.expect("resumed runtime closes");
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_session_lock_cleans_up_empty_directory_on_close_and_failed_resume() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());

    // Untouched new session removes its empty directory when closed.
    let untouched_id = session_id("runtime-lock-empty-close");
    let untouched = Runtime::builder(untouched_id.clone())
        .session_store(store.clone())
        .session_lock(true)
        .build()
        .expect("untouched locked runtime builds");
    assert!(store.session_dir(&untouched_id).exists());
    untouched.close().await.expect("untouched runtime closes");
    assert!(
        !store.session_dir(&untouched_id).exists(),
        "closing an untouched session must remove its empty lock directory"
    );

    // Failed resume of a non-existent session must not leave behind an empty lock directory.
    let missing_id = session_id("runtime-lock-missing-resume");
    let missing_error = Runtime::builder(missing_id.clone())
        .session_lock(true)
        .resume_from_store(store.clone())
        .await
        .err()
        .expect("missing session resume must fail");
    assert_eq!(missing_error.diagnostic_code(), "session_store");
    assert!(
        !store.session_dir(&missing_id).exists(),
        "failed resume must not leave behind an empty session directory"
    );
}
