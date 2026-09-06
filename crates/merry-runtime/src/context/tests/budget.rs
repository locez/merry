use crate::{
    CheckpointDecision, ContextBudget, ContextBudgetPolicy, decide_checkpoint,
    resolve_context_window,
};
use merry_core::ContextWindowSource;

#[test]
fn context_budget_balanced_uses_large_windows_without_step_count_compaction() {
    let budget = ContextBudget::from_window(
        1_000_000,
        95,
        120_000,
        32_000,
        ContextBudgetPolicy::Balanced,
    )
    .expect("budget should calculate");

    assert_eq!(budget.effective_window_tokens(), 950_000);
    assert_eq!(budget.stable_prefix_tokens(), 120_000);
    assert_eq!(budget.output_reserve_tokens(), 32_000);
    assert_eq!(budget.body_budget_tokens(), 798_000);
    assert_eq!(budget.soft_water_tokens(), 758_000);
    assert_eq!(budget.hard_water_tokens(), 788_000);
}

#[test]
fn context_budget_policy_watermarks_use_policy_rules() {
    let cost_aware =
        ContextBudget::from_window(100_000, 100, 1_000, 1_000, ContextBudgetPolicy::CostAware)
            .expect("cost-aware budget should calculate");
    let balanced =
        ContextBudget::from_window(100_000, 100, 1_000, 1_000, ContextBudgetPolicy::Balanced)
            .expect("balanced budget should calculate");
    let capacity =
        ContextBudget::from_window(100_000, 100, 1_000, 1_000, ContextBudgetPolicy::Capacity)
            .expect("capacity budget should calculate");

    assert_eq!(cost_aware.body_budget_tokens(), 98_000);
    assert_eq!(cost_aware.soft_water_tokens(), 58_800);
    assert_eq!(cost_aware.hard_water_tokens(), 78_400);
    assert_eq!(balanced.soft_water_tokens(), 88_000);
    assert_eq!(balanced.hard_water_tokens(), 96_000);
    assert_eq!(capacity.soft_water_tokens(), 93_000);
    assert_eq!(capacity.hard_water_tokens(), 97_000);
}

#[test]
fn context_budget_balanced_and_capacity_use_capped_window_headroom() {
    for (window, output_reserve, balanced_soft, balanced_hard, capacity_soft, capacity_hard) in [
        (64_000, 3_200, 47_600, 55_600, 52_600, 56_600),
        (128_000, 6_400, 105_200, 113_200, 110_200, 114_200),
        (256_000, 8_192, 224_448, 232_448, 228_608, 233_728),
        (512_000, 8_192, 457_728, 473_088, 465_408, 475_648),
        (1_000_000, 8_192, 901_808, 931_808, 916_808, 936_808),
    ] {
        let balanced = ContextBudget::from_window(
            window,
            95,
            0,
            output_reserve,
            ContextBudgetPolicy::Balanced,
        )
        .expect("balanced budget should calculate");
        let capacity = ContextBudget::from_window(
            window,
            95,
            0,
            output_reserve,
            ContextBudgetPolicy::Capacity,
        )
        .expect("capacity budget should calculate");

        assert_eq!(balanced.soft_water_tokens(), balanced_soft);
        assert_eq!(balanced.hard_water_tokens(), balanced_hard);
        assert_eq!(capacity.soft_water_tokens(), capacity_soft);
        assert_eq!(capacity.hard_water_tokens(), capacity_hard);
    }
}

#[test]
fn context_budget_caps_window_headroom_when_stable_prefix_leaves_tiny_body() {
    let balanced =
        ContextBudget::from_window(64_000, 95, 54_000, 3_200, ContextBudgetPolicy::Balanced)
            .expect("balanced budget should calculate");
    let capacity =
        ContextBudget::from_window(64_000, 95, 54_000, 3_200, ContextBudgetPolicy::Capacity)
            .expect("capacity budget should calculate");

    assert_eq!(balanced.body_budget_tokens(), 3_600);
    assert_eq!(balanced.soft_water_tokens(), 900);
    assert_eq!(balanced.hard_water_tokens(), 1_800);
    assert_eq!(capacity.soft_water_tokens(), 1_300);
    assert_eq!(capacity.hard_water_tokens(), 2_600);
}

#[test]
fn context_budget_rejects_invalid_percent_or_reserve() {
    assert!(
        ContextBudget::from_window(1_000_000, 0, 0, 32_000, ContextBudgetPolicy::Balanced).is_err()
    );
    assert!(
        ContextBudget::from_window(1_000, 95, 100, 1_000, ContextBudgetPolicy::Balanced).is_err()
    );
    assert!(ContextBudget::from_window(1_000, 95, 950, 1, ContextBudgetPolicy::Balanced).is_err());
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
fn checkpoint_decision_uses_watermarks_not_turn_counts() {
    let budget =
        ContextBudget::from_window(100_000, 90, 8_000, 10_000, ContextBudgetPolicy::Balanced)
            .expect("budget should calculate");

    assert_eq!(decide_checkpoint(1, budget), CheckpointDecision::Continue);
    assert_eq!(
        decide_checkpoint(budget.soft_water_tokens() - 1, budget),
        CheckpointDecision::Continue
    );
    assert_eq!(
        decide_checkpoint(budget.soft_water_tokens(), budget),
        CheckpointDecision::PlanCheckpoint
    );
    assert_eq!(
        decide_checkpoint(budget.hard_water_tokens() - 1, budget),
        CheckpointDecision::PlanCheckpoint
    );
    assert_eq!(
        decide_checkpoint(budget.hard_water_tokens(), budget),
        CheckpointDecision::RequireCheckpoint
    );
}
