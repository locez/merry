use super::{
    CitationCompactionPolicy, CompactionError, CompactionReasoningReserve, CompactionWindowBudget,
    retained_turn_fallbacks, tightened_covered_budget,
};

#[test]
fn changing_retention_preserves_other_policy_settings() {
    let policy = CitationCompactionPolicy::new(Some(1024), Some(16000), 5)
        .expect("valid policy")
        .with_one_shot_retained_tool_exchanges(2)
        .with_retained_model_turns(3)
        .expect("valid retention");
    assert_eq!(policy.retained_model_turns(), 3);
    assert_eq!(policy.one_shot_retained_tool_exchanges(), 2);
    assert_eq!(policy.target_output_tokens(), Some(1024));
    assert_eq!(policy.max_accepted_output_bytes(), Some(16000));
}

#[test]
fn destination_window_bounds_summary_and_retained_history_independently() {
    for (window, summary_target, summary_limit, history_target) in [
        (8, 1, 1, 1),
        (64_000, 3_200, 9_600, 6_400),
        (128_000, 6_400, 19_200, 12_800),
        (272_000, 8_192, 20_480, 27_200),
        (1_000_000, 8_192, 20_480, 32_768),
        (2_000_000, 8_192, 20_480, 32_768),
    ] {
        let budget = CitationCompactionPolicy::default()
            .resolve(window)
            .expect("destination budget resolves");
        assert_eq!(budget.target_output_tokens(), summary_target);
        assert_eq!(budget.output_token_limit(), summary_limit);
        assert_eq!(budget.retained_history_token_target(), history_target);
    }
    let explicit = CitationCompactionPolicy::new(Some(9000), None, 5)
        .expect("valid policy")
        .resolve(64_000)
        .expect("destination budget resolves");
    assert_eq!(explicit.retained_history_token_target(), 6_400);
}

#[test]
fn hard_acceptance_covers_a_modest_overshoot_without_a_huge_window_percentage() {
    let observed_overshoot = 8_569;
    let compact = CitationCompactionPolicy::default()
        .resolve(128_000)
        .expect("128k budget resolves");
    assert!(compact.target_output_tokens() < observed_overshoot);
    assert!(compact.output_token_limit() >= observed_overshoot);
    assert_eq!(compact.retained_history_token_target(), 12_800);

    let huge = CitationCompactionPolicy::default()
        .resolve(2_000_000)
        .expect("2m budget resolves");
    assert_eq!(huge.target_output_tokens(), 8_192);
    assert_eq!(huge.output_token_limit(), 20_480);
    assert!(huge.output_token_limit() * 20 < 2_000_000);
}

#[test]
fn install_target_is_the_window_derived_body_not_half_the_watermark() {
    let resolved = CitationCompactionPolicy::default()
        .resolve(128_000)
        .expect("128k budget resolves");
    let hard_watermark = 102_400;
    let budget = CompactionWindowBudget::new(
        128_000,
        hard_watermark,
        2_000,
        2_000,
        resolved.output_token_limit(),
    )
    .expect("valid window budget")
    .with_retained_history_target(resolved.retained_history_token_target())
    .expect("valid history target");
    let expected = 2_000 + resolved.output_token_limit() + resolved.retained_history_token_target();
    assert_eq!(budget.target_dynamic_body_tokens(), expected);
    assert_ne!(budget.target_dynamic_body_tokens(), hard_watermark / 2);
    assert!(budget.target_dynamic_body_tokens() < hard_watermark);
}

#[test]
fn retained_turn_fallbacks_try_the_longest_complete_suffix() {
    assert_eq!(retained_turn_fallbacks(5, 8), vec![5, 4, 3, 2, 1]);
    assert_eq!(retained_turn_fallbacks(5, 3), vec![3, 2, 1]);
    assert_eq!(retained_turn_fallbacks(5, 0), Vec::<usize>::new());
}

#[test]
fn preferred_installation_budget_accounts_for_fixed_input_and_summary() {
    for (fixed_input, expected_target) in [(1_000, 9_500), (40_000, 48_500), (55_000, 56_000)] {
        let budget = CompactionWindowBudget::new(64_000, 56_000, fixed_input, fixed_input, 2_100)
            .expect("valid window budget")
            .with_retained_history_target(6_400)
            .expect("valid history target");
        assert_eq!(
            budget
                .preferred()
                .expect("preferred budget")
                .max_dynamic_body_tokens(),
            expected_target,
        );
        assert_eq!(budget.max_dynamic_body_tokens(), 56_000);
    }
    assert_eq!(
        CompactionWindowBudget::new(64_000, 56_000, u64::MAX, 0, 2_100)
            .expect("valid base budget")
            .with_retained_history_target(6_400),
        Err(CompactionError::BudgetOverflow),
    );
}

#[test]
fn summary_target_is_bounded_independently_of_reasoning_output() {
    let policy = CitationCompactionPolicy::default();
    for window in [64_000, 272_000, 1_000_000, 2_000_000] {
        let budget = policy.resolve(window).expect("valid budget");
        assert!(budget.target_output_tokens() <= 8192);
        assert!(budget.output_token_limit() <= 20_480);
        assert!(budget.output_token_limit() <= window * 15 / 100);
        assert!(budget.target_output_tokens() < budget.output_token_limit());
        assert!(
            CompactionReasoningReserve::INITIAL.output_ceiling(budget, window, window / 2)
                > budget.output_token_limit()
        );
    }
}

/// Compaction output ceiling for `window` at `input_tokens`.
fn ceiling(reserve: CompactionReasoningReserve, window: u64, input_tokens: u64) -> u64 {
    let resolved = CitationCompactionPolicy::default()
        .resolve(window)
        .expect("budget resolves");
    reserve.output_ceiling(resolved, window, input_tokens)
}

/// Numbers below come from the session that exposed the starvation.
///
/// The compaction model window resolved to 272,000 tokens, the request measured
/// 200,387 input tokens, and the provider truncated at the 43,520-token ceiling —
/// exactly twice the checkpoint text budget — with 43,518 of those tokens spent
/// on reasoning. The reserve must therefore grow with the request, not with the
/// text budget.
#[test]
fn reasoning_reserve_grows_with_request_input_instead_of_the_text_budget() {
    let resolved = CitationCompactionPolicy::default()
        .resolve(272_000)
        .expect("budget resolves");
    let text_budget = resolved.output_token_limit();
    let measured_input_tokens = 220_000;

    assert_eq!(text_budget, 20_480);
    let ceiling = CompactionReasoningReserve::INITIAL.output_ceiling(
        resolved,
        272_000,
        measured_input_tokens,
    );
    assert!(
        ceiling > 2 * text_budget,
        "the reserve must exceed the old text-budget multiple, got {ceiling}"
    );
    // The reserve alone can overshoot the window, which is why the runtime's
    // fitter covers less history before it sends the request.
    let mut fitted_input_tokens = measured_input_tokens;
    while fitted_input_tokens
        + CompactionReasoningReserve::INITIAL.output_ceiling(resolved, 272_000, fitted_input_tokens)
        > 272_000
    {
        fitted_input_tokens -= fitted_input_tokens / 100;
    }
    assert!(
        fitted_input_tokens < measured_input_tokens,
        "this window needs a smaller covered window before it can host the reserve"
    );
    assert!(
        fitted_input_tokens
            + CompactionReasoningReserve::INITIAL.output_ceiling(
                resolved,
                272_000,
                fitted_input_tokens
            )
            <= 272_000
    );

    let degraded = CompactionReasoningReserve::INITIAL.degraded();
    assert!(
        degraded.output_ceiling(resolved, 272_000, measured_input_tokens) > ceiling,
        "a truncated attempt must retry with a strictly larger reserve"
    );
}

#[test]
fn adaptive_budget_scales_for_64k_and_256k_windows() {
    let policy = CitationCompactionPolicy::default();

    assert_eq!(
        policy
            .resolve(64_000)
            .expect("64k budget resolves")
            .output_token_limit(),
        9_600
    );
    assert_eq!(
        policy
            .resolve(256_000)
            .expect("256k budget resolves")
            .output_token_limit(),
        20_480
    );
}

#[test]
fn adaptive_budget_clamps_low_and_high_windows() {
    let policy = CitationCompactionPolicy::default();

    assert_eq!(
        policy
            .resolve(8_000)
            .expect("low budget resolves")
            .output_token_limit(),
        1_000
    );
    assert_eq!(
        policy
            .resolve(1_000_000)
            .expect("high budget resolves")
            .output_token_limit(),
        20_480
    );
}

#[test]
fn explicit_output_limit_overrides_adaptive_ceiling() {
    let policy =
        CitationCompactionPolicy::new(Some(9_000), None, 5).expect("valid override policy");
    let budget = policy.resolve(64_000).expect("override budget resolves");

    assert_eq!(budget.target_output_tokens(), 3_200);
    assert_eq!(budget.output_token_limit(), 9_000);
    assert_eq!(budget.max_accepted_output_bytes(), 72_000);
}

#[test]
fn adaptive_budget_rejects_zero_and_overflow() {
    assert_eq!(
        CitationCompactionPolicy::new(Some(0), None, 5),
        Err(CompactionError::InvalidPolicy {
            field: "target_output_tokens"
        })
    );
    assert_eq!(
        CitationCompactionPolicy::new(None, Some(0), 5),
        Err(CompactionError::InvalidPolicy {
            field: "max_accepted_output_bytes"
        })
    );
    assert_eq!(
        CitationCompactionPolicy::new(None, None, 0),
        Err(CompactionError::InvalidPolicy {
            field: "retained_model_turns"
        })
    );
    assert_eq!(
        CitationCompactionPolicy::default().resolve(0),
        Err(CompactionError::InvalidPolicy {
            field: "primary_window_tokens"
        })
    );
    assert_eq!(
        CitationCompactionPolicy::default().resolve(u64::MAX),
        Err(CompactionError::BudgetOverflow)
    );
    assert_eq!(
        CitationCompactionPolicy::new(Some(u64::MAX), None, 5)
            .expect("override is structurally valid")
            .resolve(64_000),
        Err(CompactionError::BudgetOverflow)
    );
}

/// Numbers from the session that exposed the collapsing retry.
///
/// The compaction window was 272,000 tokens, the checkpoint text budget
/// 21,760, the covered payload 359,176, and the fitted first attempt measured
/// 397,849 input tokens. At a doubled reserve the old arithmetic gave up
/// 433,166 tokens of history and collapsed coverage to zero, which degraded a
/// recoverable truncation into a failed step.
#[test]
fn proportional_reserve_refit_keeps_a_usable_covered_window() {
    let window = 272_000;
    let text_budget = 21_760;
    let covered_payload = 359_176;
    let measured_input = 397_849;
    let reserve = CompactionReasoningReserve::INITIAL.degraded();

    assert_eq!(reserve.percent(), 50);
    let allowed_input = reserve.allowed_input_tokens(window, text_budget);
    let tightened = tightened_covered_budget(covered_payload, measured_input, allowed_input)
        .expect("a proportional refit must keep some covered window");

    assert!(
        tightened > 0,
        "the refit must not collapse coverage to zero"
    );
    let projected_input = measured_input - (covered_payload - tightened);
    let projected_output = ceiling(reserve, window, projected_input);
    assert!(
        projected_input + projected_output <= window,
        "refitted request must fit the window: input {projected_input} plus output {projected_output}"
    );
}

#[test]
fn reserve_shrinks_the_input_budget_monotonically() {
    let window = 272_000;
    let text_budget = 21_760;

    let initial = CompactionReasoningReserve::INITIAL.allowed_input_tokens(window, text_budget);
    let degraded = CompactionReasoningReserve::INITIAL
        .degraded()
        .allowed_input_tokens(window, text_budget);
    assert!(
        degraded < initial,
        "a larger reserve must leave room for less input: {initial} then {degraded}"
    );
}

/// A window that cannot host the text budget admits no covered history.
#[test]
fn window_smaller_than_the_text_budget_admits_no_input() {
    assert_eq!(
        CompactionReasoningReserve::INITIAL.allowed_input_tokens(16_000, 21_760),
        0
    );
}

/// Numbers from the session that looped between truncation and archive-only.
///
/// The window was 272,000 tokens, the checkpoint text budget 21,760, the covered
/// payload 773,342, and the first fitted request measured 798,687 input tokens.
/// The old refit gave up 1.25x the excess input, collapsed coverage to zero, and
/// degraded to archive-only, which never replaced the checkpoint: the session
/// stayed at ~236k tokens and re-ran compaction every couple of minutes.
#[test]
fn refit_keeps_coverage_when_the_window_cannot_host_the_full_request() {
    let window = 272_000;
    let resolved = CitationCompactionPolicy::default()
        .resolve(window)
        .expect("budget resolves");
    let text_budget = resolved.output_token_limit();
    let covered_payload = 773_342;
    let measured_input = 798_687;

    let allowed_input =
        CompactionReasoningReserve::INITIAL.allowed_input_tokens(window, text_budget);
    let tightened = tightened_covered_budget(covered_payload, measured_input, allowed_input)
        .expect("the refit must keep a covered window");
    assert!(
        tightened > 0,
        "the refit must not collapse coverage to zero"
    );
    assert!(
        tightened >= covered_payload / 8,
        "the refit must keep a useful share of the covered history, kept {tightened} of {covered_payload}"
    );

    let projected_input = measured_input - (covered_payload - tightened);
    let projected_output =
        CompactionReasoningReserve::INITIAL.output_ceiling(resolved, window, projected_input);
    assert!(
        projected_input + projected_output <= window,
        "the refitted request must fit: input {projected_input} plus output {projected_output}"
    );
}

/// A small request still gets a usable ceiling, because reasoning does not shrink.
///
/// The same session truncated at a 34,022-token ceiling for a 49,051-token input
/// and at 44,337 for 90,308, while 59,624 and 66,956 finished larger requests.
/// The floor keeps small requests above that unreliable range.
#[test]
fn small_requests_still_receive_a_usable_output_ceiling() {
    let window = 272_000;
    let resolved = CitationCompactionPolicy::default()
        .resolve(window)
        .expect("budget resolves");
    let text_budget = resolved.output_token_limit();

    for input in [49_051, 90_308] {
        let ceiling = CompactionReasoningReserve::INITIAL.output_ceiling(resolved, window, input);
        assert!(
            ceiling >= 3 * text_budget,
            "ceiling {ceiling} for input {input} must clear the observed truncation range"
        );
        assert!(
            input + ceiling <= window,
            "the floored ceiling must still fit the window: input {input} plus output {ceiling}"
        );
    }
}

/// The floor and the share agree with the allowed-input solver.
#[test]
fn allowed_input_matches_the_ceiling_it_promises() {
    let window = 272_000;
    let resolved = CitationCompactionPolicy::default()
        .resolve(window)
        .expect("budget resolves");
    let text_budget = resolved.output_token_limit();

    for reserve in [
        CompactionReasoningReserve::INITIAL,
        CompactionReasoningReserve::INITIAL.degraded(),
    ] {
        let allowed = reserve.allowed_input_tokens(window, text_budget);
        let ceiling = reserve.output_ceiling(resolved, window, allowed);
        assert!(
            allowed + ceiling <= window,
            "allowed input {allowed} must fit with ceiling {ceiling}"
        );
        // Integer division leaves a token of rounding slack, so the solver must
        // simply not waste meaningful headroom beyond that.
        let next = allowed + 4;
        assert!(
            next + reserve.output_ceiling(resolved, window, next) > window,
            "allowed input {allowed} leaves room for input {next}"
        );
    }
}
