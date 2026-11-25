use crate::{
    CheckpointDecision, ContextBudget, bundled_model_context_window, decide_checkpoint,
    resolve_context_window,
};
use merry_core::ContextWindowSource;

#[test]
fn context_budget_uses_one_threshold_on_large_windows() {
    let budget = ContextBudget::from_window(1_000_000, 95, 120_000, 32_000)
        .expect("budget should calculate");

    assert_eq!(budget.effective_window_tokens(), 950_000);
    assert_eq!(budget.stable_prefix_tokens(), 120_000);
    assert_eq!(budget.output_reserve_tokens(), 32_000);
    assert_eq!(budget.body_budget_tokens(), 798_000);
    assert_eq!(budget.compact_at_tokens(), 678_300);
}

#[test]
fn context_budget_threshold_is_eighty_five_percent_of_the_body() {
    for (window, output_reserve, body_budget, compact_at) in [
        (64_000, 3_200, 57_600, 48_960),
        (128_000, 6_400, 115_200, 97_920),
        (256_000, 8_192, 235_008, 199_757),
        (512_000, 8_192, 478_208, 406_477),
        (1_000_000, 8_192, 941_808, 800_537),
    ] {
        let budget = ContextBudget::from_window(window, 95, 0, output_reserve)
            .expect("budget should calculate");
        assert_eq!(budget.body_budget_tokens(), body_budget);
        assert_eq!(budget.compact_at_tokens(), compact_at);
        assert_eq!(
            budget.compact_at_tokens(),
            budget.body_budget_tokens().saturating_mul(85).div_ceil(100)
        );
    }
}

#[test]
fn context_budget_keeps_the_same_ratio_when_the_body_is_small() {
    let budget =
        ContextBudget::from_window(64_000, 95, 54_000, 3_200).expect("budget should calculate");

    assert_eq!(budget.body_budget_tokens(), 3_600);
    assert_eq!(budget.compact_at_tokens(), 3_060);
}

#[test]
fn context_budget_rejects_invalid_percent_or_reserve() {
    assert!(ContextBudget::from_window(1_000_000, 0, 0, 32_000).is_err());
    assert!(ContextBudget::from_window(1_000, 95, 100, 1_000).is_err());
    assert!(ContextBudget::from_window(1_000, 95, 950, 1).is_err());
}

#[test]
fn context_window_resolver_prefers_explicit_config() {
    let resolved = resolve_context_window(Some(1_000_000), Some(200_000), Some(128_000), 64_000)
        .expect("window should resolve");

    assert_eq!(resolved.tokens(), 1_000_000);
    assert_eq!(resolved.source(), ContextWindowSource::ExplicitConfig);
}

#[test]
fn context_window_resolver_prefers_provider_then_catalog_before_fallback() {
    let provider = resolve_context_window(None, Some(200_000), Some(128_000), 64_000)
        .expect("provider window should resolve");
    let catalog = resolve_context_window(None, None, Some(128_000), 64_000)
        .expect("catalog window should resolve");

    assert_eq!(provider.tokens(), 200_000);
    assert_eq!(provider.source(), ContextWindowSource::ProviderCapabilities);
    assert_eq!(catalog.tokens(), 128_000);
    assert_eq!(catalog.source(), ContextWindowSource::BundledCatalog);
}

#[test]
fn context_window_resolver_falls_back_when_metadata_is_missing() {
    let resolved = resolve_context_window(None, None, None, 64_000).expect("window should resolve");

    assert_eq!(resolved.tokens(), 64_000);
    assert_eq!(resolved.source(), ContextWindowSource::Fallback);
}

#[test]
fn context_window_resolver_rejects_zero_values() {
    assert!(resolve_context_window(Some(0), Some(200_000), Some(128_000), 64_000).is_err());
    assert!(resolve_context_window(None, Some(0), Some(128_000), 64_000).is_err());
    assert!(resolve_context_window(None, None, Some(0), 64_000).is_err());
    assert!(resolve_context_window(None, None, None, 0).is_err());
}

#[test]
fn checkpoint_decision_uses_one_threshold() {
    let budget =
        ContextBudget::from_window(100_000, 90, 8_000, 10_000).expect("budget should calculate");

    assert_eq!(budget.body_budget_tokens(), 72_000);
    assert_eq!(budget.compact_at_tokens(), 61_200);
    assert_eq!(decide_checkpoint(1, budget), CheckpointDecision::Continue);
    assert_eq!(
        decide_checkpoint(budget.compact_at_tokens() - 1, budget),
        CheckpointDecision::Continue
    );
    assert_eq!(
        decide_checkpoint(budget.compact_at_tokens(), budget),
        CheckpointDecision::Compact
    );
}

#[test]
fn bundled_model_catalog_resolves_known_families_and_leaves_unknown_for_fallback() {
    assert_eq!(
        bundled_model_context_window("claude-sonnet-4-6"),
        Some(200_000)
    );
    assert_eq!(
        bundled_model_context_window("anthropic/claude-opus-4-6"),
        Some(200_000)
    );
    assert_eq!(bundled_model_context_window("gpt-4o"), Some(128_000));
    assert_eq!(
        bundled_model_context_window("openai/gpt-4.1"),
        Some(1_047_576)
    );
    assert_eq!(bundled_model_context_window("gpt-5"), Some(400_000));
    assert_eq!(bundled_model_context_window("o3-mini"), Some(200_000));
    assert_eq!(
        bundled_model_context_window("gemini-2.5-pro"),
        Some(1_048_576)
    );
    assert_eq!(bundled_model_context_window("unknown/custom-model"), None);
}
