use crate::{
    CompactionError,
    compaction::{
        CompactionCoverageBudget, CompactionPreparation, CompactionWindowBudget,
        retained_turn_fallbacks,
    },
    context::compacted_checkpoint_wrapper_token_ceiling,
    session::tests::{
        RuntimeError, SessionId, SessionState,
        rolling_compaction::{
            policy, record_completed_tool_turn, record_completed_user_turn, window_budget,
        },
        tool_call_id,
    },
};

/// A one-shot pass covers the whole history and shortens all but the newest exchanges.
///
/// Rolling cannot cover this much in one request because every covered tool result
/// travels at full length, so the payload grows with the number of tool calls. The
/// one-shot shape keeps the newest exchanges full and sends the older ones as
/// notices, which is what lets a window that shrank far below the history reduce it
/// in one pass. Call and result stay together in every exchange, because the runtime
/// rejects a window that carries only one of the pair.
#[test]
fn one_shot_covers_everything_and_shortens_all_but_the_newest_tool_exchanges() {
    let mut session =
        SessionState::new(SessionId::new("one-shot-payload").expect("valid session id"));
    for turn in 1..=4 {
        record_completed_tool_turn(
            &mut session,
            &format!("one-shot-call-{turn}"),
            &format!("one-shot-result-{turn}"),
            &format!("result body {turn} {}", "x".repeat(4_000)),
        );
    }

    let preparation = session
        .build_one_shot_compaction_preparation(
            policy(1),
            policy(1).resolve(64_000).expect("budget resolves"),
            window_budget(10_000),
            CompactionCoverageBudget::unbounded(),
            1,
        )
        .expect("preparation succeeds")
        .expect("the one-shot pass replaces the checkpoint");
    let CompactionPreparation::ReplaceCheckpoint(input) = preparation else {
        panic!("a one-shot pass must replace the checkpoint rather than archive");
    };

    // The pass covers everything before the retained tail: turns 1 to 3.
    assert_eq!(input.window_plan().covered_turn_ids_u64(), vec![1, 2, 3]);
    assert_eq!(input.window_plan().retained_turn_ids_u64(), vec![4]);

    let payload: serde_json::Value =
        serde_json::from_str(&input.to_model_payload_json().expect("payload serializes"))
            .expect("payload parses");
    let covered_exchanges = payload["window"]
        .as_array()
        .expect("window is an array")
        .iter()
        .flat_map(|turn| turn["items"].as_array().expect("items are an array"))
        .filter(|item| item["role"] == "tool_exchange")
        .collect::<Vec<_>>();
    assert_eq!(
        covered_exchanges.len(),
        1,
        "only the newest covered exchange travels through the payload"
    );
    let retained = covered_exchanges[0];
    assert_eq!(
        retained["result"]["artifact_id"].as_str(),
        Some("one-shot-result-3"),
        "the retained exchange is the newest covered one"
    );
    assert!(
        retained["result"]["content"]
            .as_str()
            .is_some_and(|content| content.contains("result body 3")),
        "the retained exchange keeps its result"
    );

    // The omitted exchanges leave nothing behind: no exchange item, no artifact id,
    // no marker. Measured on a real session, covered call arguments alone were
    // 225,800 tokens against a 190,400 token input budget, and a minimal marker per
    // exchange still cost about 95,000 tokens across 1,324 exchanges, so the payload
    // could not fit while any per-exchange content remained.
    let payload_text = input.to_model_payload_json().expect("payload serializes");
    for omitted in 1..=2 {
        assert!(
            !payload_text.contains(&format!("one-shot-result-{omitted}")),
            "omitted exchange {omitted} must not name its artifact either"
        );
    }
    assert!(
        covered_exchanges.iter().all(|item| {
            item["call_id"].as_str() != Some("one-shot-call-1")
                && item["call_id"].as_str() != Some("one-shot-call-2")
        }),
        "no omitted call survives as a tool exchange item"
    );
    assert!(
        !payload_text.contains("merry_archived"),
        "an omitted exchange leaves no marker behind"
    );
}

/// A window that shrank below the retained history still yields a rolling pass.
///
/// This is the case that reported "no compaction window fits the compaction request
/// budget" instead of compacting: the retained history alone no longer fits the body
/// budget, so a pass that must land under the budget refuses to plan anything. A
/// rolling pass covers the largest window the compaction request can host, keeps the
/// newest turns raw, and lets the runtime run another pass until the request fits.
#[test]
fn rolling_pass_covers_the_front_when_the_window_shrank_below_the_history() {
    let mut session =
        SessionState::new(SessionId::new("rolling-window-shrink").expect("valid session id"));
    for turn in 1..=6 {
        let text = format!("turn {turn} {}", "z".repeat(8_000));
        record_completed_user_turn(&mut session, &text);
    }

    // Each turn is about 2,000 tokens, the body budget holds less than one of them,
    // and the covered budget holds the five older turns.
    let window_budget = window_budget(2_000);
    let resolved = policy(1).resolve(64_000).expect("budget resolves");
    let required = session.build_compaction_preparation_with_window_budget(
        policy(1),
        resolved,
        window_budget,
        CompactionCoverageBudget::limited(12_000),
    );
    assert!(
        matches!(required, Ok(None) | Err(_)),
        "a required fit must refuse when the retained history cannot fit: {required:?}"
    );

    let preparation = session
        .build_rolling_compaction_preparation(
            policy(1),
            resolved,
            window_budget,
            CompactionCoverageBudget::limited(12_000),
        )
        .expect("the rolling pass plans")
        .expect("the rolling pass replaces the checkpoint");
    let CompactionPreparation::ReplaceCheckpoint(input) = preparation else {
        panic!("a rolling pass must replace the checkpoint rather than archive");
    };

    // The pass compresses the oldest turns and leaves the newest raw, so the next
    // pass can continue from a smaller history.
    assert_eq!(
        input.window_plan().covered_turn_ids_u64(),
        vec![1, 2, 3, 4, 5]
    );
    assert_eq!(input.window_plan().retained_turn_ids_u64(), vec![6]);
}

/// A covered-payload budget keeps more completed turns raw before replacing.
#[test]
fn bounded_coverage_budget_retains_more_turns_before_replacing() {
    let mut session =
        SessionState::new(SessionId::new("rolling-bounded-coverage").expect("valid session id"));
    for turn in 1..=5 {
        let text = format!("turn {turn} {}", "y".repeat(4_000));
        record_completed_user_turn(&mut session, &text);
    }

    let preparation = session
        .build_compaction_preparation_with_window_budget(
            policy(1),
            policy(1).resolve(64_000).expect("budget resolves"),
            window_budget(10_000),
            CompactionCoverageBudget::limited(2_100),
        )
        .expect("preparation succeeds")
        .expect("a smaller covered window stays compressible");
    let CompactionPreparation::ReplaceCheckpoint(input) = preparation else {
        panic!("a bounded coverage budget must still replace the checkpoint");
    };

    assert_eq!(input.window_plan().covered_turn_ids_u64(), vec![1, 2]);
    assert_eq!(input.window_plan().retained_turn_ids_u64(), vec![3, 4, 5]);
}

/// When no covered window fits the request budget, tool results are archived instead.
#[test]
fn zero_coverage_budget_keeps_every_turn_raw_and_archives_tool_results() {
    let mut session =
        SessionState::new(SessionId::new("rolling-zero-coverage").expect("valid session id"));
    for turn in 1..=5 {
        record_completed_tool_turn(
            &mut session,
            &format!("zero-call-{turn}"),
            &format!("zero-result-{turn}"),
            &"x".repeat(1_000),
        );
    }

    let preparation = session
        .build_compaction_preparation_with_window_budget(
            policy(1),
            policy(1).resolve(64_000).expect("budget resolves"),
            window_budget(1_300),
            CompactionCoverageBudget::limited(0),
        )
        .expect("preparation succeeds")
        .expect("archive-only reduction is required");
    let CompactionPreparation::ArchiveToolResults(input) = preparation else {
        panic!("a zero coverage budget must not replace the checkpoint");
    };

    assert!(input.window_plan().covered_turn_ids_u64().is_empty());
    assert_eq!(
        input.window_plan().retained_turn_ids_u64(),
        vec![1, 2, 3, 4, 5]
    );
}

/// The planner's coverage budget must hold under the authoritative measurement.
///
/// Planning estimates covered payload tokens from raw text plus a fixed envelope,
/// while the runtime measures the built payload after `serde_json` escaping. An
/// underestimated envelope shows up here as a covered window the budget never
/// allowed, which is a request the runtime would then refuse to send.
#[test]
fn coverage_budget_holds_under_the_authoritative_payload_measurement() {
    // Tool turns carry the largest fixed envelope; short user turns carry the
    // smallest, where the fixed part dominates the estimate.
    let tool_turns = {
        let mut session = SessionState::new(
            SessionId::new("rolling-coverage-budget-tools").expect("valid session id"),
        );
        for turn in 1..=5 {
            record_completed_tool_turn(
                &mut session,
                &format!("budget-call-{turn}"),
                &format!("budget-result-{turn}"),
                "exit code 0",
            );
        }
        session
    };
    let plain_turns = {
        let mut session = SessionState::new(
            SessionId::new("rolling-coverage-budget-plain").expect("valid session id"),
        );
        for turn in 1..=5 {
            record_completed_user_turn(&mut session, &format!("t{turn}"));
        }
        session
    };

    for (label, session) in [("tool turns", &tool_turns), ("plain turns", &plain_turns)] {
        let mut checkpoint_replacements = 0;
        for coverage_budget in [40, 80, 150, 200, 250, 300, 400, 600] {
            let preparation = session
                .build_compaction_preparation_with_window_budget(
                    policy(1),
                    policy(1).resolve(64_000).expect("budget resolves"),
                    window_budget(10_000),
                    CompactionCoverageBudget::limited(coverage_budget),
                )
                .expect("preparation succeeds");
            let Some(CompactionPreparation::ReplaceCheckpoint(input)) = preparation else {
                continue;
            };
            checkpoint_replacements += 1;
            let measured = input
                .covered_payload_token_estimate()
                .expect("payload measures");
            assert!(
                measured <= coverage_budget,
                "{label}: authoritative measurement {measured} exceeds the coverage budget {coverage_budget}"
            );
        }
        assert!(
            checkpoint_replacements > 0,
            "{label}: at least one budget must still replace the checkpoint"
        );
    }
}

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
fn preferred_history_target_keeps_a_small_tail_in_a_large_window() {
    let mut session = SessionState::new(SessionId::new("bounded-tail").expect("valid session id"));
    for turn in 1..=8 {
        record_completed_user_turn(&mut session, &format!("turn {turn} {}", "x".repeat(40_000)));
    }
    let resolved = policy(5).resolve(2_000_000).expect("budget resolves");
    let budget =
        CompactionWindowBudget::new(2_000_000, 1_800_000, 0, 0, resolved.output_token_limit())
            .expect("valid budget")
            .with_retained_history_target(resolved.retained_history_token_target())
            .expect("valid history target");
    let plan = session
        .plan_compaction_window(policy(5), budget)
        .expect("plan succeeds")
        .expect("history needs a smaller tail");
    assert_eq!(plan.covered_turn_ids_u64(), vec![1, 2, 3, 4, 5]);
    assert_eq!(plan.retained_turn_ids_u64(), vec![6, 7, 8]);
}

#[test]
fn oversized_minimum_turn_falls_back_to_one_turn_not_the_full_hard_budget() {
    let mut session = SessionState::new(SessionId::new("minimum-tail").expect("valid session id"));
    for turn in 1..=8 {
        record_completed_user_turn(&mut session, &format!("turn {turn} {}", "x".repeat(32_000)));
    }
    let resolved = policy(5).resolve(64_000).expect("budget resolves");
    let budget = CompactionWindowBudget::new(64_000, 56_000, 0, 0, resolved.output_token_limit())
        .expect("valid budget")
        .with_retained_history_target(resolved.retained_history_token_target())
        .expect("valid history target");
    let plan = session
        .plan_compaction_window(policy(5), budget)
        .expect("mandatory raw turn still fits the hard budget")
        .expect("history is compacted");
    assert_eq!(plan.covered_turn_ids_u64(), vec![1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(plan.retained_turn_ids_u64(), vec![8]);
}

#[test]
fn oversized_five_turn_tail_keeps_four_whole_exchanges_before_archiving() {
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

    assert_eq!(plan.covered_turn_ids_u64(), vec![1, 2]);
    assert_eq!(plan.retained_turn_ids_u64(), vec![3, 4, 5, 6]);
    assert!(plan.archived_tool_call_ids_for_tests().is_empty());
}

#[test]
fn planner_selects_every_complete_tail_size_against_the_token_budget() {
    let mut session =
        SessionState::new(SessionId::new("rolling-fallback").expect("valid session id"));
    for turn in 1..=8 {
        record_completed_user_turn(&mut session, &format!("turn-{turn}-{}", "x".repeat(396)));
    }

    for (tokens, retained) in [
        (650, vec![4, 5, 6, 7, 8]),
        (550, vec![5, 6, 7, 8]),
        (350, vec![7, 8]),
    ] {
        let plan = session
            .plan_compaction_window(policy(5), window_budget(tokens))
            .expect("plan succeeds")
            .expect("history needs compaction");
        assert_eq!(plan.retained_turn_ids_u64(), retained);
        assert!(plan.archived_tool_call_ids_for_tests().is_empty());
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
            CompactionCoverageBudget::unbounded(),
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
            CompactionCoverageBudget::unbounded(),
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
            CompactionCoverageBudget::limited(0),
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
    assert_eq!(retained_turn_fallbacks(7, 9), vec![7, 6, 5, 4, 3, 2, 1]);
    assert_eq!(retained_turn_fallbacks(3, 9), vec![3, 2, 1]);
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
            CompactionCoverageBudget::unbounded(),
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
            CompactionCoverageBudget::limited(0),
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
