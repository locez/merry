use super::*;
use crate::{
    FileSessionStore,
    session::transcript::{
        PersistedTranscriptItem, ToolCallPromptProjection, ToolResultPromptProjection,
    },
};

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
              "claims": [
                {
                  "id": "c1",
                  "kind": "completed_action",
                  "text": "The older context and tool batch were compacted.",
                  "refs": ["r1", "r2", "r3"]
                }
              ],
              "working_intent": null
            }"#,
        )
        .expect("resolved batch checkpoint installs");

    assert_eq!(outcome.covered_history_item_count(), 3);
    assert_eq!(
        session.transcript_items_for_tests(),
        vec!["user:retained raw tail"]
    );
}

#[test]
fn permission_review_includes_hidden_final_output_but_compaction_window_skips_it() {
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
    assert_eq!(window.len(), 1);
    assert_eq!(window[0]["role"], "user");
    assert!(
        !payload
            .to_string()
            .contains("must-not-reenter-model-context")
    );

    let full = session
        .transcript_snapshot()
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
        .transcript_prompt_snapshot()
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
              "claims": [
                {
                  "id": "c1",
                  "kind": "completed_action",
                  "text": "The old visible context was compacted.",
                  "refs": ["r1"]
                }
              ],
              "working_intent": null
            }"#,
        )
        .expect("checkpoint installs");

    let full = session
        .transcript_snapshot()
        .expect("full transcript remains readable");
    assert_eq!(full.len(), 3);
    assert!(matches!(
        &full[0],
        crate::session::TranscriptItemSnapshot::ToolCall { call }
            if call.id() == final_call.id()
    ));
    assert!(matches!(
        &full[1],
        crate::session::TranscriptItemSnapshot::ToolResult { content, .. }
            if content.as_text()
                == Some(r#"{"private_final_output":"preserve-after-install"}"#)
    ));
    assert!(matches!(
        &full[2],
        crate::session::TranscriptItemSnapshot::UserMessage { text, .. }
            if text == "retained visible tail"
    ));

    let prompt = session
        .transcript_prompt_snapshot()
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
    assert_eq!(stored_items.len(), 3);
    assert_eq!(stored_items[0]["type"], "tool_call");
    assert_eq!(stored_items[1]["type"], "tool_result");
    assert_eq!(stored_items[2]["type"], "user_message");

    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    session.save_to(&store).await.expect("session saves");
    let loaded = SessionState::load_from(&store, &session_id)
        .await
        .expect("compacted session loads");
    let loaded_transcript = loaded.transcript.persisted();
    assert!(matches!(
        &loaded_transcript.items[0],
        PersistedTranscriptItem::ToolCall {
            call,
            prompt_projection: ToolCallPromptProjection::Hidden,
            ..
        } if call.id() == final_call.id()
    ));
    assert!(matches!(
        &loaded_transcript.items[1],
        PersistedTranscriptItem::ToolResult {
            call_id,
            prompt_projection: ToolResultPromptProjection::Hidden,
            ..
        } if call_id == final_call.id()
    ));
    assert!(matches!(
        &loaded
            .transcript_snapshot()
            .expect("loaded full transcript remains readable")[1],
        crate::session::TranscriptItemSnapshot::ToolResult { content, .. }
            if content.as_text()
                == Some(r#"{"private_final_output":"preserve-after-install"}"#)
    ));
    assert_eq!(
        loaded
            .transcript_prompt_snapshot()
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
fn rolling_compaction_candidate_can_cite_prior_claim_and_new_window_ref() {
    let mut session =
        SessionState::new(SessionId::new("rolling-install").expect("valid session id"));
    let checkpoint = citation_plain_runtime_checkpoint_for_tests(
        "checkpoint-existing",
        "Runtime cannot validate open semantic truth.",
    );
    session.set_compacted_checkpoint(checkpoint);
    session
        .record_test_user_message_body("new compacted work")
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
    let checkpoint_id = input.manifest().checkpoint_id().clone();

    session
            .install_citation_compaction_candidate(
                input,
                r#"{
                  "claims": [
                    {
                      "id": "c2",
                      "kind": "constraint",
                      "text": "Carry the prior semantic-validation constraint while adding new compacted work.",
                      "refs": ["prior-c1", "r1"]
                    }
                  ],
                  "working_intent": null
                }"#,
            )
            .expect("install succeeds with prior and new refs");

    let prior_excerpt = session
        .read_checkpoint_ref(
            &checkpoint_id,
            &CheckpointRefId::new("prior-c1").expect("valid ref id"),
        )
        .expect("prior claim ref remains inspectable");
    let new_excerpt = session
        .read_checkpoint_ref(
            &checkpoint_id,
            &CheckpointRefId::new("r1").expect("valid ref id"),
        )
        .expect("new window ref remains inspectable");

    assert_eq!(
        prior_excerpt.source_kind(),
        CheckpointSourceKind::PriorCheckpointClaim
    );
    assert!(prior_excerpt.excerpt().contains("open semantic truth"));
    assert_eq!(new_excerpt.source_kind(), CheckpointSourceKind::UserMessage);
    assert_eq!(new_excerpt.excerpt(), "new compacted work");
}

#[test]
fn installing_valid_checkpoint_removes_only_covered_history() {
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
          "claims": [
            {
              "id": "c1",
              "kind": "completed_action",
              "text": "The older user and assistant messages were covered by compaction.",
              "refs": ["r1", "r2"]
            }
          ],
          "working_intent": null
        }"#;

    let outcome = session
        .install_citation_compaction_candidate(input, candidate_json)
        .expect("install succeeds");

    assert_eq!(outcome.covered_history_item_count(), 2);
    assert_eq!(session.transcript_items_for_tests(), vec!["user:tail user"]);
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
          "claims": [
            {
              "id": "c1",
              "kind": "constraint",
              "text": "This cites a missing ref.",
              "refs": ["r-missing"]
            }
          ],
          "working_intent": null
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
