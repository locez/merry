use super::*;
use crate::CheckpointSourceKind;

#[test]
fn checkpoint_refs_record_user_and_assistant_transcript_items() {
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
        .build_test_citation_compaction_input(
            CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy"),
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
    assert_eq!(
        assistant.source_kind(),
        CheckpointSourceKind::AssistantMessage
    );
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
    session
        .submit_tool_result(
            ToolCallResult::succeeded(call_b.id().clone()),
            ToolOutput::json(r#"{"result":"b"}"#),
        )
        .expect("B resolves first");
    session
        .submit_tool_result(
            ToolCallResult::succeeded(call_a.id().clone()),
            ToolOutput::json(r#"{"result":"a"}"#),
        )
        .expect("A resolves second");
    session
        .record_test_user_message_body("retained raw tail")
        .expect("tail records");

    let input = session
        .build_test_citation_compaction_input(
            CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy"),
        )
        .expect("input builds")
        .expect("old history is compressible");
    let refs = input.manifest().refs();
    let ref_b = refs
        .iter()
        .find(|reference| reference.id().as_str() == "h3")
        .expect("B result ref exists");
    let ref_a = refs
        .iter()
        .find(|reference| reference.id().as_str() == "h4")
        .expect("A result ref exists");

    assert_eq!(ref_a.source_kind(), CheckpointSourceKind::ToolResult);
    assert_eq!(ref_b.source_kind(), CheckpointSourceKind::ToolResult);
}
