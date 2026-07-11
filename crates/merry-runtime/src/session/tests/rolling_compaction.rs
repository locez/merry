use super::*;
use crate::{
    CheckpointError, CompactionError, FileSessionStore,
    compaction::{
        CompactionPreparation, CompactionWindowBudget, checkpoint_from_candidate_json,
        retained_turn_fallbacks,
    },
    context::compacted_checkpoint_wrapper_token_ceiling,
    session::transcript::ToolResultPromptProjection,
};

fn policy(retained_model_turns: usize) -> CitationCompactionPolicy {
    CitationCompactionPolicy::new(None, None, retained_model_turns).expect("valid policy")
}

fn window_budget(max_dynamic_body_tokens: u64) -> CompactionWindowBudget {
    CompactionWindowBudget::new(64_000, max_dynamic_body_tokens, 0, 0, 128)
        .expect("valid window budget")
}

fn record_completed_user_turn(session: &mut SessionState, text: &str) {
    session
        .record_test_user_message_body(text)
        .expect("completed user turn records");
}

fn record_completed_tool_turn(
    session: &mut SessionState,
    call_id: &str,
    result_artifact_id: &str,
    result_body: &str,
) {
    let turn_id = session.begin_model_turn().expect("tool turn begins");
    session
        .record_user_message_body(turn_id, &format!("use {call_id}"))
        .expect("tool user message records");
    let call = pending_tool_call(call_id);
    session
        .record_tool_call_batch_pending(
            turn_id,
            PendingToolCallBatch::new(
                ToolCallBatchId::new(&format!("batch-{call_id}")).expect("valid batch id"),
                vec![call.clone()],
            )
            .expect("valid tool batch"),
        )
        .expect("tool call records");
    session
        .close_model_response(turn_id, true)
        .expect("tool response closes");
    session
        .submit_tool_result(
            ToolCallResult::succeeded(
                call.id().clone(),
                ArtifactRef::new(artifact_id(result_artifact_id), ArtifactKind::Text),
            ),
            ArtifactContent::text(result_body),
        )
        .expect("tool result records");
}

fn checkpoint_candidate(ref_id: &str) -> String {
    serde_json::json!({
        "confirmed_decisions": [],
        "rejected_approaches": [],
        "constraints_preferences_boundaries": [],
        "corrected_misunderstandings": [],
        "durable_conclusions": [{
            "id": "c1",
            "text": "The covered prefix was compacted.",
            "refs": [ref_id],
        }],
        "open_questions": [],
        "current_progress_and_next_steps": [],
        "exact_details": [],
        "handoffs": [],
    })
    .to_string()
}

fn rolling_keep_candidate(ref_id: &str) -> String {
    serde_json::json!({
        "confirmed_decisions": [],
        "rejected_approaches": [],
        "constraints_preferences_boundaries": [],
        "corrected_misunderstandings": [],
        "durable_conclusions": [{
            "id": "c1",
            "text": "The covered prefix was compacted.",
            "refs": [ref_id],
        }],
        "open_questions": [],
        "current_progress_and_next_steps": [],
        "exact_details": [],
        "handoffs": [{"action": "keep", "old_id": "c1"}],
    })
    .to_string()
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
fn task_anchor_change_invalidates_prepared_checkpoint_window() {
    let mut session =
        SessionState::new(SessionId::new("rolling-anchor-fingerprint").expect("valid id"));
    record_completed_user_turn(&mut session, "old prefix");
    record_completed_user_turn(&mut session, "retained tail");
    let input = session
        .build_citation_compaction_input(
            policy(1),
            policy(1).resolve(64_000).expect("budget resolves"),
        )
        .expect("input builds")
        .expect("old prefix is compressible");
    session.set_task_anchor(TaskAnchor::new("new objective").expect("valid anchor"));

    let error = session
        .install_citation_compaction_candidate(input, &checkpoint_candidate("h0"))
        .expect_err("anchor change makes the window stale");
    assert!(matches!(
        error,
        RuntimeError::Compaction {
            source: CompactionError::StaleWindow
        }
    ));
}

#[test]
fn reverse_tool_results_archive_by_result_arrival_and_keep_pairs_valid() {
    let mut session =
        SessionState::new(SessionId::new("rolling-reverse-results").expect("valid session id"));
    record_completed_user_turn(&mut session, "old prefix");

    let tool_turn = session.begin_model_turn().expect("tool turn begins");
    let call_a = pending_tool_call("reverse-call-a");
    let call_b = pending_tool_call("reverse-call-b");
    session
        .record_tool_call_batch_pending(
            tool_turn,
            PendingToolCallBatch::new(
                ToolCallBatchId::new("reverse-batch").expect("valid batch id"),
                vec![call_a.clone(), call_b.clone()],
            )
            .expect("valid batch"),
        )
        .expect("calls record");
    session
        .close_model_response(tool_turn, true)
        .expect("tool response closes");
    for (call, artifact, body) in [
        (&call_b, "reverse-result-b", "b".repeat(1_000)),
        (&call_a, "reverse-result-a", "a".repeat(1_000)),
    ] {
        session
            .submit_tool_result(
                ToolCallResult::succeeded(
                    call.id().clone(),
                    ArtifactRef::new(artifact_id(artifact), ArtifactKind::Text),
                ),
                ArtifactContent::text(body),
            )
            .expect("tool result records");
    }
    for turn in 3..=6 {
        record_completed_user_turn(&mut session, &format!("small retained {turn}"));
    }

    let budget = window_budget(450);
    let plan = session
        .plan_compaction_window(policy(5), budget)
        .expect("plan succeeds")
        .expect("old prefix is compressible");
    assert_eq!(
        plan.archived_tool_call_ids_for_tests(),
        vec![call_b.id().clone()],
        "the first arriving result must be archived first"
    );

    let resolved = policy(5).resolve(64_000).expect("budget resolves");
    let input = session
        .build_citation_compaction_input_with_window_budget(policy(5), resolved, budget)
        .expect("input builds")
        .expect("old prefix is compressible");
    let result_b_ref = session
        .transcript
        .items
        .iter()
        .find_map(|item| match item {
            TranscriptItem::ToolResult { id, call_id, .. } if call_id == call_b.id() => {
                Some(format!("h{}", id.as_u64()))
            }
            _ => None,
        })
        .expect("result B ref exists");
    assert!(
        input
            .manifest()
            .refs()
            .iter()
            .any(|reference| reference.id().as_str() == result_b_ref)
    );

    session
        .install_citation_compaction_candidate(input, &checkpoint_candidate("h0"))
        .expect("checkpoint installs");
    let provider = session
        .provider_transcript_snapshot()
        .expect("provider projection builds");
    assert!(matches!(
        &provider[0],
        crate::session::TranscriptItemSnapshot::ToolCall { call }
            if call.id() == call_a.id()
    ));
    assert!(matches!(
        &provider[1],
        crate::session::TranscriptItemSnapshot::ToolCall { call }
            if call.id() == call_b.id()
    ));
    let notice = provider
        .iter()
        .find_map(|item| match item {
            crate::session::TranscriptItemSnapshot::ToolResult {
                call_id, content, ..
            } if call_id == call_b.id() => content.as_text(),
            _ => None,
        })
        .expect("result B notice exists");
    let notice: serde_json::Value = serde_json::from_str(notice).expect("notice is typed JSON");
    assert_eq!(notice["merry_archived"], true);
    assert_eq!(notice["status"], "succeeded");
    assert_eq!(notice["artifact_id"], "reverse-result-b");
    assert_eq!(notice["ref"], result_b_ref);

    let result_a = provider
        .iter()
        .find_map(|item| match item {
            crate::session::TranscriptItemSnapshot::ToolResult {
                call_id, content, ..
            } if call_id == call_a.id() => content.as_text(),
            _ => None,
        })
        .expect("result A remains visible");
    assert_eq!(result_a, "a".repeat(1_000));
}

#[test]
fn invalid_candidate_does_not_apply_planned_tool_archives() {
    let mut session =
        SessionState::new(SessionId::new("rolling-invalid-no-archive").expect("valid id"));
    record_completed_user_turn(&mut session, "old prefix");
    for turn in 1..=5 {
        record_completed_tool_turn(
            &mut session,
            &format!("invalid-call-{turn}"),
            &format!("invalid-result-{turn}"),
            &"x".repeat(1_000),
        );
    }
    let budget = window_budget(1_300);
    let input = session
        .build_citation_compaction_input_with_window_budget(
            policy(5),
            policy(5).resolve(64_000).expect("budget resolves"),
            budget,
        )
        .expect("input builds")
        .expect("old prefix is compressible");
    assert!(!input.window_plan().archived_tool_call_ids().is_empty());

    let error = session
        .install_citation_compaction_candidate(
            input,
            &checkpoint_candidate("missing-checkpoint-ref"),
        )
        .expect_err("invalid candidate is rejected");
    assert!(matches!(error, RuntimeError::Checkpoint { .. }));

    let first_result = session
        .transcript
        .items
        .iter()
        .find_map(|item| match item {
            TranscriptItem::ToolResult {
                call_id,
                prompt_projection,
                ..
            } if call_id.as_str() == "invalid-call-1" => Some(*prompt_projection),
            _ => None,
        })
        .expect("first result exists");
    assert_eq!(first_result, ToolResultPromptProjection::Full);
}

#[test]
fn retained_archive_ref_stays_pinned_but_hidden_across_rolling_compactions() {
    let mut session =
        SessionState::new(SessionId::new("rolling-existing-notice").expect("valid id"));
    record_completed_user_turn(&mut session, "old prefix");
    record_completed_user_turn(&mut session, "older retained turn");
    record_completed_tool_turn(
        &mut session,
        "existing-notice-call",
        "existing-notice-result",
        &"x".repeat(1_000),
    );
    for turn in 4..=6 {
        record_completed_user_turn(&mut session, &format!("small retained {turn}"));
    }
    let (result_ref, result_projection) = session
        .transcript
        .items
        .iter_mut()
        .find_map(|item| match item {
            TranscriptItem::ToolResult {
                id,
                call_id,
                prompt_projection,
                ..
            } if call_id.as_str() == "existing-notice-call" => {
                Some((format!("h{}", id.as_u64()), prompt_projection))
            }
            _ => None,
        })
        .expect("existing notice result exists");
    *result_projection = ToolResultPromptProjection::ArtifactNotice;

    let input = session
        .build_citation_compaction_input_with_window_budget(
            policy(5),
            policy(5).resolve(64_000).expect("budget resolves"),
            window_budget(10_000),
        )
        .expect("input builds")
        .expect("old prefix is compressible");
    assert_eq!(
        input.window_plan().archived_tool_call_ids_for_tests(),
        vec![tool_call_id("existing-notice-call")]
    );
    assert!(
        input
            .pinned_refs()
            .iter()
            .any(|id| id.as_str() == result_ref)
    );

    let error = checkpoint_from_candidate_json(
        input.manifest().checkpoint_id().clone(),
        &input,
        &checkpoint_candidate(&result_ref),
    )
    .expect_err("a retained-tail ref hidden from the compactor must be rejected");
    assert!(matches!(
        error,
        RuntimeError::Checkpoint {
            source: CheckpointError::UnknownRef { ref ref_id, .. },
        } if ref_id == &result_ref
    ));

    session
        .install_citation_compaction_candidate(input, &checkpoint_candidate("h0"))
        .expect("checkpoint installs");
    let checkpoint = session
        .compacted_checkpoint
        .as_ref()
        .and_then(crate::CompactedCheckpoint::citation_backed)
        .expect("citation checkpoint installed");
    assert!(
        checkpoint
            .manifest()
            .refs()
            .iter()
            .any(|reference| reference.id().as_str() == result_ref)
    );

    record_completed_user_turn(&mut session, "new turn after first compaction");
    let second_input = session
        .build_citation_compaction_input_with_window_budget(
            policy(5),
            policy(5).resolve(64_000).expect("budget resolves"),
            window_budget(10_000),
        )
        .expect("second input builds")
        .expect("the next oldest turn is compressible");
    assert!(
        second_input
            .pinned_refs()
            .iter()
            .any(|id| id.as_str() == result_ref)
    );
    let payload: serde_json::Value = serde_json::from_str(
        &second_input
            .to_model_payload_json()
            .expect("second payload serializes"),
    )
    .expect("second payload parses");
    let previous_ref_ids = payload["previous_checkpoint"]["original_ref_manifest"]["refs"]
        .as_array()
        .expect("previous original refs")
        .iter()
        .map(|reference| reference["id"].as_str().expect("ref id"))
        .collect::<Vec<_>>();
    assert!(previous_ref_ids.contains(&"h0"));
    assert!(
        !previous_ref_ids.contains(&result_ref.as_str()),
        "a pinned-only retained ref must not become previous-checkpoint evidence"
    );

    let error = checkpoint_from_candidate_json(
        second_input.manifest().checkpoint_id().clone(),
        &second_input,
        &checkpoint_candidate(&result_ref),
    )
    .expect_err("the hidden ref must remain invalid on the next rolling compaction");
    assert!(matches!(
        error,
        RuntimeError::Checkpoint {
            source: CheckpointError::UnknownRef { ref ref_id, .. },
        } if ref_id == &result_ref
    ));
    checkpoint_from_candidate_json(
        second_input.manifest().checkpoint_id().clone(),
        &second_input,
        &rolling_keep_candidate("h0"),
    )
    .expect("a previous entry's supplied original ref remains valid");
}

#[test]
fn archives_tried_in_five_turn_plan_do_not_leak_into_three_turn_fallback() {
    let mut session = SessionState::new(SessionId::new("rolling-archive-reset").expect("valid id"));
    record_completed_user_turn(&mut session, "old prefix");
    for turn in 1..=2 {
        let turn_id = session.begin_model_turn().expect("large tool turn begins");
        session
            .record_user_message_body(turn_id, &"discussion".repeat(200))
            .expect("large discussion records");
        let call = pending_tool_call(&format!("dropped-call-{turn}"));
        session
            .record_tool_call_batch_pending(
                turn_id,
                PendingToolCallBatch::new(
                    ToolCallBatchId::new(&format!("dropped-batch-{turn}")).expect("valid batch id"),
                    vec![call.clone()],
                )
                .expect("valid batch"),
            )
            .expect("tool call records");
        session
            .close_model_response(turn_id, true)
            .expect("tool response closes");
        session
            .submit_tool_result(
                ToolCallResult::succeeded(
                    call.id().clone(),
                    ArtifactRef::new(
                        artifact_id(&format!("dropped-result-{turn}")),
                        ArtifactKind::Text,
                    ),
                ),
                ArtifactContent::text("tool body".repeat(100)),
            )
            .expect("tool result records");
    }
    for turn in 4..=6 {
        record_completed_user_turn(&mut session, &format!("small retained {turn}"));
    }

    let plan = session
        .plan_compaction_window(policy(5), window_budget(400))
        .expect("fallback plan succeeds")
        .expect("three-turn fallback covers the large tool turns");

    assert_eq!(plan.retained_turn_ids_u64(), vec![4, 5, 6]);
    assert!(plan.archived_tool_call_ids().is_empty());
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

#[test]
fn rendered_checkpoint_over_output_limit_rejects_without_state_mutation() {
    let mut session =
        SessionState::new(SessionId::new("rolling-rendered-too-large").expect("valid id"));
    record_completed_user_turn(&mut session, "old prefix");
    record_completed_user_turn(&mut session, "retained tail");
    let small_policy =
        CitationCompactionPolicy::new(Some(5), Some(10_000), 1).expect("valid small output policy");
    let input = session
        .build_citation_compaction_input(small_policy, small_policy.resolve(64_000).unwrap())
        .expect("input builds")
        .expect("old prefix is compressible");

    let error = session
        .install_citation_compaction_candidate(input, &checkpoint_candidate("h0"))
        .expect_err("rendered checkpoint exceeds token limit");
    assert!(matches!(
        error,
        RuntimeError::Compaction {
            source: CompactionError::RenderedCheckpointTooLarge { max_tokens: 5, .. }
        }
    ));
    assert_eq!(
        session.prompt_history_projection().compacted_through(),
        None
    );
    assert!(session.compacted_checkpoint.is_none());
    assert_eq!(
        session
            .provider_transcript_snapshot()
            .expect("provider projection builds")
            .len(),
        2
    );
}

#[tokio::test]
async fn archive_only_manifest_resolves_refs_and_round_trips_through_store() {
    let session_id = SessionId::new("rolling-archive-manifest").expect("valid id");
    let mut session = SessionState::new(session_id.clone());
    for turn in 1..=5 {
        record_completed_tool_turn(
            &mut session,
            &format!("manifest-call-{turn}"),
            &format!("manifest-result-{turn}"),
            &format!("manifest exact body {turn} {}", "x".repeat(1_000)),
        );
    }
    let preparation = session
        .build_compaction_preparation_with_window_budget(
            policy(5),
            policy(5).resolve(64_000).expect("budget resolves"),
            window_budget(1_300),
        )
        .expect("preparation builds")
        .expect("archive-only is required");
    let CompactionPreparation::ArchiveToolResults(input) = preparation else {
        panic!("expected archive-only preparation");
    };
    let archived_ref = input
        .archived_refs()
        .first()
        .expect("one archived ref exists")
        .id()
        .clone();
    session
        .install_archive_only_compaction(input)
        .expect("archive-only install succeeds");

    assert!(session.compacted_checkpoint.is_none());
    let page = session
        .read_checkpoint_ref_page(&archived_ref, 0, 2_000)
        .expect("archive manifest resolves exact source");
    assert!(page.content().contains("manifest exact body 1"));

    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    session.save_to(&store).await.expect("session saves");
    let loaded = SessionState::load_from(&store, &session_id)
        .await
        .expect("session loads");
    assert!(loaded.compacted_checkpoint.is_none());
    let loaded_page = loaded
        .read_checkpoint_ref_page(&archived_ref, 0, 2_000)
        .expect("loaded archive manifest resolves exact source");
    assert_eq!(loaded_page.content(), page.content());
}

#[test]
fn prepared_archive_only_install_is_read_only_until_commit() {
    let mut session = SessionState::new(
        SessionId::new("prepared-archive-only-install").expect("valid session id"),
    );
    for turn in 1..=5 {
        record_completed_tool_turn(
            &mut session,
            &format!("prepared-archive-call-{turn}"),
            &format!("prepared-archive-result-{turn}"),
            &format!("prepared archive body {turn} {}", "x".repeat(1_000)),
        );
    }
    let preparation = session
        .build_compaction_preparation_with_window_budget(
            policy(5),
            policy(5).resolve(64_000).expect("budget resolves"),
            window_budget(1_300),
        )
        .expect("preparation builds")
        .expect("archive-only preparation exists");
    let CompactionPreparation::ArchiveToolResults(input) = preparation else {
        panic!("expected archive-only preparation");
    };
    let bundle_before = session
        .persistable_bundle()
        .expect("session is persistable before prepare")
        .document_bytes;
    let transcript_before = session.transcript.persisted();
    let projection_before = session.prompt_history_projection();
    let checkpoint_before = session.compacted_checkpoint.clone();
    let archive_manifest_before = session.archived_ref_manifest.clone();

    let prepared = session
        .prepare_archive_only_compaction_install(input)
        .expect("archive-only install prepares");

    assert_eq!(session.transcript.persisted(), transcript_before);
    assert_eq!(session.prompt_history_projection(), projection_before);
    assert_eq!(session.compacted_checkpoint, checkpoint_before);
    assert_eq!(session.archived_ref_manifest, archive_manifest_before);
    assert_eq!(
        session
            .persistable_bundle()
            .expect("session remains persistable after prepare")
            .document_bytes,
        bundle_before
    );
    assert_ne!(prepared.transcript().persisted(), transcript_before);
    assert_eq!(prepared.prompt_history_projection(), projection_before);
    assert_eq!(prepared.compacted_checkpoint(), checkpoint_before.as_ref());
    assert_ne!(prepared.archived_ref_manifest(), &archive_manifest_before);
    assert_eq!(prepared.outcome(), None);
    session
        .revalidate_prepared_compaction_install(&prepared)
        .expect("unchanged session revalidates");

    assert_eq!(session.commit_prepared_compaction_install(prepared), None);
    assert_ne!(session.transcript.persisted(), transcript_before);
    assert_ne!(session.archived_ref_manifest, archive_manifest_before);
}

#[test]
fn failed_archived_result_notice_has_exact_four_json_fields() {
    let mut session = SessionState::new(SessionId::new("rolling-failed-notice").expect("valid id"));
    let turn_id = session.begin_model_turn().expect("failed tool turn begins");
    let failed_call = pending_tool_call("failed-notice-call");
    session
        .record_tool_call_batch_pending(
            turn_id,
            PendingToolCallBatch::new(
                ToolCallBatchId::new("failed-notice-batch").expect("valid batch id"),
                vec![failed_call.clone()],
            )
            .expect("valid batch"),
        )
        .expect("failed call records");
    session
        .close_model_response(turn_id, true)
        .expect("tool response closes");
    session
        .submit_tool_result(
            ToolCallResult::failed(
                failed_call.id().clone(),
                ArtifactRef::new(artifact_id("failed-notice-result"), ArtifactKind::Text),
                ErrorInfo::new("tool_failed", "expected failure").expect("valid diagnostic"),
            ),
            ArtifactContent::text("f".repeat(1_000)),
        )
        .expect("failed result records");
    for turn in 2..=5 {
        record_completed_tool_turn(
            &mut session,
            &format!("failed-tail-call-{turn}"),
            &format!("failed-tail-result-{turn}"),
            &"x".repeat(1_000),
        );
    }

    let preparation = session
        .build_compaction_preparation_with_window_budget(
            policy(5),
            policy(5).resolve(64_000).expect("budget resolves"),
            window_budget(1_300),
        )
        .expect("preparation builds")
        .expect("archive-only is required");
    let CompactionPreparation::ArchiveToolResults(input) = preparation else {
        panic!("expected archive-only preparation");
    };
    session
        .install_archive_only_compaction(input)
        .expect("archive-only install succeeds");

    let notice = session
        .provider_transcript_snapshot()
        .expect("provider projection builds")
        .into_iter()
        .find_map(|item| match item {
            crate::session::TranscriptItemSnapshot::ToolResult {
                call_id, content, ..
            } if call_id == *failed_call.id() => content.as_text().map(str::to_owned),
            _ => None,
        })
        .expect("failed notice exists");
    let notice: serde_json::Value = serde_json::from_str(&notice).expect("notice parses");
    let object = notice.as_object().expect("notice is an object");
    assert_eq!(object.len(), 4);
    assert_eq!(notice["merry_archived"], true);
    assert_eq!(notice["status"], "failed");
    assert_eq!(notice["artifact_id"], "failed-notice-result");
    assert!(
        notice["ref"]
            .as_str()
            .is_some_and(|value| value.starts_with('h'))
    );
}
