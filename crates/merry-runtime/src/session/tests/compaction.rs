use super::*;

#[test]
fn compaction_input_excludes_retained_raw_tail() {
    let mut session =
        SessionState::new(SessionId::new("compaction-input-tail").expect("valid session id"));
    session.set_task_anchor(TaskAnchor::new("Keep the current task").expect("valid anchor"));
    session
        .record_user_message_body("old user message to compact")
        .expect("user records");
    session
        .record_assistant_text_output("old assistant message to compact".to_owned())
        .expect("assistant records");
    session
        .record_user_message_body("retained raw tail user sentinel")
        .expect("user records");
    session
        .record_assistant_text_output("retained raw tail assistant sentinel".to_owned())
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
        .record_user_message_body("old user context")
        .expect("user records");
    let call_a = pending_tool_call("batch-call-a");
    let call_b = pending_tool_call("batch-call-b");
    session
        .record_tool_call_batch_pending(
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
        .record_user_message_body("retained raw tail")
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
fn compaction_retained_raw_tail_is_policy_driven() {
    let mut session =
        SessionState::new(SessionId::new("retained-tail-policy").expect("valid session id"));
    session
        .record_user_message_body("covered user sentinel")
        .expect("user records");
    session
        .record_assistant_text_output("covered assistant sentinel".to_owned())
        .expect("assistant records");
    session
        .record_user_message_body("tail user one sentinel")
        .expect("user records");
    session
        .record_assistant_text_output("tail assistant one sentinel".to_owned())
        .expect("assistant records");
    session
        .record_user_message_body("tail user two sentinel")
        .expect("user records");
    session
        .record_assistant_text_output("tail assistant two sentinel".to_owned())
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
        .record_user_message_body("new user message to compact")
        .expect("user records");
    session
        .record_user_message_body("retained tail")
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
        .record_user_message_body("new compacted work")
        .expect("user records");
    session
        .record_user_message_body("retained tail")
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
        .record_user_message_body("old user")
        .expect("user records");
    session
        .record_assistant_text_output("old assistant".to_owned())
        .expect("assistant records");
    session
        .record_user_message_body("tail user")
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
        .record_user_message_body("old user")
        .expect("user records");
    session
        .record_user_message_body("tail user")
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
