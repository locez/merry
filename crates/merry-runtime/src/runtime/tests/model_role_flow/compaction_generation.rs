use crate::{
    CitationCompactionPolicy, CompactionConfig, RuntimeError, RuntimeModelRole, StepContext,
    runtime::{
        Runtime,
        tests::{
            model_role_flow::seed_two_history_items_for_compaction,
            support::{
                common::{
                    capture_traces_for, collect_step, completed_event, completed_event_with,
                    model_name, model_tool_call, session_id,
                },
                model_provider::{RecordingModelProvider, ScriptedModelProviderResponse},
            },
        },
    },
};
use merry_core::RuntimeJournalPayload;
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelName,
    ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelStreamContext,
    ProviderErrorKind,
};
use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

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
    runtime_with_compactor_and_steps(session_name, compactor, primary_window_tokens, 2)
}

fn runtime_with_compactor_and_steps(
    session_name: &str,
    compactor: RecordingModelProvider,
    primary_window_tokens: u64,
    primary_steps: usize,
) -> Runtime {
    let primary = RecordingModelProvider::with_script_and_capabilities(
        (0..primary_steps)
            .map(|_| ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]))
            .collect(),
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
        // These tests exercise the manual compaction path, so seeding must not
        // spend the scripted compactor responses on automatic reductions.
        .automatic_compaction(CompactionConfig::disabled())
        .build()
        .expect("runtime builds")
}

async fn seed_rolling_history(runtime: &Runtime) {
    for index in 0..6 {
        let events = collect_step(
            runtime,
            &format!("covered turn {index} {}", "payload ballast ".repeat(1_200)),
            StepContext::default(),
        )
        .await;
        assert!(
            events
                .iter()
                .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted))
        );
    }
}

fn unavailable(message: &str) -> ModelError {
    ModelError::provider(ProviderErrorKind::Unavailable, message)
}

fn invalid_request(message: &str) -> ModelError {
    ModelError::provider(ProviderErrorKind::InvalidRequest, message)
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
    assert!(requests[1].input().starts_with(requests[0].input()));
    assert_eq!(requests[0].tools(), requests[1].tools());
    assert_eq!(requests[0].generation(), requests[1].generation());
    assert_eq!(
        requests[0].stable_prefix_hash(),
        requests[1].stable_prefix_hash()
    );
    assert!(
        requests[1]
            .messages()
            .last()
            .expect("repair message")
            .content()
            .as_text()
            .contains("COMPACTION REPAIR")
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

#[tokio::test(flavor = "current_thread")]
async fn invalid_compactor_request_is_not_retried() {
    let compactor = RecordingModelProvider::with_script(vec![
        ScriptedModelProviderResponse::SetupError(invalid_request(
            "HTTP 400 invalid_json_schema: missing rationale",
        )),
        completed_candidate(VALID_CANDIDATE),
    ]);
    let runtime = runtime_with_compactor("compaction-invalid-request", compactor.clone(), 64_000);
    seed_two_history_items_for_compaction(&runtime).await;

    let error = runtime
        .compact_context_once(compaction_policy(), StepContext::default())
        .await
        .expect_err("invalid provider request should fail immediately");

    assert!(error.to_string().contains("HTTP 400"));
    assert_eq!(compactor.calls.load(Ordering::SeqCst), 1);
    assert_eq!(compactor.responses.lock().expect("response mutex").len(), 1);
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
    for kind in ["setup", "stream", "eof", "tool_output"] {
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

/// A truncated candidate is not retried with the identical request.
///
/// The truncation proves the reasoning reserve was too small, so the retry asks
/// for a larger output ceiling instead of repeating the same budget.
#[tokio::test(flavor = "current_thread")]
async fn truncated_compaction_is_not_retried_with_the_same_output_budget() {
    let compactor = RecordingModelProvider::with_script(vec![
        repeated_failure("non_stop"),
        completed_candidate(VALID_CANDIDATE),
    ]);
    let runtime = runtime_with_compactor(
        "compaction-truncation-budget-growth",
        compactor.clone(),
        64_000,
    );
    seed_two_history_items_for_compaction(&runtime).await;

    runtime
        .compact_context_once(compaction_policy(), StepContext::default())
        .await
        .expect("the degraded retry should install a checkpoint")
        .expect("a checkpoint replacement should install");

    let requests = compactor.recorded_requests();
    assert_eq!(
        requests.len(),
        2,
        "one truncated attempt plus one degraded retry"
    );
    let first_ceiling = requests[0]
        .generation()
        .max_output_tokens()
        .expect("compaction always sends an output ceiling");
    let second_ceiling = requests[1]
        .generation()
        .max_output_tokens()
        .expect("compaction always sends an output ceiling");
    assert!(
        second_ceiling > first_ceiling,
        "the retry must reserve more reasoning room than the truncated attempt: {first_ceiling} then {second_ceiling}"
    );
}

/// One truncated attempt degrades into an affordable request for a bigger reserve.
#[tokio::test(flavor = "current_thread")]
async fn truncated_compaction_retries_with_a_bigger_reserve() {
    let compactor = RecordingModelProvider::with_script(vec![
        repeated_failure("non_stop"),
        completed_candidate(VALID_CANDIDATE),
    ]);
    let runtime = runtime_with_compactor_and_steps(
        "compaction-truncation-degrade",
        compactor.clone(),
        256_000,
        6,
    );
    for index in 0..6 {
        let events = collect_step(
            &runtime,
            &format!("covered turn {index} {}", "payload ballast ".repeat(400)),
            StepContext::default(),
        )
        .await;
        assert!(
            events
                .iter()
                .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted)),
            "seed step {index} should complete"
        );
    }

    let outcome = runtime
        .compact_context_once(
            CitationCompactionPolicy::new(Some(512), Some(16_384), 1).expect("valid policy"),
            StepContext::default(),
        )
        .await
        .expect("degraded compaction should complete")
        .expect("a checkpoint replacement should install");

    assert!(outcome.covered_history_item_count() > 0);
    let requests = compactor.recorded_requests();
    assert_eq!(
        requests.len(),
        2,
        "one truncated attempt plus one degraded retry"
    );
    let first = serde_json::to_string(requests[0].input()).expect("request input serializes");
    let second = serde_json::to_string(requests[1].input()).expect("request input serializes");
    // Covering less history only happens when the window cannot afford the
    // bigger reserve; either way both attempts stay inside the window.
    for (label, request) in [("truncated attempt", &requests[0]), ("retry", &requests[1])] {
        let ceiling = request
            .generation()
            .max_output_tokens()
            .expect("compaction always sends an output ceiling");
        let input = crate::token_estimate::estimate_model_input_tokens(request.input());
        assert!(
            input + ceiling <= 256_000,
            "{label} must fit the window: input {input} plus output {ceiling}"
        );
    }
    assert!(
        !first.is_empty() && !second.is_empty(),
        "both attempts must carry the compaction payload"
    );
}

/// A request that cannot fit the compaction window keeps more turns raw.
///
/// The runtime must shrink the covered window before calling the provider, so the
/// sent request holds its input and output budget inside the model window.
#[tokio::test(flavor = "current_thread")]
async fn compaction_fits_each_rolling_request_before_installing_the_final_tail() {
    let compactor = RecordingModelProvider::with_script(vec![
        completed_candidate(VALID_CANDIDATE),
        completed_candidate(VALID_CANDIDATE),
    ]);
    let runtime =
        runtime_with_compactor_and_steps("compaction-window-refit", compactor.clone(), 32_000, 6);
    seed_rolling_history(&runtime).await;

    let outcome = runtime
        .compact_context_once(
            CitationCompactionPolicy::new(Some(10_000), Some(99_999), 1).expect("valid policy"),
            StepContext::default(),
        )
        .await
        .expect("fitted compaction should complete")
        .expect("a checkpoint replacement should install");

    let requests = compactor.recorded_requests();
    assert_eq!(
        requests.len(),
        2,
        "fitting requires rolling, and manual compaction must complete both passes"
    );
    for request in &requests {
        let (input, output) = crate::compaction::compaction_request_required_tokens(request);
        assert!(input + output <= 32_000, "every rolling request must fit");
    }
    assert_eq!(outcome.covered_history_item_count(), 10);
    assert_eq!(outcome.retained_history_item_count(), 2);
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
    let primary = RecordingModelProvider::with_script_and_capabilities(
        Vec::new(),
        ModelCapabilities::new(
            true,
            true,
            false,
            true,
            Some(2_048),
            Some(super::TIGHT_WINDOW_OUTPUT_CAP_TOKENS),
        )
        .expect("tight primary capabilities"),
    );
    let runtime = Runtime::builder(session_id("compaction-payload-too-large"))
        .model_provider(Arc::new(primary), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            ModelName::new("compaction-model").expect("model"),
        )
        .automatic_compaction(CompactionConfig::disabled())
        .build()
        .expect("runtime");
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

    let policy = CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("tight policy");
    assert!(
        runtime
            .citation_compaction_input(policy)
            .await
            .expect("destination fits")
            .is_some()
    );
    let error = runtime
        .compact_context_once(policy, StepContext::default())
        .await
        .expect_err("oversized compactor request must be rejected");

    assert!(
        matches!(
            error,
            RuntimeError::CompactionModelRequestTooLarge {
                estimated_input_tokens,
                max_output_tokens,
                compactor_window_tokens: 2_048,
            } if estimated_input_tokens + max_output_tokens > 2_048
        ),
        "unexpected error: {error:?}"
    );
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

#[tokio::test(flavor = "current_thread")]
async fn compaction_rejects_summary_budget_above_declared_output_limit_without_a_call() {
    let compactor = RecordingModelProvider::with_script_and_capabilities(
        vec![completed_candidate(VALID_CANDIDATE)],
        ModelCapabilities::new(true, true, false, true, Some(64_000), Some(128))
            .expect("capabilities"),
    );
    let runtime = runtime_with_compactor("compaction-output-limit", compactor.clone(), 64_000);
    seed_two_history_items_for_compaction(&runtime).await;
    let error = runtime
        .compact_context_once(compaction_policy(), StepContext::default())
        .await
        .expect_err("limit too small");
    assert!(matches!(
        error,
        RuntimeError::Compaction {
            source: crate::CompactionError::OutputBudgetExceedsModelLimit {
                summary_tokens: 512,
                model_limit_tokens: 128
            }
        }
    ));
    assert!(compactor.recorded_requests().is_empty());
    assert!(runtime.compacted_checkpoint_summary().await.is_none());
}

#[path = "compaction_generation/repair.rs"]
mod repair;

#[path = "compaction_generation/boundaries.rs"]
mod boundaries;
