use crate::{
    CheckpointDecision, ContextBudgetPolicy,
    runtime::{
        request_context_budget, step_usage_context_snapshot, tests::support::common::named_model,
    },
};
use merry_llm::{
    GenerationConfig, ModelCapabilities, ModelContent, ModelMessage, ModelMessageRole, ModelRequest,
};

#[test]
fn request_context_budget_uses_dynamic_estimate_watermarks() {
    let capabilities = ModelCapabilities::new(true, true, false, true, Some(100_000), Some(10_000))
        .expect("valid capabilities");
    let request = ModelRequest::new_with_continuations_and_stable_prefix(
        named_model("fake/budget-test"),
        vec![
            ModelMessage::new(
                ModelMessageRole::System,
                ModelContent::text("Base instructions.").expect("valid content"),
            )
            .expect("valid message"),
            ModelMessage::new(
                ModelMessageRole::User,
                ModelContent::text(&"a".repeat(320_000)).expect("valid content"),
            )
            .expect("valid message"),
        ],
        Vec::new(),
        Vec::new(),
        GenerationConfig::new(Some(10_000), false).expect("valid generation"),
        1,
    )
    .expect("valid request");

    let budget =
        request_context_budget(&capabilities, &request, None).expect("budget should calculate");

    assert_eq!(
        budget.window.source(),
        crate::ContextWindowSource::ProviderCapabilities
    );
    assert_eq!(budget.policy, ContextBudgetPolicy::Balanced);
    assert_eq!(budget.decision, CheckpointDecision::PlanCheckpoint);
    assert!(budget.dynamic_body_estimated_tokens >= budget.budget.soft_water_tokens());
    assert!(budget.dynamic_body_estimated_tokens < budget.budget.hard_water_tokens());

    let usage = step_usage_context_snapshot(Some(&budget), true);
    assert_eq!(
        usage
            .compaction
            .expect("compaction usage should be available")
            .dynamic_body_estimated_tokens,
        Some(budget.dynamic_body_estimated_tokens)
    );
}

#[test]
fn request_context_budget_derives_default_output_reserve_from_window() {
    let request = ModelRequest::new_with_continuations_and_stable_prefix(
        named_model("fake/default-output-reserve"),
        vec![
            ModelMessage::new(
                ModelMessageRole::User,
                ModelContent::text("Need budget.").expect("valid content"),
            )
            .expect("valid message"),
        ],
        Vec::new(),
        Vec::new(),
        GenerationConfig::default(),
        0,
    )
    .expect("valid request");

    for (window, expected_output_reserve) in [
        (32_000, 3_200),
        (64_000, 3_200),
        (128_000, 6_400),
        (256_000, 8_192),
        (512_000, 8_192),
        (1_000_000, 8_192),
        (2_000_000, 8_192),
    ] {
        let capabilities = ModelCapabilities::new(true, true, false, true, Some(window), None)
            .expect("valid capabilities");
        let budget =
            request_context_budget(&capabilities, &request, None).expect("budget should calculate");

        assert_eq!(
            budget.budget.output_reserve_tokens(),
            expected_output_reserve
        );
    }
}

#[test]
fn request_context_budget_uses_codex_style_fallback_for_unknown_models() {
    let capabilities =
        ModelCapabilities::new(true, true, false, true, None, None).expect("valid capabilities");
    let request = ModelRequest::new_with_continuations_and_stable_prefix(
        named_model("unknown/model"),
        vec![
            ModelMessage::new(
                ModelMessageRole::User,
                ModelContent::text("Need budget.").expect("valid content"),
            )
            .expect("valid message"),
        ],
        Vec::new(),
        Vec::new(),
        GenerationConfig::default(),
        0,
    )
    .expect("valid request");

    let budget =
        request_context_budget(&capabilities, &request, None).expect("budget should calculate");

    assert_eq!(budget.window.tokens(), 272_000);
    assert_eq!(budget.window.source(), crate::ContextWindowSource::Fallback);
    assert_eq!(budget.budget.effective_window_tokens(), 258_400);
}

#[test]
fn request_context_budget_prefers_an_explicit_window_override() {
    let capabilities = ModelCapabilities::new(true, true, false, true, Some(64_000), None)
        .expect("valid capabilities");
    let request = ModelRequest::new_with_continuations_and_stable_prefix(
        named_model("configured/model"),
        vec![
            ModelMessage::new(
                ModelMessageRole::User,
                ModelContent::text("Need budget.").expect("valid content"),
            )
            .expect("valid message"),
        ],
        Vec::new(),
        Vec::new(),
        GenerationConfig::default(),
        0,
    )
    .expect("valid request");

    let budget = request_context_budget(&capabilities, &request, Some(128_000))
        .expect("budget should calculate");

    assert_eq!(budget.window.tokens(), 128_000);
    assert_eq!(
        budget.window.source(),
        crate::ContextWindowSource::ExplicitConfig
    );
}
