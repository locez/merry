use crate::{
    CheckpointDecision, CitationCompactionPolicy, CompactedCheckpoint, RuntimeModelRole,
    StepContext,
    runtime::{
        AutomaticCompactionConfig, Runtime, merry_read_checkpoint_ref_tool_name,
        request_context_budget,
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
        .automatic_compaction(AutomaticCompactionConfig::enabled(
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
        .automatic_compaction(AutomaticCompactionConfig::disabled())
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
    let runtime =
        Runtime::builder(session_id("runtime-compaction-prefix-reuse"))
            .model_provider(Arc::new(primary.clone()), model_name())
            .model_provider_for_role(
                RuntimeModelRole::ContextCompaction,
                Arc::new(compactor.clone()),
                named_model("fake/prefix-reuse-compactor"),
            )
            // The compaction config owns the reasoning level and must override
            // whatever the primary model asks for.
            .automatic_compaction(AutomaticCompactionConfig::disabled().with_reasoning_effort(
                Some(merry_llm::ReasoningEffort::new("low").expect("valid reasoning effort")),
            ))
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

    let input = compaction_request.input();
    let prefix_len = compaction_request.stable_prefix_item_count();
    assert_eq!(
        input.len(),
        prefix_len + 2,
        "compaction input is the stable prefix, the directive, and the payload"
    );
    assert!(
        message_text(&input[0]).starts_with("<merry_runtime_instructions>\n"),
        "compaction must reuse the session's tagged runtime instructions"
    );
    let directive = message_text(&input[prefix_len]);
    assert!(
        directive.starts_with("<merry_compaction_instructions>\n")
            && directive.ends_with("\n</merry_compaction_instructions>"),
        "the compaction directive must be one bounded instruction block: {directive}"
    );
    assert!(
        directive.contains("Context compaction request."),
        "unexpected compaction directive: {directive}"
    );
    let payload = message_text(&input[prefix_len + 1]);
    assert!(
        payload.starts_with("<merry_compaction_payload>\n")
            && payload.ends_with("\n</merry_compaction_payload>"),
        "the compaction payload must be one bounded data block: {payload}"
    );
    assert!(
        payload.contains("\"available_ref_ids\""),
        "the payload block must still carry the structured compaction payload"
    );
    let payload_json = payload
        .trim_start_matches("<merry_compaction_payload>\n")
        .trim_end_matches("\n</merry_compaction_payload>");
    assert!(
        serde_json::from_str::<serde_json::Value>(payload_json).is_ok(),
        "the payload boundary must wrap strict JSON without escaping it"
    );

    let format = compaction_request
        .response_format()
        .expect("compaction must keep structured output");
    let merry_llm::ModelResponseFormat::StructuredOutput(format) = format;
    assert_eq!(format.name(), "compacted_checkpoint_candidate");
    assert!(
        compaction_request.tools().is_empty(),
        "compaction stays outside the agent loop and carries no tools"
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
