use crate::{
    compaction::CompactionWindowBudget,
    session::tests::{
        ArtifactContent, ArtifactKind, ArtifactRef, CitationCompactionPolicy, PendingToolCallBatch,
        SessionState, SessionStateTestExt, ToolCallBatchId, ToolCallResult, artifact_id,
        pending_tool_call,
    },
};

pub(super) fn policy(retained_model_turns: usize) -> CitationCompactionPolicy {
    CitationCompactionPolicy::new(None, None, retained_model_turns).expect("valid policy")
}

pub(super) fn window_budget(max_dynamic_body_tokens: u64) -> CompactionWindowBudget {
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

mod archive_evidence;

mod installation;

mod planning;
