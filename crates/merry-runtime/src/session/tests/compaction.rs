use super::*;

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
        .build_citation_compaction_input(
            CitationCompactionPolicy::new(128, None, 4096, 1, 1200, 16).expect("valid policy"),
        )
        .expect("input builds")
        .expect("covered turn is compressible");
    let payload: serde_json::Value =
        serde_json::from_str(&input.to_model_payload_json().expect("payload serializes"))
            .expect("payload parses");

    let window = payload["window"].as_array().expect("window is an array");
    assert_eq!(window.len(), 2);
    let payload = payload.to_string();
    assert!(payload.contains("covered user"));
    assert!(payload.contains("covered assistant"));
    assert!(!payload.contains("retained user"));
    assert!(!payload.contains("retained assistant"));
}

#[test]
fn compaction_install_advances_prompt_boundary_without_deleting_full_transcript() {
    let mut session =
        SessionState::new(SessionId::new("compaction-prompt-boundary").expect("valid session id"));
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
        .close_model_response(retained_turn, false)
        .expect("retained turn completes");

    let input = session
        .build_citation_compaction_input(
            CitationCompactionPolicy::new(128, None, 4096, 1, 1200, 16).expect("valid policy"),
        )
        .expect("input builds")
        .expect("covered turn is compressible");
    session
        .install_citation_compaction_candidate(
            input,
            r#"{
              "confirmed_decisions": [],
              "rejected_approaches": [],
              "constraints_preferences_boundaries": [],
              "corrected_misunderstandings": [],
              "durable_conclusions": [{
                "id": "c1",
                "text": "The first complete turn was compacted.",
                "refs": ["h0", "h1"]
              }],
              "open_questions": [],
              "current_progress_and_next_steps": [],
              "exact_details": [],
              "handoffs": []
            }"#,
        )
        .expect("checkpoint installs");

    assert_eq!(
        session.transcript_items_for_tests(),
        vec![
            "user:covered user",
            "assistant:covered assistant",
            "user:retained user",
        ]
    );
    assert_eq!(
        session
            .provider_transcript_snapshot()
            .expect("provider projection builds"),
        vec![crate::session::TranscriptItemSnapshot::UserMessage {
            text: "retained user".to_owned(),
            origin: crate::session::UserInputOrigin::ExternalUser,
        }]
    );
}

#[test]
fn compaction_install_advances_boundary_through_trailing_empty_aborted_turn() {
    let mut session = SessionState::new(
        SessionId::new("compaction-empty-aborted-boundary").expect("valid session id"),
    );
    let covered_turn = session.begin_model_turn().expect("covered turn begins");
    session
        .record_user_message_body(covered_turn, "covered before empty turn")
        .expect("covered user records");
    session
        .close_model_response(covered_turn, false)
        .expect("covered turn completes");
    let empty_aborted_turn = session.begin_model_turn().expect("empty turn begins");
    session
        .abort_model_turn(empty_aborted_turn)
        .expect("empty turn aborts");
    let retained_turn = session.begin_model_turn().expect("retained turn begins");
    session
        .record_user_message_body(retained_turn, "retained after empty turn")
        .expect("retained user records");
    session
        .close_model_response(retained_turn, false)
        .expect("retained turn completes");

    let input = session
        .build_citation_compaction_input(
            CitationCompactionPolicy::new(128, None, 4096, 1, 1200, 16).expect("valid policy"),
        )
        .expect("input builds")
        .expect("covered prefix is compressible");
    session
        .install_citation_compaction_candidate(
            input,
            r#"{
              "confirmed_decisions": [],
              "rejected_approaches": [],
              "constraints_preferences_boundaries": [],
              "corrected_misunderstandings": [],
              "durable_conclusions": [{
                "id": "c1",
                "text": "The prefix before the retained turn was compacted.",
                "refs": ["h0"]
              }],
              "open_questions": [],
              "current_progress_and_next_steps": [],
              "exact_details": [],
              "handoffs": []
            }"#,
        )
        .expect("checkpoint installs");

    assert_eq!(
        session.prompt_history_projection().compacted_through(),
        Some(empty_aborted_turn)
    );
    assert_eq!(
        session
            .provider_transcript_snapshot()
            .expect("provider projection builds"),
        vec![crate::session::TranscriptItemSnapshot::UserMessage {
            text: "retained after empty turn".to_owned(),
            origin: crate::session::UserInputOrigin::ExternalUser,
        }]
    );
}

#[test]
fn rolling_compaction_rejects_old_input_and_starts_after_current_boundary() {
    let mut session =
        SessionState::new(SessionId::new("rolling-compaction-boundary").expect("valid session id"));
    session
        .record_test_user_message_body("first covered user")
        .expect("covered user records");
    session
        .record_test_user_message_body("first retained user")
        .expect("retained user records");
    let policy = CitationCompactionPolicy::new(128, None, 4096, 1, 1200, 16).expect("valid policy");
    let input = session
        .build_citation_compaction_input(policy)
        .expect("first input builds")
        .expect("first prefix is compressible");
    let stale_input = input.clone();
    let candidate = r#"{
      "confirmed_decisions": [],
      "rejected_approaches": [],
      "constraints_preferences_boundaries": [],
      "corrected_misunderstandings": [],
      "durable_conclusions": [{
        "id": "c1",
        "text": "The first user turn was compacted.",
        "refs": ["h0"]
      }],
      "open_questions": [],
      "current_progress_and_next_steps": [],
      "exact_details": [],
      "handoffs": []
    }"#;
    session
        .install_citation_compaction_candidate(input, candidate)
        .expect("first checkpoint installs");

    let error = session
        .install_citation_compaction_candidate(stale_input, candidate)
        .expect_err("the old input cannot cover the same turn twice");
    assert!(matches!(
        error,
        RuntimeError::Compaction {
            source: crate::CompactionError::StaleWindow
        }
    ));

    session
        .record_test_user_message_body("second retained user")
        .expect("new retained user records");
    let next_input = session
        .build_citation_compaction_input(policy)
        .expect("next input builds")
        .expect("the former tail is now compressible");
    let next_payload = next_input
        .to_model_payload_json()
        .expect("next payload serializes");
    assert!(next_payload.contains("first retained user"));
    assert!(!next_payload.contains("first covered user"));
    assert!(!next_payload.contains("second retained user"));
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
        .build_citation_compaction_input(
            CitationCompactionPolicy::new(128, None, 4096, 1, 1200, 16).expect("valid policy"),
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
fn compaction_groups_user_commentary_and_two_tool_pairs_in_one_turn() {
    let mut session =
        SessionState::new(SessionId::new("compaction-tool-turn").expect("valid session id"));
    let turn_id = session.begin_model_turn().expect("tool turn begins");
    session
        .record_user_message_body(turn_id, "inspect both files")
        .expect("user message records");
    session
        .record_assistant_text_output(turn_id, "I will inspect both files.".to_owned())
        .expect("commentary records");
    let call_a = pending_tool_call("turn-call-a");
    let call_b = pending_tool_call("turn-call-b");
    session
        .record_tool_call_batch_pending(
            turn_id,
            PendingToolCallBatch::new(
                ToolCallBatchId::new("turn-batch").expect("valid batch id"),
                vec![call_a.clone(), call_b.clone()],
            )
            .expect("valid batch"),
        )
        .expect("batch records");
    session
        .close_model_response(turn_id, true)
        .expect("tool response closes");
    session
        .submit_tool_result(
            ToolCallResult::succeeded(
                call_b.id().clone(),
                ArtifactRef::new(artifact_id("turn-result-b"), ArtifactKind::Text),
            ),
            ArtifactContent::text("result b"),
        )
        .expect("second call resolves first");
    session
        .submit_tool_result(
            ToolCallResult::succeeded(
                call_a.id().clone(),
                ArtifactRef::new(artifact_id("turn-result-a"), ArtifactKind::Text),
            ),
            ArtifactContent::text("result a"),
        )
        .expect("first call resolves second");

    let retained_turn = session.begin_model_turn().expect("retained turn begins");
    session
        .record_user_message_body(retained_turn, "retained user")
        .expect("retained user records");
    session
        .close_model_response(retained_turn, false)
        .expect("retained turn completes");

    let turns = session
        .transcript
        .model_turns()
        .expect("complete model turns group");
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].id(), turn_id);
    assert_eq!(turns[0].status(), ModelTurnStatus::Completed);
    assert_eq!(turns[0].items().len(), 6);
    assert!(matches!(
        turns[0].items()[2],
        TranscriptItem::ToolCall { call, model_turn_id, .. }
            if call.id() == call_a.id() && *model_turn_id == turn_id
    ));
    assert!(matches!(
        turns[0].items()[3],
        TranscriptItem::ToolCall { call, model_turn_id, .. }
            if call.id() == call_b.id() && *model_turn_id == turn_id
    ));
    assert!(matches!(
        turns[0].items()[4],
        TranscriptItem::ToolResult { call_id, model_turn_id, .. }
            if call_id == call_b.id() && *model_turn_id == turn_id
    ));
    assert!(matches!(
        turns[0].items()[5],
        TranscriptItem::ToolResult { call_id, model_turn_id, .. }
            if call_id == call_a.id() && *model_turn_id == turn_id
    ));

    let input = session
        .build_citation_compaction_input(
            CitationCompactionPolicy::new(128, None, 4096, 1, 1200, 16).expect("valid policy"),
        )
        .expect("input builds")
        .expect("tool turn is compressible");
    let payload: serde_json::Value =
        serde_json::from_str(&input.to_model_payload_json().expect("payload serializes"))
            .expect("payload parses");
    let roles = payload["window"]
        .as_array()
        .expect("window is an array")
        .iter()
        .map(|item| item["role"].as_str().expect("role is text"))
        .collect::<Vec<_>>();

    assert_eq!(
        roles,
        ["user", "assistant", "tool_exchange", "tool_exchange"]
    );
    let payload = payload.to_string();
    assert!(payload.contains("result a"));
    assert!(payload.contains("result b"));
    assert!(!payload.contains("retained user"));
}
use crate::{
    FileSessionStore,
    session::{
        ModelTurnId,
        transcript::{
            PersistedTranscriptItem, ToolCallPromptProjection, ToolResultPromptProjection,
            Transcript, TranscriptItemId,
        },
    },
};

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
            next_model_turn_id,
        };

        let error = session
            .build_citation_compaction_input(
                CitationCompactionPolicy::new(128, None, 4096, 1, 1200, 16).expect("valid policy"),
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
fn artifact_notice_is_provider_only_and_compaction_reads_exact_content() {
    let mut session =
        SessionState::new(SessionId::new("compaction-artifact-notice").expect("valid session id"));
    let call = pending_tool_call("artifact-notice-call");
    session
        .record_test_tool_call_pending(call.clone())
        .expect("tool call records");
    let result_artifact_id = artifact_id("artifact-notice-result");
    let exact_content = "exact artifact notice source content";
    session
        .submit_tool_result(
            ToolCallResult::succeeded(
                call.id().clone(),
                ArtifactRef::new(result_artifact_id.clone(), ArtifactKind::Text),
            ),
            ArtifactContent::text(exact_content),
        )
        .expect("tool result records");
    let result_projection = session
        .transcript
        .items
        .iter_mut()
        .find_map(|item| match item {
            TranscriptItem::ToolResult {
                call_id,
                prompt_projection,
                ..
            } if call_id == call.id() => Some(prompt_projection),
            _ => None,
        });
    *result_projection.expect("tool result projection exists") =
        ToolResultPromptProjection::ArtifactNotice;
    session
        .record_test_user_message_body("retained artifact notice tail")
        .expect("retained user records");

    let provider = session
        .provider_transcript_snapshot()
        .expect("provider projection builds");
    assert_eq!(provider.len(), 3);
    assert!(matches!(
        &provider[0],
        crate::session::TranscriptItemSnapshot::ToolCall { call: provider_call }
            if provider_call.id() == call.id()
    ));
    assert!(matches!(
        &provider[1],
        crate::session::TranscriptItemSnapshot::ToolResult { content, .. }
            if content.as_text()
                == Some("Tool result content is stored in artifact artifact-notice-result.")
    ));

    let input = session
        .build_citation_compaction_input(
            CitationCompactionPolicy::new(128, None, 4096, 1, 1200, 16).expect("valid policy"),
        )
        .expect("input builds")
        .expect("tool turn is compressible");
    let payload = input.to_model_payload_json().expect("payload serializes");
    assert!(payload.contains(exact_content));
    assert!(!payload.contains("Tool result content is stored in artifact"));

    session
        .install_citation_compaction_candidate(
            input,
            r#"{
              "confirmed_decisions": [],
              "rejected_approaches": [],
              "constraints_preferences_boundaries": [],
              "corrected_misunderstandings": [],
              "durable_conclusions": [{
                "id": "c1",
                "text": "The exact tool exchange was compacted.",
                "refs": ["h1"]
              }],
              "open_questions": [],
              "current_progress_and_next_steps": [],
              "exact_details": [],
              "handoffs": []
            }"#,
        )
        .expect("checkpoint installs");

    assert_eq!(
        session
            .provider_transcript_snapshot()
            .expect("provider projection rebuilds"),
        vec![crate::session::TranscriptItemSnapshot::UserMessage {
            text: "retained artifact notice tail".to_owned(),
            origin: crate::session::UserInputOrigin::ExternalUser,
        }]
    );
    assert!(matches!(
        &session
            .full_transcript_snapshot()
            .expect("full transcript remains exact")[1],
        crate::session::TranscriptItemSnapshot::ToolResult { content, .. }
            if content.as_text() == Some(exact_content)
    ));
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

    let policy = CitationCompactionPolicy::new(128, None, 4096, 2, 1200, 16).expect("valid policy");
    let input = session
        .build_citation_compaction_input(policy)
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
fn compaction_accepts_resolved_multi_tool_batches() {
    let mut session =
        SessionState::new(SessionId::new("compaction-tool-batch").expect("valid session id"));
    session
        .record_test_user_message_body("old user context")
        .expect("user records");
    let call_a = pending_tool_call("batch-call-a");
    let call_b = pending_tool_call("batch-call-b");
    session
        .record_test_tool_call_batch_pending(
            PendingToolCallBatch::new(
                ToolCallBatchId::new("tool-batch-compaction").expect("valid batch id"),
                vec![call_a.clone(), call_b.clone()],
            )
            .expect("valid batch"),
        )
        .expect("batch records");
    let artifact_b = ArtifactRef::new(artifact_id("artifact-b"), ArtifactKind::Json);
    session
        .submit_tool_result(
            ToolCallResult::succeeded(call_b.id().clone(), artifact_b),
            ArtifactContent::json(r#"{"result":"b"}"#),
        )
        .expect("second call resolves first");
    let artifact_a = ArtifactRef::new(artifact_id("artifact-a"), ArtifactKind::Json);
    session
        .submit_tool_result(
            ToolCallResult::succeeded(call_a.id().clone(), artifact_a),
            ArtifactContent::json(r#"{"result":"a"}"#),
        )
        .expect("first call resolves second");
    session
        .record_test_user_message_body("retained raw tail")
        .expect("tail records");

    let input = session
        .build_citation_compaction_input(
            CitationCompactionPolicy::new(128, None, 4096, 1, 1200, 16).expect("valid policy"),
        )
        .expect("resolved batch must not look stale")
        .expect("old history is compressible");
    let payload = serde_json::from_str::<serde_json::Value>(
        &input.to_model_payload_json().expect("payload serializes"),
    )
    .expect("payload parses");
    assert_eq!(payload["window"].as_array().expect("window").len(), 3);
    assert_eq!(payload["window"][1]["role"], "tool_exchange");
    assert_eq!(payload["window"][2]["role"], "tool_exchange");

    let outcome = session
        .install_citation_compaction_candidate(
            input,
            r#"{
              "confirmed_decisions": [],
              "rejected_approaches": [],
              "constraints_preferences_boundaries": [],
              "corrected_misunderstandings": [],
              "durable_conclusions": [
                {
                  "id": "c1",
                  "text": "The older context and tool batch were compacted.",
                  "refs": ["h0", "h4", "h3"]
                }
              ],
              "open_questions": [],
              "current_progress_and_next_steps": [],
              "exact_details": [],
              "handoffs": []
            }"#,
        )
        .expect("resolved batch checkpoint installs");

    assert_eq!(outcome.covered_history_item_count(), 3);
    assert_eq!(
        session.transcript_items_for_tests(),
        vec![
            "user:old user context",
            "tool_call:batch-call-a",
            "tool_call:batch-call-b",
            "tool_result:batch-call-b:{\"result\":\"b\"}",
            "tool_result:batch-call-a:{\"result\":\"a\"}",
            "user:retained raw tail",
        ]
    );
    assert_eq!(
        session
            .provider_transcript_snapshot()
            .expect("provider projection builds"),
        vec![crate::session::TranscriptItemSnapshot::UserMessage {
            text: "retained raw tail".to_owned(),
            origin: crate::session::UserInputOrigin::ExternalUser,
        }]
    );
}

#[test]
fn compaction_uses_full_hidden_exchange_while_provider_projection_skips_it() {
    let mut session =
        SessionState::new(SessionId::new("compaction-hidden-final").expect("valid session id"));
    session
        .record_test_user_message_body("old visible context")
        .expect("old user context records");
    let final_call = pending_tool_call("hidden-final-call");
    session
        .record_test_tool_call_pending(final_call.clone())
        .expect("final-output call records");
    session
        .record_final_output(
            final_call.id().clone(),
            r#"{"private_final_output":"must-not-reenter-model-context"}"#.to_owned(),
        )
        .expect("final output records");
    session
        .record_test_user_message_body("retained visible tail")
        .expect("tail records");

    let review_context = session
        .permission_review_context_snapshot()
        .expect("permission context builds");
    let review_debug = format!("{review_context:?}");
    assert!(review_debug.contains("tool_call:lookup"));
    assert!(review_debug.contains("must-not-reenter-model-context"));

    let input = session
        .build_citation_compaction_input(
            CitationCompactionPolicy::new(128, None, 4096, 1, 1200, 16).expect("valid policy"),
        )
        .expect("compaction input builds")
        .expect("old visible context should be compressible");
    let payload = serde_json::from_str::<serde_json::Value>(
        &input.to_model_payload_json().expect("payload serializes"),
    )
    .expect("payload parses");
    let window = payload["window"].as_array().expect("window is an array");
    assert_eq!(window.len(), 2);
    assert_eq!(window[0]["role"], "user");
    assert_eq!(window[1]["role"], "tool_exchange");
    assert!(
        payload
            .to_string()
            .contains("must-not-reenter-model-context")
    );

    let full = session
        .full_transcript_snapshot()
        .expect("full transcript builds");
    assert_eq!(full.len(), 4);
    assert!(matches!(
        &full[1],
        crate::session::TranscriptItemSnapshot::ToolCall { call }
            if call.id() == final_call.id()
    ));
    assert!(matches!(
        &full[2],
        crate::session::TranscriptItemSnapshot::ToolResult { content, .. }
            if content.as_text()
                == Some(r#"{"private_final_output":"must-not-reenter-model-context"}"#)
    ));

    let prompt = session
        .provider_transcript_snapshot()
        .expect("prompt transcript builds");
    assert_eq!(prompt.len(), 2);
    assert!(prompt.iter().all(|item| matches!(
        item,
        crate::session::TranscriptItemSnapshot::UserMessage { .. }
    )));
}

#[tokio::test]
async fn installing_compaction_preserves_hidden_final_output_in_full_and_stored_transcript() {
    let session_id = SessionId::new("install-hidden-final").expect("valid session id");
    let mut session = SessionState::new(session_id.clone());
    session
        .record_test_user_message_body("old visible context")
        .expect("old user context records");
    let final_call = pending_tool_call("hidden-install-final-call");
    session
        .record_test_tool_call_pending(final_call.clone())
        .expect("final-output call records");
    session
        .record_final_output(
            final_call.id().clone(),
            r#"{"private_final_output":"preserve-after-install"}"#.to_owned(),
        )
        .expect("final output records");
    session
        .record_test_user_message_body("retained visible tail")
        .expect("tail records");

    let policy = CitationCompactionPolicy::new(128, None, 4096, 1, 1200, 16).expect("valid policy");
    let input = session
        .build_citation_compaction_input(policy)
        .expect("compaction input builds")
        .expect("old visible context should be compressible");
    session
        .install_citation_compaction_candidate(
            input,
            r#"{
              "confirmed_decisions": [],
              "rejected_approaches": [],
              "constraints_preferences_boundaries": [],
              "corrected_misunderstandings": [],
              "durable_conclusions": [
                {
                  "id": "c1",
                  "text": "The old visible context was compacted.",
                  "refs": ["h0"]
                }
              ],
              "open_questions": [],
              "current_progress_and_next_steps": [],
              "exact_details": [],
              "handoffs": []
            }"#,
        )
        .expect("checkpoint installs");

    let full = session
        .full_transcript_snapshot()
        .expect("full transcript remains readable");
    assert_eq!(full.len(), 4);
    assert!(matches!(
        &full[0],
        crate::session::TranscriptItemSnapshot::UserMessage { text, .. }
            if text == "old visible context"
    ));
    assert!(matches!(
        &full[1],
        crate::session::TranscriptItemSnapshot::ToolCall { call }
            if call.id() == final_call.id()
    ));
    assert!(matches!(
        &full[2],
        crate::session::TranscriptItemSnapshot::ToolResult { content, .. }
            if content.as_text()
                == Some(r#"{"private_final_output":"preserve-after-install"}"#)
    ));
    assert!(matches!(
        &full[3],
        crate::session::TranscriptItemSnapshot::UserMessage { text, .. }
            if text == "retained visible tail"
    ));

    let prompt = session
        .provider_transcript_snapshot()
        .expect("prompt transcript remains readable");
    assert_eq!(prompt.len(), 1);
    assert!(matches!(
        &prompt[0],
        crate::session::TranscriptItemSnapshot::UserMessage { text, .. }
            if text == "retained visible tail"
    ));

    let stored: serde_json::Value = serde_json::from_slice(
        &session
            .persistable_bundle()
            .expect("post-compaction session is persistable")
            .document_bytes,
    )
    .expect("stored session is JSON");
    let stored_items = stored["transcript"]["items"]
        .as_array()
        .expect("stored transcript items are an array");
    assert_eq!(stored_items.len(), 4);
    assert_eq!(stored_items[0]["type"], "user_message");
    assert_eq!(stored_items[1]["type"], "tool_call");
    assert_eq!(stored_items[2]["type"], "tool_result");
    assert_eq!(stored_items[3]["type"], "user_message");
    assert_eq!(stored["prompt_history_projection"]["compacted_through"], 2);
    assert!(
        stored["transcript"]
            .get("prompt_history_projection")
            .is_none()
    );

    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    session.save_to(&store).await.expect("session saves");
    let loaded = SessionState::load_from(&store, &session_id)
        .await
        .expect("compacted session loads");
    let loaded_transcript = loaded.transcript.persisted();
    assert!(matches!(
        &loaded_transcript.items[1],
        PersistedTranscriptItem::ToolCall {
            call,
            prompt_projection: ToolCallPromptProjection::Hidden,
            ..
        } if call.id() == final_call.id()
    ));
    assert!(matches!(
        &loaded_transcript.items[2],
        PersistedTranscriptItem::ToolResult {
            call_id,
            prompt_projection: ToolResultPromptProjection::Hidden,
            ..
        } if call_id == final_call.id()
    ));
    assert!(matches!(
        &loaded
            .full_transcript_snapshot()
            .expect("loaded full transcript remains readable")[2],
        crate::session::TranscriptItemSnapshot::ToolResult { content, .. }
            if content.as_text()
                == Some(r#"{"private_final_output":"preserve-after-install"}"#)
    ));
    assert_eq!(
        loaded
            .provider_transcript_snapshot()
            .expect("loaded prompt transcript remains readable"),
        vec![crate::session::TranscriptItemSnapshot::UserMessage {
            text: "retained visible tail".to_owned(),
            origin: crate::session::UserInputOrigin::ExternalUser,
        }]
    );

    assert!(
        loaded
            .build_citation_compaction_input(policy)
            .expect("next compaction window builds")
            .is_none(),
        "the preserved hidden exchange must not be covered again"
    );
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
        .build_citation_compaction_input(
            CitationCompactionPolicy::new(128, None, 4096, 4, 1200, 16).expect("valid policy"),
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
fn compaction_input_includes_previous_checkpoint_without_old_raw_body() {
    let mut session = SessionState::new(SessionId::new("rolling-input").expect("valid session id"));
    let checkpoint = citation_plain_runtime_checkpoint_for_tests(
        "checkpoint-existing",
        "The prior direction rejected resource timelines.",
    );
    session.set_compacted_checkpoint(checkpoint);
    session
        .record_test_user_message_body("new user message to compact")
        .expect("user records");
    session
        .record_test_user_message_body("retained tail")
        .expect("user records");

    let input = session
        .build_citation_compaction_input(
            CitationCompactionPolicy::new(128, None, 4096, 1, 1200, 16).expect("valid policy"),
        )
        .expect("input builds")
        .expect("input exists");
    let payload = input.to_model_payload_json().expect("payload serializes");

    assert!(payload.contains("previous_checkpoint"));
    assert!(payload.contains("The prior direction rejected resource timelines."));
    assert!(payload.contains("new user message to compact"));
    assert!(!payload.contains("retained tail"));
}

#[test]
fn installing_valid_checkpoint_hides_only_covered_history_from_provider() {
    let mut session =
        SessionState::new(SessionId::new("install-checkpoint").expect("valid session id"));
    session
        .record_test_user_message_body("old user")
        .expect("user records");
    session
        .record_test_assistant_text_output("old assistant".to_owned())
        .expect("assistant records");
    session
        .record_test_user_message_body("tail user")
        .expect("user records");

    let policy = CitationCompactionPolicy::new(128, None, 4096, 1, 1200, 16).expect("valid policy");
    let input = session
        .build_citation_compaction_input(policy)
        .expect("input builds")
        .expect("input exists");
    let candidate_json = r#"{
          "confirmed_decisions": [],
          "rejected_approaches": [],
          "constraints_preferences_boundaries": [],
          "corrected_misunderstandings": [],
          "durable_conclusions": [
            {
              "id": "c1",
              "text": "The older user and assistant messages were covered by compaction.",
              "refs": ["h0", "h1"]
            }
          ],
          "open_questions": [],
          "current_progress_and_next_steps": [],
          "exact_details": [],
          "handoffs": []
        }"#;

    let outcome = session
        .install_citation_compaction_candidate(input, candidate_json)
        .expect("install succeeds");

    assert_eq!(outcome.covered_history_item_count(), 2);
    assert_eq!(
        session.transcript_items_for_tests(),
        vec!["user:old user", "assistant:old assistant", "user:tail user"]
    );
    assert_eq!(
        session
            .provider_transcript_snapshot()
            .expect("provider projection builds"),
        vec![crate::session::TranscriptItemSnapshot::UserMessage {
            text: "tail user".to_owned(),
            origin: crate::session::UserInputOrigin::ExternalUser,
        }]
    );
    assert!(
        session
            .context_snapshot()
            .compacted_checkpoint_for_tests()
            .is_some()
    );
}

#[test]
fn failed_checkpoint_install_keeps_history_unchanged() {
    let mut session =
        SessionState::new(SessionId::new("install-checkpoint-rollback").expect("valid session id"));
    session
        .record_test_user_message_body("old user")
        .expect("user records");
    session
        .record_test_user_message_body("tail user")
        .expect("user records");

    let policy = CitationCompactionPolicy::new(128, None, 4096, 1, 1200, 16).expect("valid policy");
    let input = session
        .build_citation_compaction_input(policy)
        .expect("input builds")
        .expect("input exists");
    let bad_candidate_json = r#"{
          "confirmed_decisions": [],
          "rejected_approaches": [],
          "constraints_preferences_boundaries": [
            {
              "id": "c1",
              "text": "This cites a missing ref.",
              "refs": ["r-missing"]
            }
          ],
          "corrected_misunderstandings": [],
          "durable_conclusions": [],
          "open_questions": [],
          "current_progress_and_next_steps": [],
          "exact_details": [],
          "handoffs": []
        }"#;

    let error = session
        .install_citation_compaction_candidate(input, bad_candidate_json)
        .expect_err("bad candidate must fail");

    assert!(matches!(error, RuntimeError::Checkpoint { .. }));
    assert_eq!(
        session.transcript_items_for_tests(),
        vec!["user:old user", "user:tail user"]
    );
}
