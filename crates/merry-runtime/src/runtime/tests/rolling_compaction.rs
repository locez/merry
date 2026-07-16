use super::*;
use crate::{
    CheckpointHandoffAction, CheckpointSection, ContextCompiler, FileSessionStore,
    SessionTranscriptItem,
    session::{ModelTurnId, TranscriptItemSnapshot},
};
use merry_core::ArtifactId;
use merry_llm::{ModelContent, ModelInputItem, ModelMessage};
use serde::Deserialize;
use serde_json::Value;

const FIXTURE_JSON: &str =
    include_str!("../../../tests/fixtures/citation_compaction_design_fixture.json");
const DEEP_SOURCE_SENTINEL: &str = "SOURCE_SENTINEL_AFTER_BYTE_1200";
const EXPECTED_ENTRY_COUNT: usize = 12;

#[derive(Debug, Deserialize)]
struct RollingCompactionFixture {
    semantic_values: RollingSemanticValues,
    messages: Vec<RollingFixtureMessage>,
    candidates: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct RollingFixtureMessage {
    role: String,
    text: String,
}

#[derive(Debug, Deserialize)]
struct RollingSemanticValues {
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

#[derive(Debug)]
struct CycleState {
    transcript: Vec<SessionTranscriptItem>,
    boundary: ModelTurnId,
    checkpoint_id: CheckpointId,
    checkpoint_item: ModelInputItem,
}

#[tokio::test(flavor = "current_thread")]
async fn rolling_compaction_preserves_protocol_and_meaning_for_64k_three_times() {
    run_three_cycle_case(64_000, 5_120).await;
}

#[tokio::test(flavor = "current_thread")]
async fn rolling_compaction_preserves_protocol_and_meaning_for_256k_three_times() {
    run_three_cycle_case(256_000, 20_480).await;
}

async fn run_three_cycle_case(window_tokens: u64, expected_output_ceiling: u64) {
    let fixture: RollingCompactionFixture =
        serde_json::from_str(FIXTURE_JSON).expect("rolling compaction fixture parses");
    assert_eq!(fixture.candidates.len(), 3);
    assert_eq!(fixture.semantic_values.progress_generations.len(), 3);
    let semantic_source = fixture
        .messages
        .iter()
        .find(|message| message.role == "user")
        .expect("fixture contains a semantic source user message")
        .text
        .clone();

    let primary = RecordingModelProvider::with_script_and_capabilities(
        (0..32).map(scripted_assistant_response).collect(),
        primary_capabilities(window_tokens),
    );
    let compactor = RecordingModelProvider::with_script_and_capabilities(
        fixture
            .candidates
            .iter()
            .map(|candidate| {
                let candidate = serde_json::to_string(candidate).expect("candidate serializes");
                ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
                    vec![ModelOutput::text(&candidate)],
                    FinishReason::Stop,
                ))])
            })
            .collect(),
        compactor_capabilities(),
    );
    let policy = CitationCompactionPolicy::new(None, None, 5).expect("valid rolling policy");
    let id = session_id(&format!("rolling-context-{window_tokens}"));
    let runtime = Runtime::builder(id.clone())
        .model_provider(Arc::new(primary.clone()), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            named_model("fake/rolling-compactor"),
        )
        .automatic_compaction(AutomaticCompactionConfig::enabled(policy))
        .build()
        .expect("runtime builds");

    let target_turn_bytes =
        usize::try_from(window_tokens.saturating_mul(48) / 100).expect("test window fits usize");
    let mut submitted_inputs = Vec::new();
    let mut previous_cycle: Option<CycleState> = None;
    let mut first_source_text = None;
    let mut large_turn_index = 0usize;
    let mut completed_cycles = 0usize;

    while completed_cycles < 3 {
        assert!(
            large_turn_index < 24,
            "three compactions should finish promptly"
        );
        let marker = format!("large-turn-{large_turn_index}");
        let prefix = if large_turn_index == 0 {
            format!(
                "{marker}\n{semantic_source}\n{}\n{DEEP_SOURCE_SENTINEL}",
                "source-padding-before-deep-sentinel ".repeat(40)
            )
        } else {
            format!("{marker}\nsynthetic context for rolling compaction")
        };
        let input = padded_text(&prefix, target_turn_bytes);
        if first_source_text.is_none() {
            assert!(
                input
                    .find(DEEP_SOURCE_SENTINEL)
                    .is_some_and(|offset| offset > 1_200),
                "the exact source sentinel must sit beyond the old excerpt boundary"
            );
            first_source_text = Some(input.clone());
        }
        submitted_inputs.push(input.clone());
        large_turn_index += 1;

        let events = collect_step(&runtime, &input, StepContext::default()).await;
        assert_step_completed_without_failure(&events);
        let Some(checkpoint_id) = completed_checkpoint_id(&events) else {
            continue;
        };

        completed_cycles += 1;
        let cycle = completed_cycles;
        assert_eq!(compactor.recorded_requests().len(), cycle);
        if cycle == 1 {
            assert_eq!(
                large_turn_index, 8,
                "seven large turns must fit and the eighth must cross the hard watermark"
            );
        }
        let checkpoint_id =
            CheckpointId::new(&checkpoint_id).expect("event checkpoint id is valid");
        assert_eq!(
            runtime
                .compacted_checkpoint_summary()
                .await
                .and_then(|summary| summary.checkpoint_id().cloned()),
            Some(checkpoint_id.clone())
        );

        let primary_requests = primary.recorded_requests();
        let trigger_request = primary_requests.last().expect("trigger request records");
        assert_recent_raw_turns(trigger_request, &submitted_inputs);

        let compactor_requests = compactor.recorded_requests();
        let compactor_request = compactor_requests
            .get(cycle - 1)
            .expect("one compactor request per completed cycle");
        assert_eq!(
            compactor_request.generation().max_output_tokens(),
            Some(expected_output_ceiling)
        );
        let compactor_input =
            serde_json::to_string(compactor_request.input()).expect("compactor input serializes");
        assert!(
            !compactor_input.contains(&marker),
            "current user input must stay out of compaction input"
        );
        if cycle == 1 {
            assert!(compactor_input.contains(DEEP_SOURCE_SENTINEL));
        } else {
            assert!(compactor_input.contains("previous_checkpoint"));
        }

        let checkpoint_item = checkpoint_item(trigger_request);
        if let Some(previous) = &previous_cycle {
            assert_ne!(checkpoint_id, previous.checkpoint_id);
            assert_ne!(
                checkpoint_item, previous.checkpoint_item,
                "checkpoint bytes should change only when the rolling replacement installs"
            );
        }

        let transcript = runtime
            .session_transcript()
            .await
            .expect("full transcript reads");
        if let Some(previous) = &previous_cycle {
            assert!(
                transcript.starts_with(&previous.transcript),
                "full transcript must retain every prior source item"
            );
        }
        assert_eq!(transcript.len(), submitted_inputs.len() * 2);

        let boundary = prompt_boundary(&runtime).await;
        if let Some(previous) = &previous_cycle {
            assert!(boundary > previous.boundary);
        }
        assert_eq!(
            boundary.as_u64(),
            u64::try_from(submitted_inputs.len() - 6).expect("step count fits u64"),
            "a completed trigger turn follows the five completed turns retained at the boundary"
        );
        assert_provider_projection_has_six_raw_turns(&runtime, &submitted_inputs).await;
        assert_checkpoint_meaning(&runtime, &fixture.semantic_values, cycle).await;
        assert_current_refs_read_original_source(
            &runtime,
            first_source_text.as_deref().expect("first source exists"),
        )
        .await;

        let probe = format!("checkpoint stability probe {cycle}");
        let installed_cycle = CycleState {
            transcript,
            boundary,
            checkpoint_id,
            checkpoint_item,
        };
        let cycle_state = run_resume_probe(
            &runtime,
            &primary,
            window_tokens,
            policy,
            &probe,
            &installed_cycle,
            first_source_text.as_deref().expect("first source exists"),
        )
        .await;
        submitted_inputs.push(probe);
        previous_cycle = Some(cycle_state);
    }

    assert_eq!(compactor.recorded_requests().len(), 3);
    assert_eq!(completed_cycles, 3);
}

async fn run_resume_probe(
    runtime: &Runtime,
    primary: &RecordingModelProvider,
    window_tokens: u64,
    policy: CitationCompactionPolicy,
    probe: &str,
    installed_cycle: &CycleState,
    original_source: &str,
) -> CycleState {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    runtime
        .save_session_to(store.clone())
        .await
        .expect("cycle state saves");

    let resumed_primary = RecordingModelProvider::with_script_and_capabilities(
        vec![scripted_assistant_response(
            installed_cycle.transcript.len() / 2,
        )],
        primary_capabilities(window_tokens),
    );
    let resumed = Runtime::builder(runtime.session_id().clone())
        .model_provider(Arc::new(resumed_primary.clone()), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(RecordingModelProvider::new()),
            named_model("fake/rolling-compactor"),
        )
        .automatic_compaction(AutomaticCompactionConfig::enabled(policy))
        .resume_from_store(store)
        .await
        .expect("cycle state resumes");

    let request_count_before = primary.recorded_requests().len();
    let context_count_before = primary.recorded_contexts().len();
    let original_events = collect_step(runtime, probe, StepContext::default()).await;
    let resumed_events = collect_step(&resumed, probe, StepContext::default()).await;
    assert_step_completed_without_failure(&original_events);
    assert_step_completed_without_failure(&resumed_events);
    assert_no_compaction(&original_events);
    assert_no_compaction(&resumed_events);

    let original_requests = primary.recorded_requests();
    let original_request = original_requests
        .get(request_count_before)
        .expect("original probe request records");
    let resumed_requests = resumed_primary.recorded_requests();
    let resumed_request = resumed_requests
        .first()
        .expect("resumed probe request records");
    assert_eq!(original_request, resumed_request);
    assert!(
        original_request
            .input()
            .starts_with(primary.recorded_requests()[request_count_before - 1].input())
    );
    assert_eq!(
        checkpoint_item(original_request),
        installed_cycle.checkpoint_item
    );
    assert_eq!(
        checkpoint_item(resumed_request),
        installed_cycle.checkpoint_item
    );

    let original_contexts = primary.recorded_contexts();
    let resumed_contexts = resumed_primary.recorded_contexts();
    assert_eq!(
        original_contexts[context_count_before]
            .prompt_cache_key()
            .map(SessionId::as_str),
        resumed_contexts[0]
            .prompt_cache_key()
            .map(SessionId::as_str)
    );

    let transcript = runtime
        .session_transcript()
        .await
        .expect("original transcript reads after probe");
    let resumed_transcript = resumed
        .session_transcript()
        .await
        .expect("resumed transcript reads after probe");
    assert!(transcript.starts_with(&installed_cycle.transcript));
    assert_eq!(transcript.len(), installed_cycle.transcript.len() + 2);
    assert_eq!(transcript, resumed_transcript);
    assert_eq!(prompt_boundary(runtime).await, installed_cycle.boundary);
    assert_eq!(prompt_boundary(&resumed).await, installed_cycle.boundary);
    assert_eq!(
        runtime
            .compacted_checkpoint_summary()
            .await
            .and_then(|summary| summary.checkpoint_id().cloned()),
        Some(installed_cycle.checkpoint_id.clone())
    );
    assert_eq!(
        resumed
            .compacted_checkpoint_summary()
            .await
            .and_then(|summary| summary.checkpoint_id().cloned()),
        Some(installed_cycle.checkpoint_id.clone())
    );
    assert_current_refs_read_original_source(&resumed, original_source).await;

    CycleState {
        transcript,
        boundary: installed_cycle.boundary,
        checkpoint_id: installed_cycle.checkpoint_id.clone(),
        checkpoint_item: installed_cycle.checkpoint_item.clone(),
    }
}

fn primary_capabilities(window_tokens: u64) -> ModelCapabilities {
    ModelCapabilities::new(true, true, false, true, Some(window_tokens), Some(16))
        .expect("valid primary capabilities")
}

fn compactor_capabilities() -> ModelCapabilities {
    ModelCapabilities::new(true, true, false, true, Some(1_000_000), Some(32_000))
        .expect("valid compactor capabilities")
}

fn scripted_assistant_response(index: usize) -> ScriptedModelProviderResponse {
    ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
        vec![ModelOutput::text(&assistant_output(index))],
        FinishReason::Stop,
    ))])
}

fn assistant_output(index: usize) -> String {
    format!("assistant-result-{index}")
}

fn padded_text(prefix: &str, target_bytes: usize) -> String {
    assert!(prefix.len() < target_bytes);
    let mut text = String::with_capacity(target_bytes);
    text.push_str(prefix);
    text.push('\n');
    text.push_str(&"x".repeat(target_bytes - text.len()));
    assert_eq!(text.len(), target_bytes);
    text
}

fn completed_checkpoint_id(events: &[RuntimeJournalEvent]) -> Option<String> {
    events.iter().find_map(|event| match &event.payload {
        RuntimeJournalPayload::CompactionCompleted { checkpoint_id, .. } => {
            Some(checkpoint_id.clone())
        }
        _ => None,
    })
}

fn assert_step_completed_without_failure(events: &[RuntimeJournalEvent]) {
    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted))
    );
    assert!(events.iter().all(|event| {
        !matches!(
            event.payload,
            RuntimeJournalPayload::Failed { .. } | RuntimeJournalPayload::Cancelled { .. }
        )
    }));
}

fn assert_no_compaction(events: &[RuntimeJournalEvent]) {
    assert!(events.iter().all(|event| {
        !matches!(
            event.payload,
            RuntimeJournalPayload::CompactionStarted
                | RuntimeJournalPayload::CompactionCompleted { .. }
        )
    }));
}

fn checkpoint_item(request: &ModelRequest) -> ModelInputItem {
    let mut items = request.input().iter().filter(|item| {
        matches!(
            item,
            ModelInputItem::Message(message)
                if message.role() == ModelMessageRole::System
                    && message
                        .content()
                        .as_text()
                        .starts_with("<merry_checkpoint>\ncompacted-checkpoint:\n")
        )
    });
    let checkpoint = items
        .next()
        .expect("request contains one compacted checkpoint item")
        .clone();
    assert!(
        items.next().is_none(),
        "request must not contain both old and new checkpoint items"
    );
    checkpoint
}

fn assert_recent_raw_turns(request: &ModelRequest, submitted_inputs: &[String]) {
    assert!(submitted_inputs.len() >= 6);
    let body = request
        .input()
        .iter()
        .filter(|item| {
            !matches!(
                item,
                ModelInputItem::Message(message) if message.role() == ModelMessageRole::System
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    let expected_users = &submitted_inputs[submitted_inputs.len() - 6..];
    let first_global_index = submitted_inputs.len() - 6;
    let mut expected = Vec::with_capacity(11);
    for (offset, user) in expected_users[..5].iter().enumerate() {
        expected.push(text_message(ModelMessageRole::User, user));
        expected.push(text_message(
            ModelMessageRole::Assistant,
            &assistant_output(first_global_index + offset),
        ));
    }
    expected.push(text_message(
        ModelMessageRole::User,
        expected_users[5].as_str(),
    ));
    assert_eq!(body, expected);
}

fn text_message(role: ModelMessageRole, text: &str) -> ModelInputItem {
    ModelInputItem::Message(
        ModelMessage::new(
            role,
            ModelContent::text(text).expect("test message content is valid"),
        )
        .expect("test message is valid"),
    )
}

async fn prompt_boundary(runtime: &Runtime) -> ModelTurnId {
    runtime
        .inner
        .session
        .lock()
        .await
        .prompt_history_projection()
        .compacted_through()
        .expect("rolling compaction advances the prompt boundary")
}

async fn assert_provider_projection_has_six_raw_turns(
    runtime: &Runtime,
    submitted_inputs: &[String],
) {
    let session = runtime.inner.session.lock().await;
    let projection = session
        .provider_transcript_snapshot()
        .expect("provider transcript projects");
    assert_eq!(projection.len(), 12);
    let expected_users = &submitted_inputs[submitted_inputs.len() - 6..];
    let first_global_index = submitted_inputs.len() - 6;
    for (index, pair) in projection.chunks_exact(2).enumerate() {
        assert!(matches!(
            &pair[0],
            TranscriptItemSnapshot::UserMessage { text, .. } if text == &expected_users[index]
        ));
        assert!(matches!(
            &pair[1],
            TranscriptItemSnapshot::AssistantText { text }
                if text == &assistant_output(first_global_index + index)
        ));
    }
}

async fn assert_checkpoint_meaning(
    runtime: &Runtime,
    expected: &RollingSemanticValues,
    cycle: usize,
) {
    let snapshot = runtime.context_snapshot().await;
    let checkpoint = snapshot
        .compacted_checkpoint_for_tests()
        .expect("checkpoint exists");
    let citation = checkpoint
        .citation_backed()
        .expect("automatic checkpoint is citation backed");
    assert_eq!(citation.sections().entry_count(), EXPECTED_ENTRY_COUNT);
    for section in CheckpointSection::ALL {
        assert!(
            !citation.sections().entries(section).is_empty(),
            "checkpoint section {} must not be empty",
            section.as_str()
        );
    }

    assert_section_entry_with_rationale(
        citation.sections(),
        CheckpointSection::ConfirmedDecision,
        &expected.confirmed_decision,
        &expected.decision_reason,
    );
    assert_section_entry_with_rationale(
        citation.sections(),
        CheckpointSection::RejectedApproach,
        &expected.rejected_approach,
        &expected.rejection_reason,
    );
    for (section, exact) in [
        (
            CheckpointSection::ConstraintPreferenceBoundary,
            expected.constraint.as_str(),
        ),
        (
            CheckpointSection::CorrectedMisunderstanding,
            expected.correction.as_str(),
        ),
        (
            CheckpointSection::DurableConclusion,
            expected.durable_conclusion.as_str(),
        ),
        (
            CheckpointSection::DurableConclusion,
            expected.loss_conclusion.as_str(),
        ),
        (
            CheckpointSection::OpenQuestion,
            expected.open_question.as_str(),
        ),
        (
            CheckpointSection::CurrentProgressAndNextStep,
            expected.progress_generations[cycle - 1].as_str(),
        ),
        (
            CheckpointSection::CurrentProgressAndNextStep,
            expected.next_step.as_str(),
        ),
        (CheckpointSection::ExactDetail, expected.exact_path.as_str()),
        (
            CheckpointSection::ExactDetail,
            expected.exact_interface.as_str(),
        ),
    ] {
        assert_section_entry(citation.sections(), section, exact);
    }
    assert_section_entry(
        citation.sections(),
        CheckpointSection::ExactDetail,
        &expected.exact_number.to_string(),
    );

    let text = ContextCompiler::new()
        .compile(&snapshot)
        .expect("checkpoint context compiles")
        .to_snapshot();
    for exact in [
        expected.confirmed_decision.as_str(),
        expected.decision_reason.as_str(),
        expected.rejected_approach.as_str(),
        expected.rejection_reason.as_str(),
        expected.constraint.as_str(),
        expected.correction.as_str(),
        expected.durable_conclusion.as_str(),
        expected.loss_conclusion.as_str(),
        expected.open_question.as_str(),
        expected.progress_generations[cycle - 1].as_str(),
        expected.next_step.as_str(),
        expected.exact_path.as_str(),
        expected.exact_interface.as_str(),
    ] {
        assert!(text.contains(exact), "checkpoint lost exact value: {exact}");
    }
    assert!(text.contains(&expected.exact_number.to_string()));

    if cycle == 1 {
        assert!(citation.handoffs().is_empty());
    } else {
        assert_eq!(citation.handoffs().len(), EXPECTED_ENTRY_COUNT);
        assert_eq!(
            citation
                .handoffs()
                .iter()
                .filter(|handoff| handoff.action() == CheckpointHandoffAction::Replace)
                .count(),
            1
        );
        assert_eq!(
            citation
                .handoffs()
                .iter()
                .filter(|handoff| handoff.action() == CheckpointHandoffAction::Keep)
                .count(),
            EXPECTED_ENTRY_COUNT - 1
        );
    }
}

fn assert_section_entry(
    sections: &crate::CheckpointSections,
    section: CheckpointSection,
    expected: &str,
) {
    assert!(
        sections
            .entries(section)
            .iter()
            .any(|entry| entry.text() == expected),
        "checkpoint section {} lost exact value: {expected}",
        section.as_str()
    );
}

fn assert_section_entry_with_rationale(
    sections: &crate::CheckpointSections,
    section: CheckpointSection,
    expected_text: &str,
    expected_rationale: &str,
) {
    assert!(
        sections.entries(section).iter().any(|entry| {
            entry.text() == expected_text && entry.rationale() == Some(expected_rationale)
        }),
        "checkpoint section {} lost exact value or rationale: {expected_text}",
        section.as_str()
    );
}

async fn assert_current_refs_read_original_source(runtime: &Runtime, original: &str) {
    let snapshot = runtime.context_snapshot().await;
    let checkpoint = snapshot
        .compacted_checkpoint_for_tests()
        .and_then(crate::CompactedCheckpoint::citation_backed)
        .expect("citation checkpoint exists");
    assert_eq!(checkpoint.manifest().refs().len(), 1);
    let reference = &checkpoint.manifest().refs()[0];
    assert_eq!(reference.id().as_str(), "h0");
    let (artifact_id, content) = read_full_ref(runtime, reference.id()).await;
    assert!(artifact_id.as_str().starts_with("user-message-"));
    assert_eq!(content, original);
    assert!(content.contains(DEEP_SOURCE_SENTINEL));
}

async fn read_full_ref(runtime: &Runtime, ref_id: &crate::CheckpointRefId) -> (ArtifactId, String) {
    let mut offset = 0usize;
    let mut artifact_id = None;
    let mut content = String::new();
    loop {
        let page = runtime
            .read_checkpoint_ref_page(ref_id, offset, 4096)
            .await
            .expect("checkpoint ref page reads");
        match &artifact_id {
            Some(existing) => assert_eq!(existing, page.artifact_id()),
            None => artifact_id = Some(page.artifact_id().clone()),
        }
        assert_eq!(page.offset(), offset);
        content.push_str(page.content());
        match page.next_offset() {
            Some(next) => offset = next,
            None => {
                assert_eq!(content.len(), page.total_bytes());
                break;
            }
        }
    }
    (artifact_id.expect("ref has at least one page"), content)
}
