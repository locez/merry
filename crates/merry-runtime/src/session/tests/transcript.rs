use super::*;
use crate::session::{Transcript, UserInputOrigin};

#[test]
fn transcript_assigns_monotonic_ids_and_never_reuses_after_retain() {
    let mut transcript = Transcript::new();

    let first = transcript
        .push_user_message("first user", UserInputOrigin::ExternalUser)
        .expect("first id should allocate");
    let second = transcript
        .push_user_message("second user", UserInputOrigin::ExternalUser)
        .expect("second id should allocate");

    assert_eq!(first.as_u64(), 0);
    assert_eq!(second.as_u64(), 1);

    transcript.retain_ids([second].into_iter().collect());

    let third = transcript
        .push_user_message("third user", UserInputOrigin::ExternalUser)
        .expect("third id should allocate");

    assert_eq!(third.as_u64(), 2);
    assert_eq!(
        transcript
            .items()
            .iter()
            .map(|item| item.id().as_u64())
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[test]
fn session_records_user_tool_result_assistant_and_second_user_in_transcript_order() {
    let mut session = SessionState::new(session_id());
    let call = pending_tool_call("call-order");

    session
        .record_user_message_body("first user")
        .expect("first user records");
    session
        .record_tool_call_pending(call.clone())
        .expect("tool call should become pending");
    let artifact = ArtifactRef::new(artifact_id("tool-result-order"), ArtifactKind::Text);
    let result = ToolCallResult::succeeded(call.id().clone(), artifact);
    session
        .submit_tool_result(result, ArtifactContent::text("tool output"))
        .expect("tool result records");
    session
        .record_assistant_text_output("assistant answer".to_owned())
        .expect("assistant records");
    session
        .record_user_message_body("second user")
        .expect("second user records");

    assert_eq!(
        session.transcript_items_for_tests(),
        vec![
            "user:first user",
            "tool_call:call-order",
            "tool_result:call-order:tool output",
            "assistant:assistant answer",
            "user:second user",
        ]
    );
}
