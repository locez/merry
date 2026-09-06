use crate::{
    CompactionError,
    compaction::{CompactionPreparation, CompactionWindowBudget, retained_turn_fallbacks},
    context::compacted_checkpoint_wrapper_token_ceiling,
    session::tests::{
        RuntimeError, SessionId, SessionState,
        rolling_compaction::{
            policy, record_completed_tool_turn, record_completed_user_turn, window_budget,
        },
        tool_call_id,
    },
};

#[test]
fn default_plan_keeps_latest_five_completed_turns_raw() {
    let mut session =
        SessionState::new(SessionId::new("rolling-default-five").expect("valid session id"));
    for turn in 1..=8 {
        record_completed_user_turn(&mut session, &format!("turn {turn}"));
    }

    let plan = session
        .plan_compaction_window(policy(5), window_budget(10_000))
        .expect("plan succeeds")
        .expect("old prefix is compressible");

    assert_eq!(plan.covered_turn_ids_u64(), vec![1, 2, 3]);
    assert_eq!(plan.retained_turn_ids_u64(), vec![4, 5, 6, 7, 8]);
}

#[test]
fn oversized_tail_archives_oldest_tool_result_before_reducing_turn_count() {
    let mut session =
        SessionState::new(SessionId::new("rolling-archive-tools").expect("valid session id"));
    record_completed_user_turn(&mut session, "old prefix to compact");
    for turn in 1..=5 {
        record_completed_tool_turn(
            &mut session,
            &format!("call-{turn}"),
            &format!("result-{turn}"),
            &"x".repeat(1_000),
        );
    }

    let plan = session
        .plan_compaction_window(policy(5), window_budget(1_300))
        .expect("plan succeeds")
        .expect("old prefix is compressible");

    assert_eq!(plan.retained_turn_ids_u64(), vec![2, 3, 4, 5, 6]);
    assert_eq!(
        plan.archived_tool_call_ids_for_tests(),
        vec![tool_call_id("call-1")]
    );
}

#[test]
fn planner_falls_back_from_five_to_three_then_one_completed_turn() {
    let mut session =
        SessionState::new(SessionId::new("rolling-fallback").expect("valid session id"));
    for turn in 1..=8 {
        record_completed_user_turn(&mut session, &format!("turn-{turn}-{}", "x".repeat(396)));
    }

    let three = session
        .plan_compaction_window(policy(5), window_budget(450))
        .expect("three-turn plan succeeds")
        .expect("old prefix is compressible");
    assert_eq!(three.retained_turn_ids_u64(), vec![6, 7, 8]);

    let one = session
        .plan_compaction_window(policy(5), window_budget(250))
        .expect("one-turn plan succeeds")
        .expect("old prefix is compressible");
    assert_eq!(one.retained_turn_ids_u64(), vec![8]);
}

#[test]
fn compaction_input_contains_fact_after_1200_bytes() {
    let mut session =
        SessionState::new(SessionId::new("rolling-full-payload").expect("valid session id"));
    record_completed_user_turn(&mut session, &format!("{}EXACT-END", "x".repeat(1_400)));
    for turn in 1..=5 {
        record_completed_user_turn(&mut session, &format!("retained {turn}"));
    }

    let input = session
        .build_citation_compaction_input_with_window_budget(
            policy(5),
            policy(5).resolve(64_000).expect("budget resolves"),
            window_budget(10_000),
        )
        .expect("input builds")
        .expect("old prefix is compressible");

    assert!(
        input
            .to_model_payload_json()
            .expect("payload serializes")
            .contains("EXACT-END")
    );
}

#[test]
fn planner_reports_uncompressible_fixed_input_and_minimum_raw_turn() {
    let mut session =
        SessionState::new(SessionId::new("rolling-errors").expect("valid session id"));
    record_completed_user_turn(&mut session, "old prefix");
    record_completed_user_turn(&mut session, &"x".repeat(1_000));

    let fixed_error = session
        .plan_compaction_window(
            policy(1),
            CompactionWindowBudget::new(64_000, 200, 100, 100, 128).expect("valid budget"),
        )
        .expect_err("fixed input and checkpoint cannot fit");
    assert!(matches!(
        fixed_error,
        RuntimeError::Compaction {
            source: CompactionError::UncompressibleCurrentInput
        }
    ));

    let minimum_error = session
        .plan_compaction_window(policy(1), window_budget(200))
        .expect_err("one raw completed turn cannot fit");
    assert!(matches!(
        minimum_error,
        RuntimeError::Compaction {
            source: CompactionError::MinimumRawTurnCannotFit
        }
    ));
}

#[test]
fn exactly_five_completed_turns_that_fit_need_no_preparation() {
    let mut session =
        SessionState::new(SessionId::new("rolling-exact-five").expect("valid session id"));
    for turn in 1..=5 {
        record_completed_user_turn(&mut session, &format!("turn {turn}"));
    }

    let preparation = session
        .build_compaction_preparation_with_window_budget(
            policy(5),
            policy(5).resolve(64_000).expect("budget resolves"),
            window_budget(10_000),
        )
        .expect("preparation succeeds");

    assert!(preparation.is_none());
}

#[test]
fn exactly_five_large_tool_turns_use_archive_only_without_dropping_turns() {
    let mut session =
        SessionState::new(SessionId::new("rolling-exact-five-tools").expect("valid session id"));
    for turn in 1..=5 {
        record_completed_tool_turn(
            &mut session,
            &format!("exact-five-call-{turn}"),
            &format!("exact-five-result-{turn}"),
            &"x".repeat(1_000),
        );
    }

    let preparation = session
        .build_compaction_preparation_with_window_budget(
            policy(5),
            policy(5).resolve(64_000).expect("budget resolves"),
            window_budget(1_300),
        )
        .expect("preparation succeeds")
        .expect("archive-only preparation is required");
    let CompactionPreparation::ArchiveToolResults(input) = preparation else {
        panic!("exactly five turns must not replace the checkpoint");
    };

    assert_eq!(
        input.window_plan().covered_turn_ids_u64(),
        Vec::<u64>::new()
    );
    assert_eq!(
        input.window_plan().retained_turn_ids_u64(),
        vec![1, 2, 3, 4, 5]
    );
    assert_eq!(
        input.window_plan().archived_tool_call_ids_for_tests(),
        vec![tool_call_id("exact-five-call-1")]
    );
}

#[test]
fn retained_turn_fallbacks_keep_configured_order_for_seven_and_three() {
    assert_eq!(retained_turn_fallbacks(7, 9), vec![7, 5, 3, 1]);
    assert_eq!(retained_turn_fallbacks(3, 9), vec![3, 1]);
}

#[test]
fn configured_five_with_two_small_completed_turns_needs_no_preparation() {
    let mut session =
        SessionState::new(SessionId::new("rolling-five-config-two-small").expect("valid id"));
    record_completed_user_turn(&mut session, "small one");
    record_completed_user_turn(&mut session, "small two");

    let preparation = session
        .build_compaction_preparation_with_window_budget(
            policy(5),
            policy(5).resolve(64_000).expect("budget resolves"),
            window_budget(10_000),
        )
        .expect("preparation builds");
    assert!(preparation.is_none());
}

#[test]
fn configured_five_with_two_large_tool_turns_archives_without_dropping_one() {
    let mut session =
        SessionState::new(SessionId::new("rolling-five-config-two-tools").expect("valid id"));
    for turn in 1..=2 {
        record_completed_tool_turn(
            &mut session,
            &format!("two-tool-call-{turn}"),
            &format!("two-tool-result-{turn}"),
            &"x".repeat(1_000),
        );
    }

    let preparation = session
        .build_compaction_preparation_with_window_budget(
            policy(5),
            policy(5).resolve(64_000).expect("budget resolves"),
            window_budget(400),
        )
        .expect("preparation builds")
        .expect("archive-only is required");
    let CompactionPreparation::ArchiveToolResults(input) = preparation else {
        panic!("two available turns must not fall back to retaining one");
    };
    assert_eq!(input.window_plan().retained_turn_ids_u64(), vec![1, 2]);
    assert_eq!(
        input.window_plan().archived_tool_call_ids_for_tests(),
        vec![tool_call_id("two-tool-call-1")]
    );
}

#[test]
fn completed_retention_keeps_newer_aborted_and_open_turns_without_counting_them() {
    let mut session =
        SessionState::new(SessionId::new("rolling-mixed-turn-status").expect("valid session id"));
    let aborted_one = session.begin_model_turn().expect("aborted turn begins");
    session
        .record_user_message_body(aborted_one, "aborted one")
        .expect("aborted item records");
    session
        .abort_model_turn(aborted_one)
        .expect("first turn aborts");
    record_completed_user_turn(&mut session, "completed two");
    record_completed_user_turn(&mut session, "completed three");
    let aborted_four = session.begin_model_turn().expect("aborted turn begins");
    session
        .record_user_message_body(aborted_four, "aborted four")
        .expect("aborted item records");
    session
        .abort_model_turn(aborted_four)
        .expect("fourth turn aborts");
    record_completed_user_turn(&mut session, "completed five");
    let open_six = session.begin_model_turn().expect("open turn begins");
    session
        .record_user_message_body(open_six, "open six")
        .expect("open item records");

    let plan = session
        .plan_compaction_window(policy(2), window_budget(10_000))
        .expect("plan succeeds")
        .expect("old prefix is compressible");

    assert_eq!(plan.covered_turn_ids_u64(), vec![1, 2]);
    assert_eq!(plan.retained_turn_ids_u64(), vec![3, 4, 5, 6]);
}

#[test]
fn completed_turn_after_open_turn_is_rejected_as_stale() {
    let mut session =
        SessionState::new(SessionId::new("rolling-terminal-after-open").expect("valid id"));
    record_completed_user_turn(&mut session, "completed one");
    let open = session.begin_model_turn().expect("open turn begins");
    session
        .record_user_message_body(open, "open two")
        .expect("open item records");
    record_completed_user_turn(&mut session, "completed three");

    let error = session
        .plan_compaction_window(policy(1), window_budget(10_000))
        .expect_err("terminal turn after open turn is stale");
    assert!(matches!(
        error,
        RuntimeError::Compaction {
            source: CompactionError::StaleWindow
        }
    ));
}

#[test]
fn checkpoint_wrapper_tokens_are_part_of_the_fit_boundary() {
    let mut session =
        SessionState::new(SessionId::new("rolling-checkpoint-wrapper").expect("valid id"));
    record_completed_user_turn(&mut session, "old prefix");
    record_completed_user_turn(&mut session, "x");

    let output_limit = 128;
    let hard_watermark = output_limit + 2;
    let without_wrapper = session
        .plan_compaction_window(
            policy(1),
            CompactionWindowBudget::new(64_000, hard_watermark, 0, 0, output_limit)
                .expect("valid budget"),
        )
        .expect("plan without wrapper computes");
    assert!(without_wrapper.is_some(), "output plus one raw token fits");

    let with_wrapper = session
        .plan_compaction_window(
            policy(1),
            CompactionWindowBudget::new(
                64_000,
                hard_watermark,
                0,
                0,
                output_limit + compacted_checkpoint_wrapper_token_ceiling(),
            )
            .expect("valid budget"),
        )
        .expect_err("wrapper overhead crosses the hard watermark");
    assert!(matches!(
        with_wrapper,
        RuntimeError::Compaction {
            source: CompactionError::UncompressibleCurrentInput
        }
    ));
}

#[test]
fn fit_requires_strictly_less_than_the_hard_watermark() {
    let mut session =
        SessionState::new(SessionId::new("rolling-strict-hard-water").expect("valid id"));
    record_completed_user_turn(&mut session, "old prefix");
    record_completed_user_turn(&mut session, "x");

    let error = session
        .plan_compaction_window(
            policy(1),
            CompactionWindowBudget::new(64_000, 129, 0, 0, 128).expect("valid equality budget"),
        )
        .expect_err("equality with the hard watermark is not a fit");
    assert!(matches!(
        error,
        RuntimeError::Compaction {
            source: CompactionError::MinimumRawTurnCannotFit
        }
    ));
}
