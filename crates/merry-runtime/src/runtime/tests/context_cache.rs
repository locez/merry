use crate::{
    CheckpointDecision, CitationCompactionPolicy, CompactedCheckpoint, RuntimeModelRole,
    StepContext,
    runtime::{
        CompactionConfig, Runtime, merry_read_checkpoint_ref_tool_name, request_context_budget,
        tests::support::{
            common::{collect_step, completed_event_with, model_name, named_model, session_id},
            memory::{ScriptedMemoryActivationSource, activated_memory, record_memory_artifact},
            model_provider::{RecordingModelProvider, ScriptedModelProviderResponse},
            runtime_factories::runtime_with_provider_and_memory_source,
        },
    },
};
use merry_core::RuntimeJournalPayload;
use merry_llm::{FinishReason, GenerationConfig, ModelCapabilities, ModelOutput, ModelProvider};
use std::sync::Arc;

const CACHE_KEY_COMPACTION_CANDIDATE: &str = r#"{
  "confirmed_decisions": [],
  "rejected_approaches": [],
  "constraints_preferences_boundaries": [],
  "corrected_misunderstandings": [],
  "durable_conclusions": [{
    "id": "c1",
    "text": "The oldest complete model turn was compacted.",
    "refs": ["h0", "h1"]
  }],
  "open_questions": [],
  "current_progress_and_next_steps": [],
  "exact_details": [],
  "handoffs": []
}"#;

fn checkpoint_item_index(input: &[merry_llm::ModelInputItem], checkpoint_text: &str) -> usize {
    input
        .iter()
        .position(|item| {
            matches!(
                item,
                merry_llm::ModelInputItem::Message(message)
                    if message.content().as_text().contains(checkpoint_text)
            )
        })
        .expect("request should contain the compacted checkpoint")
}

fn input_contains_text(input: &[merry_llm::ModelInputItem], expected: &str) -> bool {
    input.iter().any(|item| {
        matches!(
            item,
            merry_llm::ModelInputItem::Message(message)
                if message.content().as_text().contains(expected)
        )
    })
}

#[tokio::test(flavor = "current_thread")]
async fn requests_append_to_the_actual_provider_input_until_checkpoint_replacement() {
    let provider = RecordingModelProvider::new();
    let runtime = Runtime::builder(session_id("runtime-append-only-input"))
        .compacted_checkpoint(
            CompactedCheckpoint::new("cache-stable checkpoint sentinel").expect("valid checkpoint"),
        )
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    for text in ["first request", "second request", "third request"] {
        let events = collect_step(&runtime, text, StepContext::default()).await;
        assert!(
            events
                .iter()
                .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted)),
            "each provider step should complete"
        );
    }

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[1].input().starts_with(requests[0].input()),
        "the second actual provider input should append to the first"
    );
    assert!(
        requests[2].input().starts_with(requests[1].input()),
        "the third actual provider input should append to the second"
    );
    assert_eq!(requests[0].tools(), requests[1].tools());
    assert_eq!(requests[1].tools(), requests[2].tools());
    assert_eq!(requests[0].tools(), requests[2].tools());
    assert!(
        requests[0]
            .tools()
            .iter()
            .any(|tool| tool.name() == &merry_read_checkpoint_ref_tool_name())
    );
}

#[tokio::test(flavor = "current_thread")]
async fn checkpoint_prefix_stays_byte_stable_when_activated_memory_changes() {
    let checkpoint_text = "fixed checkpoint before changing memory";
    let memory_text = "This memory appears in the first request only.";
    let memory = activated_memory(
        "memory-first-request-only",
        memory_text,
        "memory-first-request-only-artifact",
    );
    let source = ScriptedMemoryActivationSource::new(vec![vec![memory], Vec::new()]);
    let provider = RecordingModelProvider::new();
    let runtime = runtime_with_provider_and_memory_source(
        "runtime-checkpoint-prefix-stability",
        provider.clone(),
        source,
    );
    record_memory_artifact(
        &runtime,
        "memory-first-request-only-artifact",
        "exact evidence for the first request memory",
    );
    runtime
        .inner
        .session
        .try_lock()
        .expect("session lock should be free")
        .set_compacted_checkpoint(
            CompactedCheckpoint::new(checkpoint_text).expect("valid checkpoint"),
        );

    collect_step(&runtime, "First topic request.", StepContext::default()).await;
    collect_step(&runtime, "Second topic request.", StepContext::default()).await;

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert!(input_contains_text(requests[0].input(), memory_text));
    assert!(!input_contains_text(requests[1].input(), memory_text));

    let first_checkpoint = checkpoint_item_index(requests[0].input(), checkpoint_text);
    let second_checkpoint = checkpoint_item_index(requests[1].input(), checkpoint_text);
    assert_eq!(
        &requests[0].input()[..=first_checkpoint],
        &requests[1].input()[..=second_checkpoint],
        "stable instructions and the checkpoint item must remain byte-identical even when later memory changes"
    );
    assert_eq!(requests[0].tools(), requests[1].tools());
}

#[tokio::test(flavor = "current_thread")]
async fn soft_watermark_does_not_call_the_compaction_provider() {
    let primary = RecordingModelProvider::with_script_and_capabilities(
        Vec::new(),
        ModelCapabilities::new(true, true, false, true, Some(100_000), Some(10_000))
            .expect("valid primary capabilities"),
    );
    let compactor = RecordingModelProvider::new();
    let runtime = Runtime::builder(session_id("runtime-soft-watermark"))
        .model_provider(Arc::new(primary.clone()), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            named_model("fake/soft-watermark-compactor"),
        )
        .automatic_compaction(CompactionConfig::enabled(
            CitationCompactionPolicy::new(None, None, 1).expect("valid policy"),
        ))
        .build()
        .expect("runtime should build");
    let generation = GenerationConfig::new(Some(10_000), false).expect("valid generation");

    let events = collect_step(
        &runtime,
        &"a".repeat(320_000),
        StepContext::default().with_generation_config(generation),
    )
    .await;

    let requests = primary.recorded_requests();
    assert_eq!(requests.len(), 1);
    let budget = request_context_budget(primary.capabilities(), &requests[0], None)
        .expect("request budget should resolve");
    assert_eq!(budget.decision, CheckpointDecision::PlanCheckpoint);
    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted))
    );
    assert!(events.iter().all(|event| {
        !matches!(
            event.payload,
            RuntimeJournalPayload::CompactionStarted
                | RuntimeJournalPayload::CompactionCompleted { .. }
        )
    }));
    assert!(compactor.recorded_requests().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn primary_and_compaction_streams_use_the_runtime_session_as_prompt_cache_key() {
    let primary = RecordingModelProvider::with_script_and_capabilities(
        Vec::new(),
        ModelCapabilities::new(true, true, false, true, Some(64_000), None)
            .expect("valid primary capabilities"),
    );
    let compactor = RecordingModelProvider::with_script_and_capabilities(
        vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event_with(
                vec![ModelOutput::text(CACHE_KEY_COMPACTION_CANDIDATE)],
                FinishReason::Stop,
            ),
        )])],
        ModelCapabilities::new(true, true, false, true, Some(256_000), None)
            .expect("valid compactor capabilities"),
    );
    let runtime = Runtime::builder(session_id("runtime-primary-compaction-cache-key"))
        .model_provider(Arc::new(primary.clone()), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            named_model("fake/cache-key-compactor"),
        )
        .automatic_compaction(CompactionConfig::disabled())
        .build()
        .expect("runtime should build");

    for text in ["old turn for cache-key compaction", "retained raw tail"] {
        let events = collect_step(&runtime, text, StepContext::default()).await;
        assert!(
            events
                .iter()
                .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted))
        );
    }
    runtime
        .compact_context_once(
            CitationCompactionPolicy::new(Some(512), Some(16_384), 1)
                .expect("valid compaction policy"),
            StepContext::default(),
        )
        .await
        .expect("compaction should succeed")
        .expect("history should compact");

    let primary_contexts = primary.recorded_contexts();
    assert_eq!(primary_contexts.len(), 2);
    assert!(primary_contexts.iter().all(|context| {
        context
            .prompt_cache_key()
            .is_some_and(|key| key.as_str() == "runtime-primary-compaction-cache-key")
    }));
    let compaction_contexts = compactor.recorded_contexts();
    assert_eq!(compaction_contexts.len(), 1);
    assert_eq!(
        compaction_contexts[0]
            .prompt_cache_key()
            .expect("compaction cache key should be set")
            .as_str(),
        "runtime-primary-compaction-cache-key"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn coordinator_tool_specs_and_stable_prefix_stay_fixed_across_plan_activation() {
    let provider = RecordingModelProvider::new();
    let runtime = Runtime::builder(session_id("runtime-plan-tool-cache-stability"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .coordinator_plan_tools()
        .build()
        .expect("runtime should build");

    collect_step(&runtime, "request before planning", StepContext::default()).await;
    runtime
        .begin_plan(crate::BeginPlanInput {
            reason: "coordinate a recursive task".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan activation succeeds");
    collect_step(&runtime, "request during planning", StepContext::default()).await;

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].tools(), requests[1].tools());
    assert_eq!(
        requests[0].stable_prefix_hash(),
        requests[1].stable_prefix_hash()
    );
    assert_ne!(
        requests[0].dynamic_context_hash(),
        requests[1].dynamic_context_hash()
    );
    for name in crate::plan::tools::COORDINATOR_PLAN_TOOL_NAMES {
        assert!(
            requests[0]
                .tools()
                .iter()
                .any(|tool| tool.name().as_str() == name),
            "missing stable plan tool {name}"
        );
    }
}

fn message_text(item: &merry_llm::ModelInputItem) -> &str {
    match item {
        merry_llm::ModelInputItem::Message(message) => message.content().as_text(),
        other => panic!("expected a message input item, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn compaction_request_reuses_the_step_stable_prefix_and_appends_the_directive() {
    let primary = RecordingModelProvider::with_script_and_capabilities(
        Vec::new(),
        ModelCapabilities::new(true, true, false, true, Some(64_000), None)
            .expect("valid primary capabilities"),
    );
    let compactor = RecordingModelProvider::with_script_and_capabilities(
        vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event_with(
                vec![ModelOutput::text(CACHE_KEY_COMPACTION_CANDIDATE)],
                FinishReason::Stop,
            ),
        )])],
        ModelCapabilities::new(true, true, false, true, Some(256_000), None)
            .expect("valid compactor capabilities"),
    );
    let runtime = Runtime::builder(session_id("runtime-compaction-prefix-reuse"))
        .model_provider(Arc::new(primary.clone()), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            named_model("fake/prefix-reuse-compactor"),
        )
        // The compaction config owns the reasoning level and must override
        // whatever the primary model asks for.
        .automatic_compaction(CompactionConfig::disabled().with_reasoning_effort(Some(
            merry_llm::ReasoningEffort::new("low").expect("valid reasoning effort"),
        )))
        .build()
        .expect("runtime should build");
    let generation = GenerationConfig::new(None, false)
        .expect("valid generation")
        .with_reasoning_effort(Some(
            merry_llm::ReasoningEffort::new("max").expect("valid reasoning effort"),
        ));

    for text in ["old turn before prefix reuse", "retained raw tail"] {
        collect_step(
            &runtime,
            text,
            StepContext::default().with_generation_config(generation.clone()),
        )
        .await;
    }
    runtime
        .compact_context_once(
            CitationCompactionPolicy::new(Some(512), Some(16_384), 1)
                .expect("valid compaction policy"),
            StepContext::default().with_generation_config(generation),
        )
        .await
        .expect("compaction should succeed")
        .expect("history should compact");

    let primary_requests = primary.recorded_requests();
    let primary_request = primary_requests.last().expect("primary request recorded");
    let compaction_requests = compactor.recorded_requests();
    let compaction_request = compaction_requests
        .first()
        .expect("compaction request recorded");

    assert_eq!(
        compaction_request.stable_prefix_item_count(),
        primary_request.stable_prefix_item_count(),
        "compaction must reuse the session stable prefix item count"
    );
    assert_eq!(
        compaction_request.stable_prefix_input(),
        primary_request.stable_prefix_input(),
        "compaction prefix items must stay byte-identical to the step prefix"
    );
    assert!(
        !compaction_request.stable_prefix_input().is_empty(),
        "compaction must keep the session prefix instead of a dedicated system prompt"
    );

    assert!(
        compaction_request
            .input()
            .starts_with(primary_request.input())
    );
    assert_eq!(compaction_request.tools(), primary_request.tools());
    assert_eq!(
        compaction_request.tool_profile_hash(),
        primary_request.tool_profile_hash()
    );
    assert_eq!(
        compaction_request.response_format(),
        primary_request.response_format()
    );
    assert_eq!(
        compaction_request.stable_prefix_hash(),
        primary_request.stable_prefix_hash()
    );
    let directive = message_text(compaction_request.input().last().expect("tail directive"));
    assert!(directive.contains("COMPACTION REQUEST: Update the session checkpoint"));
    assert!(directive.contains(
        "Summary soft target: at most 512 estimated tokens; hard rendered-summary limit: 512 estimated tokens"
    ));
    assert!(directive.contains("covered_history_references"));
    assert!(
        !directive.contains("old turn before prefix reuse"),
        "history must not be duplicated into the directive"
    );
    assert_eq!(
        compaction_request
            .generation()
            .reasoning_effort()
            .map(merry_llm::ReasoningEffort::as_str),
        Some("low"),
        "compaction must use the configured compaction reasoning level, not the primary model's"
    );
    assert_eq!(
        primary_request
            .generation()
            .reasoning_effort()
            .map(merry_llm::ReasoningEffort::as_str),
        Some("max"),
        "the primary request keeps its own reasoning effort"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn compaction_preserves_native_tool_items_and_indexes_only_covered_evidence() {
    use crate::runtime::tests::support::common::{artifact_id, pending_tool_call};
    use merry_core::{
        ArtifactKind, ArtifactRef, PendingToolCallBatch, ToolCallBatchId, ToolCallResult,
    };
    use merry_llm::ModelInputItem;
    let primary = RecordingModelProvider::new();
    let compactor =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event_with(
                vec![ModelOutput::text(
                    &CACHE_KEY_COMPACTION_CANDIDATE.replace("\"h0\", \"h1\"", "\"h0\""),
                )],
                FinishReason::Stop,
            ),
        )])]);
    let runtime = Runtime::builder(session_id("cache-native-tools"))
        .model_provider(Arc::new(primary.clone()), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            model_name(),
        )
        .automatic_compaction(CompactionConfig::disabled())
        .build()
        .expect("runtime");
    {
        let mut session = runtime.inner.session.lock().await;
        for index in 0..2 {
            let turn = session.begin_model_turn().expect("turn");
            session
                .record_user_message_body(turn, &format!("covered user {index}"))
                .expect("user");
            let call = pending_tool_call(&format!("cache-call-{index}"));
            session
                .record_tool_call_batch_pending(
                    turn,
                    PendingToolCallBatch::new(
                        ToolCallBatchId::new(&format!("cache-batch-{index}")).expect("batch id"),
                        vec![call.clone()],
                    )
                    .expect("batch"),
                )
                .expect("call");
            session.close_model_response(turn, true).expect("close");
            session
                .submit_tool_result(
                    ToolCallResult::succeeded(
                        call.id().clone(),
                        ArtifactRef::new(
                            artifact_id(&format!("cache-result-{index}")),
                            ArtifactKind::Text,
                        ),
                    ),
                    crate::ArtifactContent::text(format!("verbatim result {index}")),
                )
                .expect("result");
        }
    }
    collect_step(&runtime, "retained current user", StepContext::default()).await;
    runtime
        .compact_context_once(
            CitationCompactionPolicy::new(Some(512), Some(16384), 1).expect("policy"),
            StepContext::default(),
        )
        .await
        .expect("compaction");
    let requests = compactor.recorded_requests();
    let request = &requests[0];
    let originals = primary.recorded_requests();
    let original = originals.last().expect("primary request");
    assert!(request.input().starts_with(original.input()));
    assert_eq!(request.tools(), original.tools());
    assert_eq!(request.stable_prefix_hash(), original.stable_prefix_hash());
    assert_eq!(
        request
            .input()
            .iter()
            .filter(|item| matches!(item, ModelInputItem::ToolCall(_)))
            .count(),
        2
    );
    assert_eq!(
        request
            .input()
            .iter()
            .filter(|item| matches!(item, ModelInputItem::ToolResult(_)))
            .count(),
        2
    );
    let directive = message_text(request.input().last().expect("directive"));
    assert!(!directive.contains("verbatim result"));
    assert!(!directive.contains("retained current user"));
    let payload_json = directive
        .split_once("<merry_compaction_payload>\n")
        .expect("payload start")
        .1
        .split_once("\n</merry_compaction_payload>")
        .expect("payload end")
        .0;
    let payload: serde_json::Value = serde_json::from_str(payload_json).expect("payload json");
    let references = payload["covered_history_references"]
        .as_array()
        .expect("ref index");
    assert_eq!(references.len(), 4);
    for reference in references {
        let index = usize::try_from(reference["input_item_index"].as_u64().expect("input index"))
            .expect("index fits");
        match reference["ref_id"].as_str().expect("ref id") {
            "h2" | "h5" => assert!(matches!(
                &request.input()[index],
                ModelInputItem::ToolResult(_)
            )),
            "h0" | "h3" => assert!(
                matches!(&request.input()[index], ModelInputItem::Message(message) if message.role() == merry_llm::ModelMessageRole::User)
            ),
            other => panic!("uncovered ref {other}"),
        }
    }
}
