use crate::{
    CheckpointRefId, CitationCompactionPolicy, CompiledContextSection, ContextCompiler,
    ContextEntry, ContextEvidence, ContextSummary, FileSessionStore, ProjectRules, TaskAnchor,
    artifact::ArtifactContent,
    runtime::{
        Runtime,
        tests::support::common::{RuntimeSessionStateTestExt, session_id},
    },
    session::SessionState,
};
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, EvidenceLocator, PendingToolCall, RuntimeJournalEvent,
    RuntimeJournalPayload, ToolCallArguments, ToolName,
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
    runtime.observe_recorded_journal_events(std::slice::from_ref(&event));
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
    resumed.observe_recorded_journal_events(std::slice::from_ref(&next_event));
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
async fn resumed_runtime_preserves_same_id_manual_context_summary() {
    const OLD: &str = "old managed construction context sentinel";
    const NEW: &str = "new managed construction context sentinel";
    const MANUAL: &str = "manual same-id context sentinel";

    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let session_id = session_id("runtime-resume-preserves-manual-same-id-summary");
    let runtime = Runtime::builder(session_id.clone())
        .initial_context_summary("project-capabilities", OLD)
        .build()
        .expect("runtime builds with old construction context");
    let manual_artifact = ArtifactRef::new(
        ArtifactId::new("manual-project-capabilities-evidence").expect("valid artifact id"),
        ArtifactKind::Text,
    );
    runtime
        .record_artifact(manual_artifact.clone(), ArtifactContent::text(MANUAL))
        .await
        .expect("manual evidence artifact records");
    let manual_evidence = runtime
        .evidence_ref(manual_artifact.id(), EvidenceLocator::whole_artifact())
        .await
        .expect("manual evidence resolves");
    runtime
        .record_context_summary(
            ContextSummary::new(
                "project-capabilities",
                MANUAL,
                vec![
                    ContextEvidence::new("manual project capability evidence", manual_evidence)
                        .expect("manual context evidence builds"),
                ],
            )
            .expect("manual context summary builds"),
        )
        .await
        .expect("manual same-id summary records");
    runtime
        .save_session_to(store.clone())
        .await
        .expect("old session saves");

    let resumed = Runtime::builder(session_id)
        .initial_context_summary("project-capabilities", NEW)
        .resume_from_store(store)
        .await
        .expect("runtime resumes with current construction context");
    let snapshot = ContextCompiler::new()
        .compile(&resumed.context_snapshot().await)
        .expect("resumed context compiles")
        .to_snapshot();

    assert_eq!(snapshot.matches(OLD).count(), 0);
    assert_eq!(snapshot.matches(NEW).count(), 1);
    assert_eq!(snapshot.matches(MANUAL).count(), 1);
    assert_eq!(snapshot.matches("summary:project-capabilities").count(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn resumed_runtime_preserves_manual_summary_backed_by_another_seed_refresh_artifact() {
    const MANUAL: &str = "manual cross-seed text";
    const CURRENT: &str = "current target seed text";

    let refresh_artifact_id = |seed_id: &str| {
        let mut session = SessionState::new(session_id("runtime-cross-seed-refresh-id-source"));
        session
            .reconcile_construction_context_seed(seed_id, &format!("old {seed_id} text"))
            .expect("initial construction seed records");
        session
            .reconcile_construction_context_seed(seed_id, MANUAL)
            .expect("refresh construction seed records");
        let context = ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("refresh construction seed context compiles");
        context
            .sections()
            .iter()
            .find_map(|section| match section {
                CompiledContextSection::Summary { id, text, evidence }
                    if id == seed_id && text == MANUAL =>
                {
                    evidence
                        .first()
                        .map(|item| item.reference().artifact_id.clone())
                }
                _ => None,
            })
            .expect("refresh construction seed evidence exists")
    };
    let other_seed_refresh_artifact_id = refresh_artifact_id("other-seed");
    let target_seed_refresh_artifact_id = refresh_artifact_id("target-seed");
    assert!(
        other_seed_refresh_artifact_id
            .as_str()
            .starts_with("context-seed-refresh-")
    );
    assert_ne!(
        other_seed_refresh_artifact_id,
        target_seed_refresh_artifact_id
    );

    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let target_session_id = session_id("runtime-resume-preserves-cross-seed-refresh-summary");
    let mut target_session = SessionState::new(target_session_id.clone());
    target_session
        .record_artifact_state(
            ArtifactRef::new(other_seed_refresh_artifact_id.clone(), ArtifactKind::Text),
            ArtifactContent::text(MANUAL),
        )
        .expect("cross-seed refresh artifact records");
    let manual_evidence = target_session
        .evidence_ref(
            &other_seed_refresh_artifact_id,
            EvidenceLocator::whole_artifact(),
        )
        .expect("cross-seed refresh evidence resolves");
    target_session
        .record_context_entry(ContextEntry::summary(
            ContextSummary::new(
                "target-seed",
                MANUAL,
                vec![
                    ContextEvidence::new("seeded runtime context", manual_evidence)
                        .expect("manual seeded-label evidence builds"),
                ],
            )
            .expect("manual cross-seed summary builds"),
        ))
        .expect("manual cross-seed summary records");
    target_session
        .save_to(&store)
        .await
        .expect("target session saves");

    let resumed = Runtime::builder(target_session_id)
        .initial_context_summary("target-seed", CURRENT)
        .resume_from_store(store)
        .await
        .expect("target runtime resumes");
    let snapshot = ContextCompiler::new()
        .compile(&resumed.context_snapshot().await)
        .expect("resumed target context compiles")
        .to_snapshot();

    assert_eq!(snapshot.matches(MANUAL).count(), 1);
    assert_eq!(snapshot.matches(CURRENT).count(), 1);
    assert_eq!(snapshot.matches("summary:target-seed").count(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn resumed_runtime_reads_runtime_generated_checkpoint_ref_evidence() {
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
        .install_citation_compaction_candidate(
            input,
            r#"{
              "confirmed_decisions": [],
              "rejected_approaches": [],
              "constraints_preferences_boundaries": [],
              "corrected_misunderstandings": [],
              "durable_conclusions": [{
                "id": "c1",
                "text": "Keep the exact persisted user source.",
                "refs": ["h0"]
              }],
              "open_questions": [],
              "current_progress_and_next_steps": [],
              "exact_details": [],
              "handoffs": []
            }"#,
        )
        .expect("checkpoint installs");
    session.save_to(&store).await.expect("session saves");

    let resumed = Runtime::builder(id)
        .resume_from_store(store)
        .await
        .expect("runtime resumes with checkpoint evidence");
    let page = resumed
        .read_checkpoint_ref_page(&CheckpointRefId::new("h0").expect("valid ref id"), 0, 4096)
        .await
        .expect("runtime-generated checkpoint ref reads after resume");

    assert_eq!(page.artifact_id().as_str(), "user-message-0");
    assert_eq!(page.content(), "exact persisted user source");
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_resume_uses_default_store_constructor_shape() {
    let session_id = session_id("runtime-resume-default-shape");
    let _resume_fn = Runtime::resume;
    let _builder = Runtime::builder(session_id);
}
