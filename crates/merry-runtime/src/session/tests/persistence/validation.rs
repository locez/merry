use crate::{
    FileSessionStore,
    session::tests::{
        SessionId, SessionState, SessionStateTestExt, pending_tool_call,
        persistence::rewrite_journal_commit, session_id,
    },
};
use merry_core::{ToolCallResult, ToolOutput};

#[tokio::test]
async fn session_state_save_rejects_pending_tool_calls() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());

    session
        .record_test_tool_call_pending(pending_tool_call("pending-save"))
        .expect("pending records");

    let error = session
        .save_to(&store)
        .await
        .expect_err("pending save rejected");
    assert!(error.to_string().contains("pending tool calls"));
}

#[tokio::test]
async fn session_state_load_rejects_session_id_mismatch() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());
    session.save_to(&store).await.expect("session saves");

    let other = SessionId::new("other-session").expect("valid session id");
    let bytes = store
        .read_state_bytes(&session_id())
        .await
        .expect("saved state reads");
    store
        .write_state_bytes(&other, &bytes)
        .await
        .expect("mismatched state writes");

    let error = SessionState::load_from(&store, &other)
        .await
        .expect_err("mismatch fails");
    assert!(
        error
            .to_string()
            .contains("does not match requested session")
    );
}

#[test]
fn session_state_save_rejects_in_progress_model_turn() {
    let mut session = SessionState::new(session_id());
    session
        .begin_model_turn()
        .expect("in-progress turn fixture should begin");

    let error = session
        .persistable_bundle()
        .expect_err("in-progress model turns are not resume-safe");

    assert!(matches!(
        error,
        crate::SessionStoreError::InvalidDocument { .. }
    ));
}

#[test]
fn session_state_save_rejects_prompt_projection_without_checkpoint() {
    let mut session = SessionState::new(session_id());
    let turn_id = session
        .begin_model_turn()
        .expect("completed turn fixture should begin");
    session
        .record_user_message_body(turn_id, "covered without checkpoint")
        .expect("user message should record");
    session
        .close_model_response(turn_id, false)
        .expect("turn should complete");
    session
        .advance_prompt_history_projection(turn_id)
        .expect("in-memory install stage may advance the projection");

    let error = session
        .persistable_bundle()
        .expect_err("a projection without its checkpoint is not resume-safe");

    assert!(matches!(
        error,
        crate::SessionStoreError::InvalidDocument { .. }
    ));
}

#[tokio::test]
async fn session_state_load_rejects_unreachable_model_turn_sequences() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let cases = [
        ("empty map with advanced counter", serde_json::json!([]), 2),
        (
            "gap in positive turn ids",
            serde_json::json!([
                {"id": 1, "status": "completed"},
                {"id": 3, "status": "completed"}
            ]),
            4,
        ),
        (
            "counter jump after contiguous turns",
            serde_json::json!([{"id": 1, "status": "completed"}]),
            3,
        ),
    ];

    SessionState::new(session_id())
        .save_to(&store)
        .await
        .expect("empty session saves");
    let saved = store
        .read_state_bytes(&session_id())
        .await
        .expect("journal reads");
    for (case, model_turns, next_model_turn_id) in cases {
        let bytes = rewrite_journal_commit(&saved, |commit| {
            commit["state"]["model_turns"] = model_turns.clone();
            commit["state"]["next_model_turn_id"] = serde_json::Value::from(next_model_turn_id);
        });
        store
            .write_state_bytes(&session_id(), &bytes)
            .await
            .expect("corrupted current state writes");

        assert!(
            matches!(
                SessionState::load_from(&store, &session_id())
                    .await
                    .expect_err("unreachable model turn sequence must reject resume"),
                crate::SessionStoreError::InvalidDocument { .. }
            ),
            "case {case}"
        );
    }
}

#[tokio::test]
async fn session_state_load_rejects_nonterminal_or_unresolved_turns() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    SessionState::new(session_id())
        .save_to(&store)
        .await
        .expect("empty session saves");
    let saved = store
        .read_state_bytes(&session_id())
        .await
        .expect("state reads");

    let in_progress = rewrite_journal_commit(&saved, |commit| {
        commit["state"]["model_turns"] = serde_json::json!([{"id": 1, "status": "in_progress"}]);
        commit["state"]["next_model_turn_id"] = serde_json::json!(2);
    });
    store
        .write_state_bytes(&session_id(), &in_progress)
        .await
        .expect("in-progress state writes");
    assert!(matches!(
        SessionState::load_from(&store, &session_id())
            .await
            .expect_err("in-progress turn must reject resume"),
        crate::SessionStoreError::InvalidDocument { .. }
    ));

    let call = pending_tool_call("unresolved-completed-call");
    let unresolved = rewrite_journal_commit(&saved, |commit| {
        commit["items"] = serde_json::json!([{
            "type": "tool_call",
            "id": 0,
            "model_turn_id": 1,
            "call": call,
        }]);
        commit["state"]["next_item_id"] = serde_json::json!(1);
        commit["state"]["model_turns"] = serde_json::json!([{"id": 1, "status": "completed"}]);
        commit["state"]["next_model_turn_id"] = serde_json::json!(2);
    });
    store
        .write_state_bytes(&session_id(), &unresolved)
        .await
        .expect("unresolved state writes");
    assert!(matches!(
        SessionState::load_from(&store, &session_id())
            .await
            .expect_err("completed turn with unresolved call must reject resume"),
        crate::SessionStoreError::InvalidDocument { .. }
    ));
}

#[tokio::test]
async fn session_state_load_rejects_mismatched_tool_result_identity() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());
    let call = pending_tool_call("persisted-result-call");
    session
        .record_test_tool_call_pending(call.clone())
        .expect("call records");
    session
        .submit_tool_result(
            ToolCallResult::succeeded(call.id().clone()),
            ToolOutput::text("result"),
        )
        .expect("result records");
    session.save_to(&store).await.expect("session saves");

    let saved = store
        .read_state_bytes(&session_id())
        .await
        .expect("state reads");
    let mismatched = rewrite_journal_commit(&saved, |commit| {
        commit["items"][1]["result"]["call_id"] =
            serde_json::Value::String("different-result-call".to_owned());
    });
    store
        .write_state_bytes(&session_id(), &mismatched)
        .await
        .expect("mismatched result state writes");

    assert!(matches!(
        SessionState::load_from(&store, &session_id())
            .await
            .expect_err("mismatched result identity must reject resume"),
        crate::SessionStoreError::InvalidDocument { .. }
    ));

    let duplicate_result = rewrite_journal_commit(&saved, |commit| {
        let mut repeated = commit["items"][1].clone();
        repeated["id"] = serde_json::Value::from(2);
        commit["items"]
            .as_array_mut()
            .expect("journal items are an array")
            .push(repeated);
        commit["state"]["next_item_id"] = serde_json::Value::from(3);
    });
    store
        .write_state_bytes(&session_id(), &duplicate_result)
        .await
        .expect("duplicate result state writes");
    assert!(matches!(
        SessionState::load_from(&store, &session_id())
            .await
            .expect_err("duplicate tool results must reject resume"),
        crate::SessionStoreError::InvalidDocument { .. }
    ));
}

#[tokio::test]
async fn session_state_rejects_unsupported_format_before_body_decode() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let document = serde_json::json!({
        "type": "header",
        "format_version": 99,
        "session_id": session_id()
    });
    let mut bytes = serde_json::to_vec(&document).expect("header serializes");
    bytes.push(b'\n');
    store
        .write_state_bytes(&session_id(), &bytes)
        .await
        .expect("unsupported state writes");

    let error = SessionState::load_from(&store, &session_id())
        .await
        .expect_err("unsupported format must reject before body decode");
    assert!(matches!(
        error,
        crate::SessionStoreError::UnsupportedFormatVersion { actual: 99 }
    ));
}
