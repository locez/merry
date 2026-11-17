use crate::support::{
    models::{ScriptedModelProvider, completed_outputs_event, completed_text_event, model_name},
    runtime::{collect_step, session_id},
};
use merry_core::RuntimeJournalPayload;
use merry_llm::{FinishReason, ModelError, ModelEvent, ModelOutput};
use merry_runtime::{CitationCompactionPolicy, ContextCompiler, Runtime, StepContext};
use std::sync::Arc;

#[derive(Debug, serde::Deserialize)]
struct CitationCompactionFixture {
    semantic_values: CitationCompactionSemanticValues,
    messages: Vec<FixtureMessage>,
    candidates: Vec<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
struct CitationCompactionSemanticValues {
    confirmed_decision: String,
    decision_reason: String,
    rejected_approach: String,
    rejection_reason: String,
    constraint: String,
    correction: String,
    durable_conclusion: String,
    loss_conclusion: String,
    open_question: String,
    progress_generations: Vec<String>,
    next_step: String,
    exact_path: String,
    exact_number: u64,
    exact_interface: String,
}

#[derive(Debug, serde::Deserialize)]
struct FixtureMessage {
    role: String,
    text: String,
}

fn fixture_provider_steps(
    fixture: &CitationCompactionFixture,
) -> Vec<Vec<Result<ModelEvent, ModelError>>> {
    let mut assistant_texts = fixture
        .messages
        .iter()
        .filter(|message| message.role == "assistant")
        .map(|message| message.text.as_str());
    let user_count = fixture
        .messages
        .iter()
        .filter(|message| message.role == "user")
        .count();
    let mut steps = Vec::with_capacity(user_count + 1);
    for _ in 0..user_count {
        let text = assistant_texts.next().unwrap_or("fixture assistant ack");
        steps.push(vec![Ok(completed_text_event(text))]);
    }
    steps.push(vec![Ok(completed_outputs_event(
        vec![ModelOutput::text(
            &serde_json::to_string(
                fixture
                    .candidates
                    .first()
                    .expect("fixture has an initial candidate"),
            )
            .expect("candidate serializes"),
        )],
        FinishReason::Stop,
    ))]);
    steps
}

async fn seed_fixture_messages(runtime: &Runtime, messages: &[FixtureMessage]) {
    for message in messages {
        match message.role.as_str() {
            "user" => {
                let events = collect_step(runtime, &message.text).await;
                assert!(
                    events
                        .iter()
                        .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted)),
                    "fixture user message should complete"
                );
            }
            "assistant" => {}
            other => panic!("unsupported fixture role: {other}"),
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn citation_compaction_fixture_preserves_required_design_meanings() {
    let fixture = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/citation_compaction_design_fixture.json"
    ));
    let fixture: CitationCompactionFixture = serde_json::from_str(fixture).expect("fixture parses");
    assert_eq!(fixture.candidates.len(), 3);
    assert_eq!(fixture.semantic_values.progress_generations.len(), 3);
    let provider = ScriptedModelProvider::new(fixture_provider_steps(&fixture));
    let runtime = Runtime::builder(session_id("citation-fixture"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime builds");

    seed_fixture_messages(&runtime, &fixture.messages).await;

    let outcome = runtime
        .compact_context_once(
            CitationCompactionPolicy::new(Some(384), Some(8192), 2).expect("valid policy"),
            StepContext::default(),
        )
        .await
        .expect("compaction succeeds")
        .expect("compaction runs");

    assert!(
        outcome.covered_history_item_count() >= 14,
        "fixture should compact enough history to reveal checkpoint behavior"
    );

    let requests = provider.recorded_requests();
    let compaction_request = requests.last().expect("compaction request exists");
    let compaction_request_text = compaction_request
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        compaction_request_text.contains(&fixture.semantic_values.confirmed_decision),
        "the compactor must receive the covered source containing all approved meanings"
    );
    assert!(
        compaction_request_text.contains("Retained tail sentinel"),
        "cache-preserving compaction keeps raw tail visible but excludes it from summary coverage"
    );

    let snapshot = ContextCompiler::new()
        .compile(&runtime.context_snapshot().await)
        .expect("context compiles")
        .to_snapshot();

    let semantics = &fixture.semantic_values;
    let exact_number = semantics.exact_number.to_string();
    for expected in [
        semantics.confirmed_decision.as_str(),
        semantics.decision_reason.as_str(),
        semantics.rejected_approach.as_str(),
        semantics.rejection_reason.as_str(),
        semantics.constraint.as_str(),
        semantics.correction.as_str(),
        semantics.durable_conclusion.as_str(),
        semantics.loss_conclusion.as_str(),
        semantics.open_question.as_str(),
        semantics.progress_generations[0].as_str(),
        semantics.next_step.as_str(),
        semantics.exact_path.as_str(),
        exact_number.as_str(),
        semantics.exact_interface.as_str(),
    ] {
        assert!(
            snapshot.contains(expected),
            "missing expected checkpoint meaning: {expected}"
        );
    }

    runtime
        .compacted_checkpoint_summary()
        .await
        .expect("prose checkpoint is installed");
}
