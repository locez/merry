use super::*;

const VALID_CANDIDATE: &str = r#"{
  "confirmed_decisions": [],
  "rejected_approaches": [],
  "constraints_preferences_boundaries": [],
  "corrected_misunderstandings": [],
  "durable_conclusions": [{
    "id": "c1",
    "text": "Old history was compacted.",
    "refs": ["h0"]
  }],
  "open_questions": [],
  "current_progress_and_next_steps": [],
  "exact_details": [],
  "handoffs": []
}"#;

fn compaction_policy() -> CitationCompactionPolicy {
    CitationCompactionPolicy::new(Some(512), Some(16_384), 1).expect("valid policy")
}

fn completed_candidate(candidate: &str) -> ScriptedModelProviderResponse {
    ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
        vec![ModelOutput::text(candidate)],
        FinishReason::Stop,
    ))])
}

fn runtime_with_compactor(
    session_name: &str,
    compactor: RecordingModelProvider,
    primary_window_tokens: u64,
) -> Runtime {
    let primary = RecordingModelProvider::with_script_and_capabilities(
        vec![
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]),
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]),
        ],
        ModelCapabilities::new(true, true, false, true, Some(primary_window_tokens), None)
            .expect("valid primary capabilities"),
    );
    Runtime::builder(session_id(session_name))
        .model_provider(Arc::new(primary), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor),
            ModelName::new("compaction-model").expect("valid model"),
        )
        .build()
        .expect("runtime builds")
}

fn unavailable(message: &str) -> ModelError {
    ModelError::provider(ProviderErrorKind::Unavailable, message)
}

#[derive(Clone)]
struct CancelOnCompletedCompactor {
    calls: Arc<AtomicUsize>,
    capabilities: ModelCapabilities,
}

impl CancelOnCompletedCompactor {
    fn new() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            capabilities: ModelCapabilities::new(true, false, false, false, None, None)
                .expect("valid capabilities"),
        }
    }
}

impl ModelProvider for CancelOnCompletedCompactor {
    fn name(&self) -> &merry_core::ProviderName {
        static NAME: OnceLock<merry_core::ProviderName> = OnceLock::new();
        NAME.get_or_init(|| {
            merry_core::ProviderName::new("cancel-on-completed-compactor")
                .expect("valid provider name")
        })
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn stream_model<'a>(
        &'a self,
        _request: ModelRequest,
        context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let token = context.cancellation_token().clone();
            let event =
                completed_event_with(vec![ModelOutput::text(VALID_CANDIDATE)], FinishReason::Stop);
            let stream = futures_util::stream::once(async move {
                token.cancel();
                Ok(event)
            });
            Ok(Box::pin(stream) as ModelEventStream)
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_json_first_attempt_then_valid_second_attempt_succeeds() {
    let compactor = RecordingModelProvider::with_script(vec![
        completed_candidate("not valid JSON"),
        completed_candidate(VALID_CANDIDATE),
    ]);
    let runtime =
        runtime_with_compactor("compaction-invalid-then-valid", compactor.clone(), 64_000);
    seed_two_history_items_for_compaction(&runtime).await;

    let outcome = runtime
        .compact_context_once(compaction_policy(), StepContext::default())
        .await
        .expect("second candidate should succeed")
        .expect("history should compact");

    assert_eq!(outcome.covered_history_item_count(), 2);
    assert_eq!(compactor.calls.load(Ordering::SeqCst), 2);
    let requests = compactor.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0], requests[1],
        "both attempts must reuse the same precompiled immutable request"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn text_delta_then_stream_error_retries_compactor_attempt() {
    let compactor = RecordingModelProvider::with_script(vec![
        ScriptedModelProviderResponse::Stream(vec![
            Ok(ModelEvent::OutputTextDelta {
                delta: "visible partial candidate".to_owned(),
            }),
            Err(unavailable("stream failed after text delta")),
        ]),
        completed_candidate(VALID_CANDIDATE),
    ]);
    let runtime = runtime_with_compactor("compaction-delta-then-error", compactor.clone(), 64_000);
    seed_two_history_items_for_compaction(&runtime).await;

    runtime
        .compact_context_once(compaction_policy(), StepContext::default())
        .await
        .expect("stream failure after delta should retry")
        .expect("history should compact");

    assert_eq!(compactor.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn compactor_makes_at_most_two_total_provider_attempts() {
    let compactor = RecordingModelProvider::with_script(vec![
        completed_candidate("not valid JSON"),
        ScriptedModelProviderResponse::SetupError(unavailable("second setup failure")),
        ScriptedModelProviderResponse::SetupError(unavailable("third failure must remain")),
    ]);
    let runtime =
        runtime_with_compactor("compaction-two-total-attempts", compactor.clone(), 64_000);
    seed_two_history_items_for_compaction(&runtime).await;

    let error = runtime
        .compact_context_once(compaction_policy(), StepContext::default())
        .await
        .expect_err("two failed attempts should return the second error");

    assert!(matches!(
        error,
        RuntimeError::CompactionModelSetup { ref message }
            if message.contains("second setup failure")
    ));
    assert_eq!(compactor.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        compactor.responses.lock().expect("response mutex").len(),
        1,
        "the third scripted failure must not be consumed"
    );
}

fn repeated_failure(kind: &str) -> ScriptedModelProviderResponse {
    match kind {
        "setup" => ScriptedModelProviderResponse::SetupError(unavailable("setup failure")),
        "stream" => ScriptedModelProviderResponse::Stream(vec![Err(unavailable("stream failure"))]),
        "eof" => ScriptedModelProviderResponse::Stream(vec![Ok(ModelEvent::Started)]),
        "non_stop" => ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
            vec![ModelOutput::text(VALID_CANDIDATE)],
            FinishReason::Length,
        ))]),
        "tool_output" => ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
            vec![ModelOutput::tool_call(model_tool_call(
                "compaction-tool-call",
            ))],
            FinishReason::Stop,
        ))]),
        _ => panic!("unknown failure kind {kind}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn compactor_failure_kinds_share_two_attempt_total_limit() {
    for kind in ["setup", "stream", "eof", "non_stop", "tool_output"] {
        let compactor = RecordingModelProvider::with_script(vec![
            repeated_failure(kind),
            repeated_failure(kind),
            completed_candidate(VALID_CANDIDATE),
        ]);
        let runtime = runtime_with_compactor(
            &format!("compaction-two-attempts-{kind}"),
            compactor.clone(),
            64_000,
        );
        seed_two_history_items_for_compaction(&runtime).await;

        runtime
            .compact_context_once(compaction_policy(), StepContext::default())
            .await
            .expect_err("two repeated failures should exhaust compaction attempts");

        assert_eq!(
            compactor.calls.load(Ordering::SeqCst),
            2,
            "failure kind {kind} must use the shared attempt budget"
        );
        assert_eq!(
            compactor.responses.lock().expect("response mutex").len(),
            1,
            "failure kind {kind} must leave the third response untouched"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_candidate_classes_retry_before_install() {
    let invalid_candidates = [
        (
            "schema",
            r#"{
              "confirmed_decisions": [],
              "rejected_approaches": [],
              "constraints_preferences_boundaries": [],
              "corrected_misunderstandings": [],
              "durable_conclusions": [],
              "open_questions": [],
              "current_progress_and_next_steps": [],
              "handoffs": []
            }"#,
        ),
        (
            "refs",
            r#"{
              "confirmed_decisions": [],
              "rejected_approaches": [],
              "constraints_preferences_boundaries": [],
              "corrected_misunderstandings": [],
              "durable_conclusions": [{"id":"c1","text":"Bad ref.","refs":["missing"]}],
              "open_questions": [],
              "current_progress_and_next_steps": [],
              "exact_details": [],
              "handoffs": []
            }"#,
        ),
        (
            "handoffs",
            r#"{
              "confirmed_decisions": [],
              "rejected_approaches": [],
              "constraints_preferences_boundaries": [],
              "corrected_misunderstandings": [],
              "durable_conclusions": [],
              "open_questions": [],
              "current_progress_and_next_steps": [],
              "exact_details": [],
              "handoffs": [{"action":"keep","old_id":"missing"}]
            }"#,
        ),
    ];

    for (kind, invalid) in invalid_candidates {
        let compactor = RecordingModelProvider::with_script(vec![
            completed_candidate(invalid),
            completed_candidate(VALID_CANDIDATE),
        ]);
        let runtime = runtime_with_compactor(
            &format!("compaction-invalid-{kind}"),
            compactor.clone(),
            64_000,
        );
        seed_two_history_items_for_compaction(&runtime).await;

        runtime
            .compact_context_once(compaction_policy(), StepContext::default())
            .await
            .unwrap_or_else(|error| panic!("{kind} candidate should retry: {error}"))
            .expect("history should compact");

        assert_eq!(compactor.calls.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn oversized_candidate_classes_retry_before_install() {
    let oversized_bytes = format!(
        r#"{{
          "confirmed_decisions": [],
          "rejected_approaches": [],
          "constraints_preferences_boundaries": [],
          "corrected_misunderstandings": [],
          "durable_conclusions": [{{"id":"c1","text":"{}","refs":["h0"]}}],
          "open_questions": [],
          "current_progress_and_next_steps": [],
          "exact_details": [],
          "handoffs": []
        }}"#,
        "large candidate ".repeat(2_000)
    );
    let compactor = RecordingModelProvider::with_script(vec![
        completed_candidate(&oversized_bytes),
        completed_candidate(VALID_CANDIDATE),
    ]);
    let runtime = runtime_with_compactor("compaction-oversized-bytes", compactor.clone(), 64_000);
    seed_two_history_items_for_compaction(&runtime).await;
    let policy = CitationCompactionPolicy::new(Some(512), Some(2_048), 1).expect("valid policy");

    runtime
        .compact_context_once(policy, StepContext::default())
        .await
        .expect("oversized first candidate should retry")
        .expect("history should compact");

    assert_eq!(compactor.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn rendered_checkpoint_too_large_retries_before_install() {
    let rendered_too_large = format!(
        r#"{{
          "confirmed_decisions": [],
          "rejected_approaches": [],
          "constraints_preferences_boundaries": [],
          "corrected_misunderstandings": [],
          "durable_conclusions": [{{"id":"c1","text":"{}","refs":["h0"]}}],
          "open_questions": [],
          "current_progress_and_next_steps": [],
          "exact_details": [],
          "handoffs": []
        }}"#,
        "rendered checkpoint ballast ".repeat(100)
    );
    let compactor = RecordingModelProvider::with_script(vec![
        completed_candidate(&rendered_too_large),
        completed_candidate(VALID_CANDIDATE),
    ]);
    let runtime =
        runtime_with_compactor("compaction-rendered-too-large", compactor.clone(), 64_000);
    seed_two_history_items_for_compaction(&runtime).await;
    let policy = CitationCompactionPolicy::new(Some(100), Some(16_384), 1).expect("valid policy");

    runtime
        .compact_context_once(policy, StepContext::default())
        .await
        .expect("rendered oversized first candidate should retry")
        .expect("history should compact");

    assert_eq!(compactor.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn smaller_compactor_window_is_rejected_before_provider_call() {
    let compactor = RecordingModelProvider::with_script_and_capabilities(
        vec![completed_candidate(VALID_CANDIDATE)],
        ModelCapabilities::new(true, true, false, true, Some(32_000), None)
            .expect("valid capabilities"),
    );
    let runtime = runtime_with_compactor("compaction-smaller-window", compactor.clone(), 64_000);
    seed_two_history_items_for_compaction(&runtime).await;

    let error = runtime
        .compact_context_once(compaction_policy(), StepContext::default())
        .await
        .expect_err("smaller compactor window must be rejected");

    assert!(matches!(
        error,
        RuntimeError::CompactionModelWindowTooSmall {
            primary_window_tokens: 64_000,
            compactor_window_tokens: 32_000,
        }
    ));
    assert_eq!(compactor.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn actual_compactor_payload_too_large_is_rejected_before_provider_call() {
    let compactor = RecordingModelProvider::with_script_and_capabilities(
        vec![completed_candidate(VALID_CANDIDATE)],
        ModelCapabilities::new(true, true, false, true, Some(2_048), None)
            .expect("valid capabilities"),
    );
    let runtime = runtime_with_compactor("compaction-payload-too-large", compactor.clone(), 2_048);
    {
        let mut session = runtime.inner.session.lock().await;
        let covered_turn = session.begin_model_turn().expect("covered turn begins");
        session
            .record_user_message_body(
                covered_turn,
                &format!("large covered payload {}", "payload ballast ".repeat(1_000)),
            )
            .expect("covered input records");
        session
            .record_assistant_text_output(covered_turn, "covered response".to_owned())
            .expect("covered response records");
        session
            .close_model_response(covered_turn, false)
            .expect("covered turn completes");
        let retained_turn = session.begin_model_turn().expect("retained turn begins");
        session
            .record_user_message_body(retained_turn, "retained tail")
            .expect("retained input records");
        session
            .record_assistant_text_output(retained_turn, "retained response".to_owned())
            .expect("retained response records");
        session
            .close_model_response(retained_turn, false)
            .expect("retained turn completes");
    }

    let error = runtime
        .compact_context_once(compaction_policy(), StepContext::default())
        .await
        .expect_err("oversized compactor request must be rejected");

    assert!(matches!(
        error,
        RuntimeError::CompactionModelInputTooLarge {
            estimated_input_tokens,
            compactor_window_tokens: 2_048,
        } if estimated_input_tokens > 2_048
    ));
    assert_eq!(compactor.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn missing_compactor_window_metadata_uses_primary_window() {
    let compactor = RecordingModelProvider::with_script_and_capabilities(
        vec![completed_candidate(VALID_CANDIDATE)],
        ModelCapabilities::new(true, true, false, true, None, None).expect("valid capabilities"),
    );
    let runtime = runtime_with_compactor("compaction-missing-window", compactor.clone(), 64_000);
    seed_two_history_items_for_compaction(&runtime).await;

    let (result, logs) = capture_traces_for(
        "compaction-missing-window",
        runtime.compact_context_once(compaction_policy(), StepContext::default()),
    )
    .await;
    result
        .expect("missing metadata should assume primary window")
        .expect("history should compact");

    assert_eq!(compactor.calls.load(Ordering::SeqCst), 1);
    assert!(logs.contains("\"event\":\"runtime.compaction.model_window_assumed\""));
    assert!(logs.contains("\"primary_window_tokens\":64000"));
}

#[tokio::test(flavor = "current_thread")]
async fn pre_cancelled_token_does_not_call_compactor() {
    let compactor = RecordingModelProvider::with_script(vec![completed_candidate(VALID_CANDIDATE)]);
    let runtime = runtime_with_compactor("compaction-pre-cancelled", compactor.clone(), 64_000);
    seed_two_history_items_for_compaction(&runtime).await;
    let token = CancellationToken::new();
    token.cancel();

    runtime
        .compact_context_once(compaction_policy(), StepContext::new(token))
        .await
        .expect_err("pre-cancelled compaction should return immediately");

    assert_eq!(compactor.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_after_completed_candidate_prevents_install() {
    let primary = RecordingModelProvider::with_script_and_capabilities(
        vec![
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]),
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]),
        ],
        ModelCapabilities::new(true, true, false, true, Some(64_000), None)
            .expect("valid primary capabilities"),
    );
    let compactor = CancelOnCompletedCompactor::new();
    let runtime = Runtime::builder(session_id("compaction-cancel-before-install"))
        .model_provider(Arc::new(primary), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            ModelName::new("compaction-model").expect("valid model"),
        )
        .build()
        .expect("runtime builds");
    seed_two_history_items_for_compaction(&runtime).await;

    runtime
        .compact_context_once(compaction_policy(), StepContext::default())
        .await
        .expect_err("cancellation observed with the completed candidate must prevent install");

    assert_eq!(compactor.calls.load(Ordering::SeqCst), 1);
    assert!(
        runtime.compacted_checkpoint_summary().await.is_none(),
        "cancellation before install must leave checkpoint state unchanged"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_does_not_retry_compactor() {
    let (started_sender, started_receiver) = oneshot::channel();
    let (dropped_sender, dropped_receiver) = oneshot::channel();
    let compactor = RecordingModelProvider::with_script(vec![
        ScriptedModelProviderResponse::PendingSetupWithDrop {
            started: started_sender,
            dropped: dropped_sender,
        },
        completed_candidate(VALID_CANDIDATE),
    ]);
    let runtime = runtime_with_compactor("compaction-cancel-setup", compactor.clone(), 64_000);
    seed_two_history_items_for_compaction(&runtime).await;
    let token = CancellationToken::new();
    let operation =
        runtime.compact_context_once(compaction_policy(), StepContext::new(token.clone()));
    tokio::pin!(operation);

    tokio::select! {
        result = &mut operation => panic!("setup completed before cancellation: {result:?}"),
        result = started_receiver => result.expect("setup should start"),
    }
    token.cancel();
    let error = tokio::time::timeout(Duration::from_secs(1), &mut operation)
        .await
        .expect("cancelled compaction should return promptly")
        .expect_err("cancelled compaction should fail");
    tokio::time::timeout(Duration::from_secs(1), dropped_receiver)
        .await
        .expect("setup future should be dropped")
        .expect("drop notification should arrive");

    assert!(error.to_string().contains("cancel"));
    assert_eq!(compactor.calls.load(Ordering::SeqCst), 1);
    assert_eq!(compactor.responses.lock().expect("response mutex").len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_failure_classes_do_not_retry_compactor() {
    for (kind, cancelled) in [
        (
            "setup",
            ScriptedModelProviderResponse::SetupError(ModelError::Cancelled),
        ),
        (
            "stream",
            ScriptedModelProviderResponse::Stream(vec![Err(ModelError::Cancelled)]),
        ),
        (
            "finish",
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
                Vec::new(),
                FinishReason::Cancelled,
            ))]),
        ),
    ] {
        let compactor = RecordingModelProvider::with_script(vec![
            cancelled,
            completed_candidate(VALID_CANDIDATE),
        ]);
        let runtime = runtime_with_compactor(
            &format!("compaction-cancelled-{kind}"),
            compactor.clone(),
            64_000,
        );
        seed_two_history_items_for_compaction(&runtime).await;

        runtime
            .compact_context_once(compaction_policy(), StepContext::default())
            .await
            .expect_err("cancelled compactor response should fail immediately");

        assert_eq!(
            compactor.calls.load(Ordering::SeqCst),
            1,
            "cancelled {kind} must not retry"
        );
        assert_eq!(compactor.responses.lock().expect("response mutex").len(), 1);
    }
}
