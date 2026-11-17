use super::{evidence, test_window};
use crate::compaction::{
    CitationCompactionInput, CitationCompactionInputParts, CitationCompactionInputPolicy,
    CitationCompactionPolicy, CitationCompactionWindowBundle,
};
use crate::{
    checkpoint::{
        CheckpointId, CheckpointRef, CheckpointRefId, CheckpointRefManifest,
        CheckpointSequenceRange, CheckpointSourceKind,
    },
    compaction::compile_citation_compaction_model_request,
};
use merry_llm::{ModelContent, ModelInputItem, ModelMessage, ModelMessageRole, ModelName};

fn test_stable_prefix() -> Vec<ModelInputItem> {
    vec![ModelInputItem::Message(
        ModelMessage::new(
            ModelMessageRole::System,
            ModelContent::text("stable runtime instructions").expect("valid prefix text"),
        )
        .expect("valid system message"),
    )]
}

#[test]
fn payload_compaction_request_asks_for_prose_not_a_schema() {
    let policy = CitationCompactionPolicy::new(Some(128), Some(4096), 2).expect("valid policy");
    let checkpoint_id = CheckpointId::new("checkpoint-1").expect("valid checkpoint id");
    let manifest = CheckpointRefManifest::new(
        checkpoint_id,
        vec![CheckpointRef::new(
            CheckpointRefId::new("r1").expect("valid ref id"),
            CheckpointSourceKind::UserMessage,
            CheckpointSequenceRange::new(1, 1).expect("valid range"),
            evidence("user-message-1"),
        )],
    )
    .expect("valid manifest");
    let (window, plan) = test_window(1, "r1", "Need strict checkpoint JSON.");
    let input = CitationCompactionInput::new(
        CitationCompactionInputParts {
            input_policy: CitationCompactionInputPolicy::new(
                policy,
                policy.resolve(64_000).expect("test budget resolves"),
            ),
            task_anchor_snapshot: None,
            manifest,
        },
        CitationCompactionWindowBundle {
            covered_history_ids: [1].into_iter().collect(),
            window,
            window_plan: plan,
            archived_refs: Vec::new(),
        },
    );

    let request = compile_citation_compaction_model_request(
        &input,
        &ModelName::new("compaction-model").expect("valid model"),
        &crate::compaction::CompactionRequestSource::new(
            merry_llm::ModelRequest::new_with_input_and_stable_prefix(
                ModelName::new("compaction-model").expect("valid model"),
                test_stable_prefix(),
                Vec::new(),
                merry_llm::GenerationConfig::default(),
                1,
            )
            .expect("valid request"),
            &[],
            0,
        )
        .expect("valid source"),
        crate::compaction::CompactionRequestMode::Payload,
        Some(&merry_llm::ReasoningEffort::new("high").expect("valid reasoning effort")),
        input.resolved_budget().output_token_limit(),
    )
    .expect("compaction request compiles");

    assert!(request.response_format().is_none());
    let text = request
        .messages()
        .last()
        .expect("summary instruction")
        .content()
        .as_text();
    assert!(text.contains("1. Primary Request and Intent:"));
    assert!(text.contains("<merry_covered_history>"));
    assert!(text.contains("Need strict checkpoint JSON."));
    assert!(!text.contains("confirmed_decisions"));
    assert!(request.response_format().is_none());
}
