use crate::{
    FileSessionStore,
    session::{
        tests::{
            ArtifactContent, ArtifactKind, ArtifactRef, CitationCompactionPolicy, ModelTurnStatus,
            PendingToolCallBatch, SessionId, SessionState, SessionStateTestExt, ToolCallBatchId,
            ToolCallResult, TranscriptItem, artifact_id, pending_tool_call,
        },
        transcript::{
            PersistedTranscriptItem, ToolCallPromptProjection, ToolResultPromptProjection,
        },
    },
};

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
        .build_test_citation_compaction_input(
            CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy"),
        )
        .expect("input builds")
        .expect("tool turn is compressible");
    let payload: serde_json::Value =
        serde_json::from_str(&input.to_model_payload_json().expect("payload serializes"))
            .expect("payload parses");
    let window = payload["window"].as_array().expect("window is an array");
    assert_eq!(window.len(), 1);
    let roles = window[0]["items"]
        .as_array()
        .expect("turn items are an array")
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
    let notice = match &provider[1] {
        crate::session::TranscriptItemSnapshot::ToolResult { content, .. } => {
            serde_json::from_str::<serde_json::Value>(
                content.as_text().expect("artifact notice is textual JSON"),
            )
            .expect("artifact notice parses")
        }
        other => panic!("expected tool result, got {other:?}"),
    };
    assert_eq!(notice["merry_archived"], true);
    assert_eq!(notice["status"], "succeeded");
    assert_eq!(notice["artifact_id"], "artifact-notice-result");
    assert!(
        notice["ref"]
            .as_str()
            .is_some_and(|value| value.starts_with('h'))
    );

    let input = session
        .build_test_citation_compaction_input(
            CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy"),
        )
        .expect("input builds")
        .expect("tool turn is compressible");
    let payload = input.to_model_payload_json().expect("payload serializes");
    assert!(payload.contains(exact_content));
    assert!(!payload.contains("merry_archived"));

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
            images: Vec::new(),
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
        .build_test_citation_compaction_input(
            CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy"),
        )
        .expect("resolved batch must not look stale")
        .expect("old history is compressible");
    let payload = serde_json::from_str::<serde_json::Value>(
        &input.to_model_payload_json().expect("payload serializes"),
    )
    .expect("payload parses");
    let window = payload["window"].as_array().expect("window");
    assert_eq!(window.len(), 2);
    let tool_items = window[1]["items"].as_array().expect("tool turn items");
    assert_eq!(tool_items.len(), 2);
    assert_eq!(tool_items[0]["role"], "tool_exchange");
    assert_eq!(tool_items[1]["role"], "tool_exchange");

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

    assert_eq!(outcome.covered_model_turn_count(), 2);
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
            images: Vec::new(),
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
        .build_test_citation_compaction_input(
            CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy"),
        )
        .expect("compaction input builds")
        .expect("old visible context should be compressible");
    let payload = serde_json::from_str::<serde_json::Value>(
        &input.to_model_payload_json().expect("payload serializes"),
    )
    .expect("payload parses");
    let window = payload["window"].as_array().expect("window is an array");
    assert_eq!(window.len(), 2);
    assert_eq!(window[0]["items"][0]["role"], "user");
    assert_eq!(window[1]["items"][0]["role"], "tool_exchange");
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

    let policy = CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy");
    let input = session
        .build_test_citation_compaction_input(policy)
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
            images: Vec::new(),
            origin: crate::session::UserInputOrigin::ExternalUser,
        }]
    );

    assert!(
        loaded
            .build_test_citation_compaction_input(policy)
            .expect("next compaction window builds")
            .is_none(),
        "the preserved hidden exchange must not be covered again"
    );
}
