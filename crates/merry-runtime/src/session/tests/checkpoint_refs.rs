use super::*;

#[test]
fn checkpoint_refs_point_to_original_user_and_assistant_artifacts() {
    let mut session = SessionState::new(
        SessionId::new("checkpoint-direct-discussion-refs").expect("valid session id"),
    );
    let covered_turn = session.begin_model_turn().expect("covered turn begins");
    session
        .record_user_message_body(covered_turn, "exact user source")
        .expect("user message records");
    session
        .record_assistant_text_output(covered_turn, "exact assistant source".to_owned())
        .expect("assistant output records");
    session
        .close_model_response(covered_turn, false)
        .expect("covered turn completes");
    session
        .record_test_user_message_body("retained tail")
        .expect("tail records");

    let input = session
        .build_citation_compaction_input(
            CitationCompactionPolicy::new(128, None, 4096, 1, 1200, 16).expect("valid policy"),
        )
        .expect("input builds")
        .expect("covered turn is compressible");
    let refs = input.manifest().refs();

    let user = refs
        .iter()
        .find(|reference| reference.id().as_str() == "h0")
        .expect("user ref should use transcript item id");
    let assistant = refs
        .iter()
        .find(|reference| reference.id().as_str() == "h1")
        .expect("assistant ref should use transcript item id");
    assert_eq!(user.source_kind(), CheckpointSourceKind::UserMessage);
    assert_eq!(user.evidence().artifact_id.as_str(), "user-message-0");
    assert!(user.evidence().locator.is_whole_artifact());
    assert_eq!(
        assistant.source_kind(),
        CheckpointSourceKind::AssistantMessage
    );
    assert!(
        assistant
            .evidence()
            .artifact_id
            .as_str()
            .starts_with("assistant-output-")
    );
    assert!(assistant.evidence().locator.is_whole_artifact());
}

#[test]
fn checkpoint_tool_result_refs_preserve_b_after_a_result_identity() {
    let mut session = SessionState::new(
        SessionId::new("checkpoint-direct-tool-result-refs").expect("valid session id"),
    );
    session
        .record_test_user_message_body("old user context")
        .expect("user records");
    let call_a = pending_tool_call("direct-ref-call-a");
    let call_b = pending_tool_call("direct-ref-call-b");
    session
        .record_test_tool_call_batch_pending(
            PendingToolCallBatch::new(
                ToolCallBatchId::new("direct-ref-batch").expect("valid batch id"),
                vec![call_a.clone(), call_b.clone()],
            )
            .expect("valid batch"),
        )
        .expect("batch records");
    let artifact_b = ArtifactRef::new(artifact_id("direct-ref-artifact-b"), ArtifactKind::Json);
    session
        .submit_tool_result(
            ToolCallResult::succeeded(call_b.id().clone(), artifact_b),
            ArtifactContent::json(r#"{"result":"b"}"#),
        )
        .expect("B resolves first");
    let artifact_a = ArtifactRef::new(artifact_id("direct-ref-artifact-a"), ArtifactKind::Json);
    session
        .submit_tool_result(
            ToolCallResult::succeeded(call_a.id().clone(), artifact_a),
            ArtifactContent::json(r#"{"result":"a"}"#),
        )
        .expect("A resolves second");
    session
        .record_test_user_message_body("retained raw tail")
        .expect("tail records");

    let input = session
        .build_citation_compaction_input(
            CitationCompactionPolicy::new(128, None, 4096, 1, 1200, 16).expect("valid policy"),
        )
        .expect("input builds")
        .expect("old history is compressible");
    let refs = input.manifest().refs();
    let ref_a = refs
        .iter()
        .find(|reference| reference.evidence().artifact_id.as_str() == "direct-ref-artifact-a")
        .expect("A result ref exists");
    let ref_b = refs
        .iter()
        .find(|reference| reference.evidence().artifact_id.as_str() == "direct-ref-artifact-b")
        .expect("B result ref exists");

    assert_eq!(ref_a.id().as_str(), "h4");
    assert_eq!(ref_b.id().as_str(), "h3");
    assert_eq!(ref_a.source_kind(), CheckpointSourceKind::ToolResult);
    assert_eq!(ref_b.source_kind(), CheckpointSourceKind::ToolResult);
}

#[test]
fn rolling_compaction_carries_original_refs_unchanged() {
    let mut session =
        SessionState::new(SessionId::new("rolling-original-ref-carry").expect("valid session id"));
    session
        .record_test_user_message_body("first exact source")
        .expect("first source records");
    session
        .record_test_user_message_body("first retained source")
        .expect("first tail records");
    let policy = CitationCompactionPolicy::new(128, None, 4096, 1, 1200, 16).expect("valid policy");
    let first_input = session
        .build_citation_compaction_input(policy)
        .expect("first input builds")
        .expect("first prefix is compressible");
    let original = first_input
        .manifest()
        .refs()
        .iter()
        .find(|reference| reference.id().as_str() == "h0")
        .expect("first direct ref exists")
        .clone();
    session
        .install_citation_compaction_candidate(
            first_input,
            r#"{
              "claims": [{
                "id": "c1",
                "kind": "constraint",
                "text": "Keep the first exact source.",
                "refs": ["h0"]
              }],
              "working_intent": null
            }"#,
        )
        .expect("first checkpoint installs");
    session
        .record_test_user_message_body("second retained source")
        .expect("second tail records");

    let second_input = session
        .build_citation_compaction_input(policy)
        .expect("second input builds")
        .expect("former tail is compressible");
    let carried = second_input
        .manifest()
        .refs()
        .iter()
        .find(|reference| reference.id().as_str() == "h0")
        .expect("old direct ref is carried");
    assert_eq!(carried, &original);
    session
        .install_citation_compaction_candidate(
            second_input,
            r#"{
              "claims": [{
                "id": "c2",
                "kind": "constraint",
                "text": "Keep both exact sources.",
                "refs": ["h0", "h1"]
              }],
              "working_intent": null
            }"#,
        )
        .expect("second checkpoint installs");

    let installed = session
        .compacted_checkpoint
        .as_ref()
        .and_then(crate::CompactedCheckpoint::citation_backed)
        .expect("citation checkpoint is installed");
    let carried = installed
        .manifest()
        .refs()
        .iter()
        .find(|reference| reference.id().as_str() == "h0")
        .expect("installed checkpoint keeps old ref");
    assert_eq!(carried, &original);
}

#[test]
fn checkpoint_manifest_drops_refs_unused_by_candidate() {
    let mut session = SessionState::new(
        SessionId::new("checkpoint-unused-ref-filter").expect("valid session id"),
    );
    let covered_turn = session.begin_model_turn().expect("covered turn begins");
    session
        .record_user_message_body(covered_turn, "used source")
        .expect("user source records");
    session
        .record_assistant_text_output(covered_turn, "unused source".to_owned())
        .expect("assistant source records");
    session
        .close_model_response(covered_turn, false)
        .expect("covered turn completes");
    session
        .record_test_user_message_body("retained tail")
        .expect("tail records");
    let input = session
        .build_citation_compaction_input(
            CitationCompactionPolicy::new(128, None, 4096, 1, 1200, 16).expect("valid policy"),
        )
        .expect("input builds")
        .expect("covered turn is compressible");
    assert_eq!(input.manifest().refs().len(), 2);

    session
        .install_citation_compaction_candidate(
            input,
            r#"{
              "claims": [{
                "id": "c1",
                "kind": "constraint",
                "text": "Keep only the user source.",
                "refs": ["h0"]
              }],
              "working_intent": null
            }"#,
        )
        .expect("checkpoint installs");

    let installed = session
        .compacted_checkpoint
        .as_ref()
        .and_then(crate::CompactedCheckpoint::citation_backed)
        .expect("citation checkpoint is installed");
    assert_eq!(installed.manifest().refs().len(), 1);
    assert_eq!(installed.manifest().refs()[0].id().as_str(), "h0");
}
