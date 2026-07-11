use super::{CitationCompactionPolicy, CompactionError};

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
