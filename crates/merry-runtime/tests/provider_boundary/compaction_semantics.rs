use crate::support::{
    compaction::read_full_checkpoint_ref,
    models::{ScriptedModelProvider, completed_outputs_event, completed_text_event, model_name},
    runtime::{collect_step, session_id},
};
use futures_util::StreamExt;
use merry_core::RuntimeJournalPayload;
use merry_llm::{
    FinishReason, GenerationConfig, ModelContent, ModelError, ModelEvent, ModelEventStream,
    ModelMessage, ModelMessageRole, ModelName, ModelOutput, ModelProvider, ModelRequest,
    ModelResponseFormat, ModelStreamContext, ModelStructuredOutputFormat,
    testing::FakeModelProvider,
};
use merry_provider_openai::{OpenAiProvider, OpenAiProviderConfig};
use merry_runtime::{
    CheckpointRefId, CheckpointSection, CheckpointSections, CitationCompactionInput,
    CitationCompactionPolicy, CompactedCheckpointCandidate, ContextCompiler, Runtime,
    RuntimeModelRole, StepContext, citation_compaction_system_prompt,
};
use std::{collections::BTreeSet, sync::Arc};
use tokio_util::sync::CancellationToken;

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

fn live_openai_provider_from_env() -> Option<(OpenAiProvider, ModelName)> {
    if std::env::var("MERRY_OPENAI_LIVE_TESTS").ok().as_deref() != Some("1") {
        eprintln!("skipping live compactor test: set MERRY_OPENAI_LIVE_TESTS=1");
        return None;
    }

    let api_key = match std::env::var("MERRY_OPENAI_API_KEY")
        .or_else(|_| std::env::var("OPENAI_API_KEY"))
    {
        Ok(value) => value,
        Err(_) => {
            eprintln!("skipping live compactor test: set MERRY_OPENAI_API_KEY or OPENAI_API_KEY");
            return None;
        }
    };
    let model = match std::env::var("MERRY_OPENAI_MODEL") {
        Ok(value) => value,
        Err(_) => {
            eprintln!("skipping live compactor test: set MERRY_OPENAI_MODEL");
            return None;
        }
    };
    let mut config = match OpenAiProviderConfig::new(&api_key) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("skipping live compactor test: invalid OpenAI config: {error}");
            return None;
        }
    };
    if let Ok(base_url) = std::env::var("MERRY_OPENAI_BASE_URL") {
        config = match config.with_base_url(&base_url) {
            Ok(config) => config,
            Err(error) => {
                eprintln!("skipping live compactor test: invalid MERRY_OPENAI_BASE_URL: {error}");
                return None;
            }
        };
    }
    let model = match ModelName::new(&model) {
        Ok(model) => model,
        Err(error) => {
            eprintln!("skipping live compactor test: invalid MERRY_OPENAI_MODEL: {error}");
            return None;
        }
    };

    Some((OpenAiProvider::new(config), model))
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

async fn request_live_compaction_candidate(
    compactor: &OpenAiProvider,
    compaction_model: &ModelName,
    input: &CitationCompactionInput,
) -> String {
    let payload = input
        .to_model_payload_json()
        .expect("compaction payload serializes");
    let response_format = ModelResponseFormat::StructuredOutput(
        ModelStructuredOutputFormat::new(
            "compacted_checkpoint_candidate",
            input
                .model_response_schema()
                .expect("structured output schema is valid"),
        )
        .expect("structured output format is valid"),
    );
    let request = ModelRequest::new_with_continuations_and_stable_prefix_and_response_format(
        compaction_model.clone(),
        vec![
            ModelMessage::new(
                ModelMessageRole::System,
                ModelContent::text(citation_compaction_system_prompt())
                    .expect("system prompt is valid"),
            )
            .expect("system message is valid"),
            ModelMessage::new(
                ModelMessageRole::User,
                ModelContent::text(&payload).expect("payload is valid model content"),
            )
            .expect("user message is valid"),
        ],
        Vec::new(),
        Vec::new(),
        GenerationConfig::new(Some(input.resolved_budget().output_token_limit()), false)
            .expect("generation config is valid"),
        1,
        Some(response_format),
    )
    .expect("compaction request is valid");
    let stream = compactor
        .stream_model(request, ModelStreamContext::new(CancellationToken::new()))
        .await
        .expect("live compactor stream starts");
    collect_text_output(stream)
        .await
        .expect("live compactor returns text")
}

fn assert_live_candidate_meaning(
    candidate: &CompactedCheckpointCandidate,
    expected: &CitationCompactionSemanticValues,
    cycle: usize,
) {
    let sections = candidate.sections();
    assert_candidate_entry_contains(
        sections,
        CheckpointSection::ConfirmedDecision,
        &["five", "completed model turn"],
        &["short", "tool", "frequent"],
    );
    assert_candidate_entry_contains(
        sections,
        CheckpointSection::RejectedApproach,
        &["compress", "soft watermark"],
        &["distortion", "cache"],
    );
    for (section, required) in [
        (
            CheckpointSection::ConstraintPreferenceBoundary,
            &["checkpoint replacement", "cache", "boundary"][..],
        ),
        (
            CheckpointSection::CorrectedMisunderstanding,
            &["task ledger", "not", "context compression"][..],
        ),
        (
            CheckpointSection::DurableConclusion,
            &["refs", "exact", "continuation"][..],
        ),
        (
            CheckpointSection::DurableConclusion,
            &["lossy", "distortion"][..],
        ),
        (
            CheckpointSection::OpenQuestion,
            &["live", "semantic retention", "deterministic"][..],
        ),
    ] {
        assert_candidate_entry_contains(sections, section, required, &[]);
    }
    let progress_words = live_progress_words(cycle);
    assert_candidate_entry_contains(
        sections,
        CheckpointSection::CurrentProgressAndNextStep,
        progress_words,
        &[],
    );
    assert_candidate_entry_contains(
        sections,
        CheckpointSection::CurrentProgressAndNextStep,
        &["256k", "three-cycle"],
        &[],
    );
    for other_cycle in (0..3).filter(|other| *other != cycle) {
        let other_words = live_progress_words(other_cycle);
        assert!(
            sections
                .entries(CheckpointSection::CurrentProgressAndNextStep)
                .iter()
                .all(|entry| {
                    let text = entry.text().to_ascii_lowercase();
                    !other_words.iter().all(|word| text.contains(word))
                }),
            "live checkpoint retained the wrong progress generation {}",
            other_cycle + 1
        );
    }
    for exact in [
        expected.exact_path.as_str(),
        expected.exact_interface.as_str(),
    ] {
        assert_candidate_exact_entry(sections, CheckpointSection::ExactDetail, exact);
    }
    assert_candidate_exact_entry(
        sections,
        CheckpointSection::ExactDetail,
        &expected.exact_number.to_string(),
    );
}

fn live_progress_words(cycle: usize) -> &'static [&'static str] {
    match cycle {
        0 => &["design", "approved", "rolling replacement"],
        1 => &["first rolling checkpoint", "next replacement"],
        2 => &["two rolling checkpoint", "third generation"],
        _ => panic!("live test has exactly three cycles"),
    }
}

fn assert_candidate_entry_contains(
    sections: &CheckpointSections,
    section: CheckpointSection,
    text_needles: &[&str],
    rationale_needles: &[&str],
) {
    assert!(
        sections.entries(section).iter().any(|entry| {
            let text = entry.text().to_ascii_lowercase();
            let rationale = entry.rationale().unwrap_or_default().to_ascii_lowercase();
            text_needles.iter().all(|needle| text.contains(needle))
                && rationale_needles
                    .iter()
                    .all(|needle| rationale.contains(needle))
        }),
        "live checkpoint section {} lost required meaning {:?} or reason {:?}",
        section.as_str(),
        text_needles,
        rationale_needles
    );
}

fn assert_candidate_exact_entry(
    sections: &CheckpointSections,
    section: CheckpointSection,
    exact: &str,
) {
    assert!(
        sections
            .entries(section)
            .iter()
            .any(|entry| entry.text() == exact),
        "live checkpoint section {} lost exact literal: {exact}",
        section.as_str()
    );
}

fn candidate_entry_ids(candidate: &CompactedCheckpointCandidate) -> BTreeSet<String> {
    CheckpointSection::ALL
        .into_iter()
        .flat_map(|section| candidate.sections().entries(section))
        .map(|entry| entry.id().as_str().to_owned())
        .collect()
}

fn assert_candidate_handoffs(
    candidate: &CompactedCheckpointCandidate,
    previous_ids: &BTreeSet<String>,
) {
    let handed_off = candidate
        .handoffs()
        .iter()
        .map(|handoff| handoff.old_id().as_str().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(&handed_off, previous_ids);
}

async fn assert_candidate_refs_resolve_original_sources(
    runtime: &Runtime,
    candidate: &CompactedCheckpointCandidate,
    original_sources: &[String],
) {
    let refs = CheckpointSection::ALL
        .into_iter()
        .flat_map(|section| candidate.sections().entries(section))
        .flat_map(|entry| entry.refs().iter().cloned())
        .collect::<BTreeSet<_>>();
    assert!(
        !refs.is_empty(),
        "live checkpoint must cite original sources"
    );
    for ref_id in refs {
        let content = read_full_checkpoint_ref(runtime, &ref_id).await;
        assert!(
            original_sources.iter().any(|source| source == &content),
            "checkpoint ref {} did not resolve to an original transcript artifact",
            ref_id.as_str()
        );
    }
}

async fn collect_text_output(stream: ModelEventStream) -> Result<String, String> {
    let mut stream = stream;
    let mut saw_delta = false;
    while let Some(item) = stream.next().await {
        match item {
            Ok(ModelEvent::Started) => {}
            Ok(ModelEvent::OutputTextDelta { delta }) => {
                if !delta.is_empty() {
                    saw_delta = true;
                }
            }
            Ok(ModelEvent::ToolCallRequested { .. }) => {
                return Err("model requested a tool call".to_owned());
            }
            Ok(ModelEvent::Completed { response }) => {
                if response.finish_reason() != FinishReason::Stop {
                    return Err(format!(
                        "model finished with {:?}",
                        response.finish_reason()
                    ));
                }
                let [ModelOutput::Text { text }] = response.outputs() else {
                    return Err("model must return exactly one text output".to_owned());
                };
                if saw_delta {
                    eprintln!("live compactor emitted streaming text deltas");
                }
                return Ok(text.clone());
            }
            Err(error) => return Err(error.to_string()),
        }
    }

    Err("model stream ended before completion".to_owned())
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
        !compaction_request_text.contains("Retained tail sentinel"),
        "retained raw tail must stay out of the compactor request"
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

    let summary = runtime
        .compacted_checkpoint_summary()
        .await
        .expect("citation checkpoint is installed");
    assert_eq!(summary.entry_count(), 12);
    assert_eq!(summary.ref_count(), 1);

    let page = runtime
        .read_checkpoint_ref_page(&CheckpointRefId::new("h0").expect("valid ref id"), 0, 4096)
        .await
        .expect("ref resolves");
    let first_user_message = fixture
        .messages
        .iter()
        .find(|message| message.role == "user")
        .expect("fixture has a user source message");
    assert_eq!(page.content(), first_user_message.text);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires live OpenAI-compatible compactor; set MERRY_OPENAI_LIVE_TESTS=1, MERRY_OPENAI_API_KEY or OPENAI_API_KEY, and MERRY_OPENAI_MODEL"]
async fn live_compactor_preserves_eight_categories_across_three_rolls() {
    let Some((compactor, compaction_model)) = live_openai_provider_from_env() else {
        return;
    };
    let fixture: CitationCompactionFixture = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/citation_compaction_design_fixture.json"
    )))
    .expect("live rolling fixture parses");
    let primary_output = "live primary acknowledgement";
    let primary = FakeModelProvider::new(vec![Ok(completed_text_event(primary_output))]);
    let runtime = Runtime::builder(session_id("live-compactor-three-roll-quality"))
        .model_provider(Arc::new(primary), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            compaction_model.clone(),
        )
        .build()
        .expect("runtime builds");

    let mut original_sources = vec![primary_output.to_owned()];
    let policy = CitationCompactionPolicy::new(Some(1_024), None, 2).expect("valid policy");
    let mut previous_ids: Option<BTreeSet<String>> = None;

    for (cycle, range) in [0..5, 5..8, 8..11].into_iter().enumerate() {
        for (offset, fixture_message) in fixture.messages[range].iter().enumerate() {
            assert_eq!(fixture_message.role, "user");
            let message = if cycle > 0 && offset == 0 {
                format!(
                    "Progress update; preserve this exact sentence in the progress section: {}\n{}",
                    fixture.semantic_values.progress_generations[cycle], fixture_message.text
                )
            } else {
                fixture_message.text.clone()
            };
            let events = collect_step(&runtime, &message).await;
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted)),
                "live fixture message should complete"
            );
            original_sources.push(message);
        }

        let input = runtime
            .citation_compaction_input(policy)
            .await
            .expect("live compaction input builds")
            .expect("live compaction input exists");
        let candidate_json =
            request_live_compaction_candidate(&compactor, &compaction_model, &input).await;
        eprintln!(
            "live rolling compactor generation {} raw candidate:\n{candidate_json}",
            cycle + 1
        );
        let candidate = CompactedCheckpointCandidate::from_json(&candidate_json)
            .expect("live candidate uses the checkpoint schema");
        assert_live_candidate_meaning(&candidate, &fixture.semantic_values, cycle);
        match &previous_ids {
            Some(ids) => assert_candidate_handoffs(&candidate, ids),
            None => assert!(candidate.handoffs().is_empty()),
        }
        let current_ids = candidate_entry_ids(&candidate);

        let outcome = runtime
            .install_citation_compaction_candidate(input, &candidate_json)
            .await
            .expect("live compaction candidate installs");
        eprintln!(
            "live rolling compactor generation {} covered_history_items={} checkpoint_entries={}",
            cycle + 1,
            outcome.covered_history_item_count(),
            current_ids.len()
        );
        assert_candidate_refs_resolve_original_sources(&runtime, &candidate, &original_sources)
            .await;
        previous_ids = Some(current_ids);
    }
}
