use super::{
    CitationCompactionPolicy, CompactionError, CompactionReasoningReserve,
    allowed_input_tokens_for_window, tightened_covered_budget,
};

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
    let measured_input_tokens = 200_387;

    assert_eq!(text_budget, 21_760);
    let ceiling =
        CompactionReasoningReserve::INITIAL.output_ceiling(resolved, measured_input_tokens);
    assert!(
        ceiling > 2 * text_budget,
        "the reserve must exceed the old text-budget multiple, got {ceiling}"
    );
    // The reserve alone can overshoot the window, which is why the runtime's
    // fitter covers less history before it sends the request.
    let mut fitted_input_tokens = measured_input_tokens;
    while fitted_input_tokens
        + CompactionReasoningReserve::INITIAL.output_ceiling(resolved, fitted_input_tokens)
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
            + CompactionReasoningReserve::INITIAL.output_ceiling(resolved, fitted_input_tokens)
            <= 272_000
    );

    let degraded = CompactionReasoningReserve::INITIAL.degraded();
    assert!(
        degraded.output_ceiling(resolved, measured_input_tokens) > ceiling,
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
        5_120
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
        2_048
    );
    assert_eq!(
        policy
            .resolve(1_000_000)
            .expect("high budget resolves")
            .output_token_limit(),
        32_768
    );
}

#[test]
fn explicit_output_limit_overrides_adaptive_ceiling() {
    let policy =
        CitationCompactionPolicy::new(Some(9_000), None, 5).expect("valid override policy");
    let budget = policy.resolve(64_000).expect("override budget resolves");

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
    let allowed_input = allowed_input_tokens_for_window(window, text_budget, reserve);
    let tightened = tightened_covered_budget(covered_payload, measured_input, allowed_input)
        .expect("a proportional refit must keep some covered window");

    assert!(
        tightened > 0,
        "the refit must not collapse coverage to zero"
    );
    let projected_input = measured_input - (covered_payload - tightened);
    let projected_output = reserve.output_ceiling(
        CitationCompactionPolicy::default()
            .resolve(window)
            .expect("budget resolves"),
        projected_input,
    );
    assert!(
        projected_input + projected_output <= window,
        "refitted request must fit the window: input {projected_input} plus output {projected_output}"
    );
}

#[test]
fn reserve_shrinks_the_input_budget_monotonically() {
    let window = 272_000;
    let text_budget = 21_760;

    let initial =
        allowed_input_tokens_for_window(window, text_budget, CompactionReasoningReserve::INITIAL);
    let degraded = allowed_input_tokens_for_window(
        window,
        text_budget,
        CompactionReasoningReserve::INITIAL.degraded(),
    );
    assert!(
        degraded < initial,
        "a larger reserve must leave room for less input: {initial} then {degraded}"
    );
}

/// A window that cannot host the text budget admits no covered history.
#[test]
fn window_smaller_than_the_text_budget_admits_no_input() {
    assert_eq!(
        allowed_input_tokens_for_window(16_000, 21_760, CompactionReasoningReserve::INITIAL),
        0
    );
}
