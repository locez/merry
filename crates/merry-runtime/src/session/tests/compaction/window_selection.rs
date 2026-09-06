use crate::session::{
    ModelTurnId,
    tests::{
        ArtifactContent, ArtifactKind, ArtifactRef, CitationCompactionPolicy, ModelTurnStatus,
        PendingToolCallBatch, RuntimeError, SessionId, SessionState, SessionStateTestExt,
        TaskAnchor, ToolCallBatchId, ToolCallResult, TranscriptItem, artifact_id,
        pending_tool_call,
    },
    transcript::{
        ToolCallPromptProjection, ToolResultPromptProjection, Transcript, TranscriptItemId,
    },
};
use std::collections::BTreeMap;

#[test]
fn compaction_window_retains_complete_model_turns_without_splitting() {
    let mut session =
        SessionState::new(SessionId::new("compaction-complete-turns").expect("valid session id"));
    let covered_turn = session.begin_model_turn().expect("covered turn begins");
    session
        .record_user_message_body(covered_turn, "covered user")
        .expect("covered user records");
    session
        .record_assistant_text_output(covered_turn, "covered assistant".to_owned())
        .expect("covered assistant records");
    session
        .close_model_response(covered_turn, false)
        .expect("covered turn completes");

    let retained_turn = session.begin_model_turn().expect("retained turn begins");
    session
        .record_user_message_body(retained_turn, "retained user")
        .expect("retained user records");
    session
        .record_assistant_text_output(retained_turn, "retained assistant".to_owned())
        .expect("retained assistant records");
    session
        .close_model_response(retained_turn, false)
        .expect("retained turn completes");

    let input = session
        .build_test_citation_compaction_input(
            CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy"),
        )
        .expect("input builds")
        .expect("covered turn is compressible");
    let payload: serde_json::Value =
        serde_json::from_str(&input.to_model_payload_json().expect("payload serializes"))
            .expect("payload parses");

    let window = payload["window"].as_array().expect("window is an array");
    assert_eq!(window.len(), 1);
    assert_eq!(
        window[0]["items"]
            .as_array()
            .expect("turn items are an array")
            .len(),
        2
    );
    let payload = payload.to_string();
    assert!(payload.contains("covered user"));
    assert!(payload.contains("covered assistant"));
    assert!(!payload.contains("retained user"));
    assert!(!payload.contains("retained assistant"));
}

#[test]
fn compaction_window_never_covers_in_progress_model_turn() {
    let mut session =
        SessionState::new(SessionId::new("compaction-open-turn").expect("valid session id"));
    let completed_turn = session.begin_model_turn().expect("completed turn begins");
    session
        .record_user_message_body(completed_turn, "covered completed user")
        .expect("completed user records");
    session
        .close_model_response(completed_turn, false)
        .expect("first turn completes");

    let retained_turn = session.begin_model_turn().expect("retained turn begins");
    session
        .record_user_message_body(retained_turn, "retained completed user")
        .expect("retained completed user records");
    session
        .close_model_response(retained_turn, false)
        .expect("retained turn completes");

    let open_turn = session.begin_model_turn().expect("open turn begins");
    session
        .record_user_message_body(open_turn, "open user")
        .expect("open user records");
    session
        .record_assistant_text_output(open_turn, "open partial assistant".to_owned())
        .expect("open assistant records");

    let input = session
        .build_test_citation_compaction_input(
            CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy"),
        )
        .expect("input builds")
        .expect("completed prefix remains compressible");
    let payload: serde_json::Value =
        serde_json::from_str(&input.to_model_payload_json().expect("payload serializes"))
            .expect("payload parses");

    let window = payload["window"].as_array().expect("window is an array");
    assert_eq!(window.len(), 1);
    let payload = payload.to_string();
    assert!(payload.contains("covered completed user"));
    assert!(!payload.contains("retained completed user"));
    assert!(!payload.contains("open user"));
    assert!(!payload.contains("open partial assistant"));
}

#[test]
fn compaction_structure_errors_map_to_stale_window() {
    let turn_a = ModelTurnId::new(1);
    let turn_b = ModelTurnId::new(2);
    let call = pending_tool_call("invalid-structure-call");
    let result_artifact_id = artifact_id("invalid-structure-result");
    let result = ToolCallResult::succeeded(
        call.id().clone(),
        ArtifactRef::new(result_artifact_id.clone(), ArtifactKind::Text),
    );
    let call_item = |id, model_turn_id| TranscriptItem::ToolCall {
        id: TranscriptItemId::new(id),
        model_turn_id,
        call: call.clone(),
        prompt_projection: ToolCallPromptProjection::Full,
    };
    let result_item = |id, model_turn_id| TranscriptItem::ToolResult {
        id: TranscriptItemId::new(id),
        model_turn_id,
        call_id: call.id().clone(),
        result: result.clone(),
        artifact_id: result_artifact_id.clone(),
        prompt_projection: ToolResultPromptProjection::Full,
    };
    let user_item = |id, model_turn_id, artifact| TranscriptItem::UserMessage {
        id: TranscriptItemId::new(id),
        model_turn_id,
        artifact_id: artifact_id(artifact),
        image_artifact_ids: Vec::new(),
        origin: crate::session::UserInputOrigin::ExternalUser,
    };

    let cases = [
        (
            "duplicate_call",
            vec![call_item(0, turn_a), call_item(1, turn_a)],
            vec![(turn_a, ModelTurnStatus::Completed)],
            ModelTurnId::new(2),
        ),
        (
            "duplicate_result",
            vec![
                call_item(0, turn_a),
                result_item(1, turn_a),
                result_item(2, turn_a),
            ],
            vec![(turn_a, ModelTurnStatus::Completed)],
            ModelTurnId::new(2),
        ),
        (
            "result_before_call",
            vec![result_item(0, turn_a), call_item(1, turn_a)],
            vec![(turn_a, ModelTurnStatus::Completed)],
            ModelTurnId::new(2),
        ),
        (
            "cross_turn_result",
            vec![call_item(0, turn_a), result_item(1, turn_b)],
            vec![
                (turn_a, ModelTurnStatus::Completed),
                (turn_b, ModelTurnStatus::Completed),
            ],
            ModelTurnId::new(3),
        ),
        (
            "interleaved_turns",
            vec![
                user_item(0, turn_a, "interleaved-a-first"),
                user_item(1, turn_b, "interleaved-b"),
                user_item(2, turn_a, "interleaved-a-second"),
            ],
            vec![
                (turn_a, ModelTurnStatus::Completed),
                (turn_b, ModelTurnStatus::Completed),
            ],
            ModelTurnId::new(3),
        ),
    ];

    for (case, items, model_turns, next_model_turn_id) in cases {
        let mut session = SessionState::new(
            SessionId::new(&format!("compaction-invalid-{case}")).expect("valid session id"),
        );
        session.transcript = Transcript {
            items,
            next_id: TranscriptItemId::new(3),
            model_turns: model_turns.into_iter().collect(),
            model_turn_sequences: BTreeMap::new(),
            next_model_turn_id,
        };

        let error = session
            .build_test_citation_compaction_input(
                CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy"),
            )
            .expect_err("invalid turn grouping must reject compaction");

        assert!(
            matches!(
                error,
                RuntimeError::Compaction {
                    source: crate::CompactionError::StaleWindow
                }
            ),
            "case {case} returned {error:?}"
        );
    }
}

#[test]
fn compaction_input_excludes_retained_raw_tail() {
    let mut session =
        SessionState::new(SessionId::new("compaction-input-tail").expect("valid session id"));
    session.set_task_anchor(TaskAnchor::new("Keep the current task").expect("valid anchor"));
    session
        .record_test_user_message_body("old user message to compact")
        .expect("user records");
    session
        .record_test_assistant_text_output("old assistant message to compact".to_owned())
        .expect("assistant records");
    session
        .record_test_user_message_body("retained raw tail user sentinel")
        .expect("user records");
    session
        .record_test_assistant_text_output("retained raw tail assistant sentinel".to_owned())
        .expect("assistant records");

    let policy = CitationCompactionPolicy::new(Some(128), Some(4096), 2).expect("valid policy");
    let input = session
        .build_test_citation_compaction_input(policy)
        .expect("input builds")
        .expect("old prefix should be compressible");
    let payload = input.to_model_payload_json().expect("payload serializes");

    assert!(payload.contains("old user message to compact"));
    assert!(payload.contains("old assistant message to compact"));
    assert!(!payload.contains("retained raw tail user sentinel"));
    assert!(!payload.contains("retained raw tail assistant sentinel"));
    assert!(payload.contains("\"current_user_input_excluded\":true"));
}

#[test]
fn compaction_retained_raw_tail_is_policy_driven() {
    let mut session =
        SessionState::new(SessionId::new("retained-tail-policy").expect("valid session id"));
    session
        .record_test_user_message_body("covered user sentinel")
        .expect("user records");
    session
        .record_test_assistant_text_output("covered assistant sentinel".to_owned())
        .expect("assistant records");
    session
        .record_test_user_message_body("tail user one sentinel")
        .expect("user records");
    session
        .record_test_assistant_text_output("tail assistant one sentinel".to_owned())
        .expect("assistant records");
    session
        .record_test_user_message_body("tail user two sentinel")
        .expect("user records");
    session
        .record_test_assistant_text_output("tail assistant two sentinel".to_owned())
        .expect("assistant records");

    let input = session
        .build_test_citation_compaction_input(
            CitationCompactionPolicy::new(Some(128), Some(4096), 4).expect("valid policy"),
        )
        .expect("input builds")
        .expect("old prefix should be compressible");
    let payload = input.to_model_payload_json().expect("payload serializes");

    assert!(payload.contains("covered user sentinel"));
    assert!(payload.contains("covered assistant sentinel"));
    assert!(!payload.contains("tail user one sentinel"));
    assert!(!payload.contains("tail assistant one sentinel"));
    assert!(!payload.contains("tail user two sentinel"));
    assert!(!payload.contains("tail assistant two sentinel"));
}

#[test]
fn compaction_retains_recent_completed_turns_and_later_aborted_turns() {
    let mut session =
        SessionState::new(SessionId::new("retained-model-turn-policy").expect("valid session id"));

    let covered_aborted = session.begin_model_turn().expect("aborted turn begins");
    session
        .record_user_message_body(covered_aborted, "covered aborted sentinel")
        .expect("aborted content records");
    session
        .abort_model_turn(covered_aborted)
        .expect("old aborted turn closes");

    session
        .record_test_user_message_body("covered completed sentinel")
        .expect("old completed turn records");
    session
        .record_test_user_message_body("retained completed boundary sentinel")
        .expect("retained completed boundary records");

    let retained_aborted = session.begin_model_turn().expect("aborted turn begins");
    session
        .record_user_message_body(retained_aborted, "retained aborted sentinel")
        .expect("retained aborted content records");
    session
        .abort_model_turn(retained_aborted)
        .expect("recent aborted turn closes");

    session
        .record_test_user_message_body("retained latest completed sentinel")
        .expect("latest completed turn records");

    let open_turn = session.begin_model_turn().expect("open turn begins");
    session
        .record_user_message_body(open_turn, "open turn sentinel")
        .expect("open content records");

    let input = session
        .build_test_citation_compaction_input(
            CitationCompactionPolicy::new(None, None, 2).expect("valid policy"),
        )
        .expect("input builds")
        .expect("older closed turns are compressible");
    let payload = input.to_model_payload_json().expect("payload serializes");

    assert!(payload.contains("covered aborted sentinel"));
    assert!(payload.contains("covered completed sentinel"));
    assert!(!payload.contains("retained completed boundary sentinel"));
    assert!(!payload.contains("retained aborted sentinel"));
    assert!(!payload.contains("retained latest completed sentinel"));
    assert!(!payload.contains("open turn sentinel"));
}

#[test]
fn compaction_retains_an_entire_multi_item_tool_turn() {
    let mut session =
        SessionState::new(SessionId::new("retained-tool-turn").expect("valid session id"));
    session
        .record_test_user_message_body("covered old turn sentinel")
        .expect("old turn records");

    let turn_id = session.begin_model_turn().expect("tool turn begins");
    session
        .record_user_message_body(turn_id, "retained tool user sentinel")
        .expect("tool user records");
    session
        .record_assistant_text_output(turn_id, "retained tool commentary sentinel".to_owned())
        .expect("tool commentary records");
    let call_a = pending_tool_call("retained-turn-call-a");
    let call_b = pending_tool_call("retained-turn-call-b");
    session
        .record_tool_call_batch_pending(
            turn_id,
            PendingToolCallBatch::new(
                ToolCallBatchId::new("retained-turn-batch").expect("valid batch id"),
                vec![call_a.clone(), call_b.clone()],
            )
            .expect("valid batch"),
        )
        .expect("tool calls record");
    session
        .close_model_response(turn_id, true)
        .expect("tool response closes");
    for (call, artifact, content) in [
        (
            &call_a,
            "retained-turn-result-a",
            "retained tool result a sentinel",
        ),
        (
            &call_b,
            "retained-turn-result-b",
            "retained tool result b sentinel",
        ),
    ] {
        session
            .submit_tool_result(
                ToolCallResult::succeeded(
                    call.id().clone(),
                    ArtifactRef::new(artifact_id(artifact), ArtifactKind::Text),
                ),
                ArtifactContent::text(content),
            )
            .expect("tool result records");
    }

    let input = session
        .build_test_citation_compaction_input(
            CitationCompactionPolicy::new(None, None, 1).expect("valid policy"),
        )
        .expect("input builds")
        .expect("old turn is compressible");
    let payload = input.to_model_payload_json().expect("payload serializes");

    assert!(payload.contains("covered old turn sentinel"));
    for retained in [
        "retained tool user sentinel",
        "retained tool commentary sentinel",
        "retained tool result a sentinel",
        "retained tool result b sentinel",
    ] {
        assert!(
            !payload.contains(retained),
            "retained complete turn leaked: {retained}"
        );
    }
}
