use super::*;
use crate::{
    CompactionError, FileSessionStore, TaskAnchor,
    session::ModelTurnId,
    session_store::{SessionStoreCommitPause, SessionStoreStagePause},
};
use std::time::Duration;

const TRANSACTIONAL_COMPACTION_CANDIDATE: &str = r#"{
  "confirmed_decisions": [],
  "rejected_approaches": [],
  "constraints_preferences_boundaries": [],
  "corrected_misunderstandings": [],
  "durable_conclusions": [{
    "id": "c1",
    "text": "The oldest complete turn was compacted transactionally.",
    "refs": ["h0"]
  }],
  "open_questions": [],
  "current_progress_and_next_steps": [],
  "exact_details": [],
  "handoffs": []
}"#;

fn transactional_compaction_policy() -> CitationCompactionPolicy {
    CitationCompactionPolicy::new(Some(512), Some(16_384), 1).expect("valid policy")
}

async fn transactional_compaction_fixture(
    name: &str,
    durable_store: &FileSessionStore,
    runtime_store: FileSessionStore,
) -> (Runtime, SessionId, Vec<u8>, Vec<u8>) {
    let id = session_id(name);
    let primary = RecordingModelProvider::with_script_and_capabilities(
        Vec::new(),
        ModelCapabilities::new(true, true, false, true, Some(64_000), None)
            .expect("valid primary capabilities"),
    );
    let compactor =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event_with(
                vec![ModelOutput::text(TRANSACTIONAL_COMPACTION_CANDIDATE)],
                FinishReason::Stop,
            ),
        )])]);
    let runtime = Runtime::builder(id.clone())
        .session_store(runtime_store)
        .model_provider(Arc::new(primary), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor),
            named_model("fake/transactional-compactor"),
        )
        .build()
        .expect("runtime builds");
    let before_memory = {
        let mut session = runtime.inner.session.lock().await;
        for text in ["old turn for durable compaction", "retained raw tail"] {
            let turn_id = session.begin_model_turn().expect("turn begins");
            session
                .record_user_message_body(turn_id, text)
                .expect("user message records");
            session
                .close_model_response(turn_id, false)
                .expect("turn completes");
        }
        session
            .persistable_bundle()
            .expect("fixture state is persistable")
            .document_bytes
    };
    let bundle = {
        let session = runtime.inner.session.lock().await;
        session
            .persistable_bundle()
            .expect("fixture state is persistable")
    };
    durable_store
        .write_bundle(bundle)
        .await
        .expect("old state saves");
    let before_disk = durable_store
        .read_state_bytes(&id)
        .await
        .expect("old state reads");
    assert_eq!(before_disk, before_memory);
    (runtime, id, before_memory, before_disk)
}

async fn runtime_persistable_bytes(runtime: &Runtime) -> Vec<u8> {
    runtime
        .inner
        .session
        .lock()
        .await
        .persistable_bundle()
        .expect("runtime state is persistable")
        .document_bytes
}

#[tokio::test(flavor = "current_thread")]
async fn failed_compaction_commit_leaves_memory_and_disk_unchanged() {
    let temp = tempfile::tempdir().expect("tempdir");
    let durable_store = FileSessionStore::new(temp.path());
    let failing_store = durable_store.clone().with_commit_failure_for_tests();
    let (runtime, id, before_memory, before_disk) = transactional_compaction_fixture(
        "runtime-compaction-commit-failure",
        &durable_store,
        failing_store,
    )
    .await;

    let error = runtime
        .compact_context_once(transactional_compaction_policy(), StepContext::default())
        .await
        .expect_err("injected atomic commit failure must fail compaction");

    assert!(matches!(error, RuntimeError::SessionStore { .. }));
    assert_eq!(runtime_persistable_bytes(&runtime).await, before_memory);
    assert_eq!(
        durable_store
            .read_state_bytes(&id)
            .await
            .expect("old disk state remains readable"),
        before_disk
    );
    assert!(runtime.compacted_checkpoint_summary().await.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn directory_sync_failure_keeps_renamed_disk_and_memory_in_step() {
    let temp = tempfile::tempdir().expect("tempdir");
    let durable_store = FileSessionStore::new(temp.path());
    let failing_store = durable_store
        .clone()
        .with_directory_sync_failure_for_tests();
    let (runtime, id, before_memory, before_disk) = transactional_compaction_fixture(
        "runtime-compaction-directory-sync-failure",
        &durable_store,
        failing_store,
    )
    .await;

    let error = runtime
        .compact_context_once(transactional_compaction_policy(), StepContext::default())
        .await
        .expect_err("directory sync failure must be reported");

    assert!(matches!(error, RuntimeError::SessionStore { .. }));
    let after_memory = runtime_persistable_bytes(&runtime).await;
    let after_disk = durable_store
        .read_state_bytes(&id)
        .await
        .expect("renamed state remains readable");
    assert_ne!(after_memory, before_memory);
    assert_ne!(after_disk, before_disk);
    assert_eq!(after_memory, after_disk);
    assert!(runtime.compacted_checkpoint_summary().await.is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_after_staging_keeps_old_checkpoint_and_disk_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let durable_store = FileSessionStore::new(temp.path());
    let pause = SessionStoreStagePause::new();
    let paused_store = durable_store
        .clone()
        .with_stage_pause_for_tests(pause.clone());
    let (runtime, id, before_memory, before_disk) = transactional_compaction_fixture(
        "runtime-compaction-stage-cancel",
        &durable_store,
        paused_store,
    )
    .await;
    let token = CancellationToken::new();
    let operation = runtime.compact_context_once(
        transactional_compaction_policy(),
        StepContext::new(token.clone()),
    );
    tokio::pin!(operation);

    tokio::select! {
        () = pause.wait_until_staged() => {}
        result = &mut operation => panic!("compaction returned before staged pause: {result:?}"),
    }
    assert_eq!(
        durable_store
            .read_state_bytes(&id)
            .await
            .expect("old disk state remains visible while staged"),
        before_disk
    );
    token.cancel();
    pause.resume();
    tokio::time::timeout(Duration::from_secs(1), &mut operation)
        .await
        .expect("cancelled staged compaction returns promptly")
        .expect_err("cancelled staged compaction must fail");

    assert_eq!(runtime_persistable_bytes(&runtime).await, before_memory);
    assert_eq!(
        durable_store
            .read_state_bytes(&id)
            .await
            .expect("old disk state remains after cancellation"),
        before_disk
    );
    assert!(runtime.compacted_checkpoint_summary().await.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn aborting_cancelled_compaction_while_staged_discards_temp_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let durable_store = FileSessionStore::new(temp.path());
    let pause = SessionStoreStagePause::new();
    let paused_store = durable_store
        .clone()
        .with_stage_pause_for_tests(pause.clone());
    let (runtime, id, before_memory, before_disk) = transactional_compaction_fixture(
        "runtime-compaction-stage-abort",
        &durable_store,
        paused_store,
    )
    .await;
    let token = CancellationToken::new();
    let runtime_for_task = runtime.clone();
    let task_token = token.clone();
    let mut task = tokio::spawn(async move {
        runtime_for_task
            .compact_context_once(
                transactional_compaction_policy(),
                StepContext::new(task_token),
            )
            .await
    });

    tokio::select! {
        () = pause.wait_until_staged() => {}
        result = &mut task => panic!("compaction returned before staged pause: {result:?}"),
    }
    token.cancel();
    task.abort();
    assert!(
        task.await
            .expect_err("outer compaction task is aborted")
            .is_cancelled()
    );
    assert!(matches!(
        runtime.acquire_active_step_permit(),
        Err(RuntimeError::StepAlreadyActive { .. })
    ));

    pause.resume();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match runtime.acquire_active_step_permit() {
                Ok(permit) => break drop(permit),
                Err(RuntimeError::StepAlreadyActive { .. }) => tokio::task::yield_now().await,
                Err(error) => panic!("unexpected permit error: {error}"),
            }
        }
    })
    .await
    .expect("detached cancellation cleanup releases the active permit");

    assert_eq!(runtime_persistable_bytes(&runtime).await, before_memory);
    assert_eq!(
        durable_store
            .read_state_bytes(&id)
            .await
            .expect("old disk state remains after staged abort"),
        before_disk
    );
    assert!(durable_store.staged_state_paths_for_tests(&id).is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn stale_window_after_staging_discards_prospective_compaction() {
    let temp = tempfile::tempdir().expect("tempdir");
    let durable_store = FileSessionStore::new(temp.path());
    let pause = SessionStoreStagePause::new();
    let paused_store = durable_store
        .clone()
        .with_stage_pause_for_tests(pause.clone());
    let (runtime, id, _before_memory, before_disk) = transactional_compaction_fixture(
        "runtime-compaction-stage-stale",
        &durable_store,
        paused_store,
    )
    .await;
    let operation =
        runtime.compact_context_once(transactional_compaction_policy(), StepContext::default());
    tokio::pin!(operation);

    tokio::select! {
        () = pause.wait_until_staged() => {}
        result = &mut operation => panic!("compaction returned before staged pause: {result:?}"),
    }
    {
        let mut session = runtime.inner.session.lock().await;
        session.set_task_anchor(TaskAnchor::new("concurrent task change").expect("valid anchor"));
    }
    pause.resume();
    let error = tokio::time::timeout(Duration::from_secs(1), &mut operation)
        .await
        .expect("stale staged compaction returns promptly")
        .expect_err("stale staged compaction must fail");

    assert!(matches!(
        error,
        RuntimeError::Compaction {
            source: CompactionError::StaleWindow,
        }
    ));
    assert!(runtime.compacted_checkpoint_summary().await.is_none());
    assert_eq!(
        durable_store
            .read_state_bytes(&id)
            .await
            .expect("old disk state remains after stale rejection"),
        before_disk
    );
}

#[tokio::test(flavor = "current_thread")]
async fn successful_compaction_commit_resumes_exact_transactional_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let (runtime, id, _before_memory, before_disk) = transactional_compaction_fixture(
        "runtime-compaction-transaction-success",
        &store,
        store.clone(),
    )
    .await;

    let outcome = runtime
        .compact_context_once(transactional_compaction_policy(), StepContext::default())
        .await
        .expect("transactional compaction succeeds")
        .expect("history prefix is compacted");
    let after_memory = runtime_persistable_bytes(&runtime).await;
    let after_disk = store
        .read_state_bytes(&id)
        .await
        .expect("committed state reads");
    assert_ne!(after_disk, before_disk);
    assert_eq!(after_disk, after_memory);

    let resumed = Runtime::builder(id)
        .resume_from_store(store)
        .await
        .expect("transactional compaction state resumes");
    let summary = resumed
        .compacted_checkpoint_summary()
        .await
        .expect("checkpoint resumes");
    assert_eq!(summary.checkpoint_id(), Some(outcome.checkpoint_id()));
    let session = resumed.inner.session.lock().await;
    assert_eq!(
        session
            .full_transcript_snapshot()
            .expect("full transcript resumes")
            .len(),
        2
    );
    assert_eq!(
        session.prompt_history_projection().compacted_through(),
        Some(ModelTurnId::new(1))
    );
    assert_eq!(
        session
            .provider_transcript_snapshot()
            .expect("provider transcript resumes")
            .len(),
        1
    );
}

#[tokio::test(flavor = "current_thread")]
async fn aborting_compaction_after_rename_still_installs_memory_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let durable_store = FileSessionStore::new(temp.path());
    let pause = SessionStoreCommitPause::new();
    let paused_store = durable_store
        .clone()
        .with_commit_pause_for_tests(pause.clone());
    let (runtime, id, before_memory, before_disk) = transactional_compaction_fixture(
        "runtime-compaction-post-rename-abort",
        &durable_store,
        paused_store,
    )
    .await;
    let runtime_for_task = runtime.clone();
    let mut task = tokio::spawn(async move {
        runtime_for_task
            .compact_context_once(transactional_compaction_policy(), StepContext::default())
            .await
    });

    tokio::select! {
        () = pause.wait_until_committed() => {}
        result = &mut task => panic!("compaction returned before post-rename pause: {result:?}"),
    }
    assert_ne!(
        durable_store
            .read_state_bytes(&id)
            .await
            .expect("renamed disk state reads"),
        before_disk
    );

    task.abort();
    assert!(
        task.await
            .expect_err("outer compaction task is aborted")
            .is_cancelled()
    );
    assert!(matches!(
        runtime.acquire_active_step_permit(),
        Err(RuntimeError::StepAlreadyActive { .. })
    ));

    pause.resume();
    let after_memory = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let bytes = runtime_persistable_bytes(&runtime).await;
            if bytes != before_memory {
                break bytes;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("detached durable commit finishes memory install");
    let after_disk = durable_store
        .read_state_bytes(&id)
        .await
        .expect("committed disk state reads");

    assert_eq!(after_memory, after_disk);
    assert!(runtime.compacted_checkpoint_summary().await.is_some());
    assert!(runtime.acquire_active_step_permit().is_ok());
}
