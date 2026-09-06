use crate::{
    ContextEntry, ContextEvidence, ContextSummary, FileSessionStore,
    artifact::ArtifactContent,
    session::tests::{
        SessionId, SessionState, SessionStateTestExt, artifact_id, pending_tool_call,
        persistence::current_document, session_id,
    },
};
use merry_core::{ArtifactKind, ArtifactRef, EvidenceLocator, EvidenceRef, ToolCallResult};

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
    let session = SessionState::new(session_id());
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

#[tokio::test]
async fn session_state_load_rejects_context_evidence_without_artifact() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());
    let artifact = ArtifactRef::new(artifact_id("missing-after-corruption"), ArtifactKind::Text);
    session
        .record_artifact_events(artifact.clone(), ArtifactContent::text("exact evidence"))
        .expect("artifact records");
    let evidence = EvidenceRef::new(artifact.id().clone(), EvidenceLocator::whole_artifact());
    session
        .record_context_entry(ContextEntry::summary(
            ContextSummary::new(
                "corrupted-summary",
                "A summary with corrupted persisted evidence.",
                vec![ContextEvidence::new("source", evidence).expect("context evidence")],
            )
            .expect("summary"),
        ))
        .expect("context records");
    session.save_to(&store).await.expect("session saves");

    let mut document: serde_json::Value = serde_json::from_slice(
        &store
            .read_state_bytes(&session_id())
            .await
            .expect("state reads"),
    )
    .expect("state is json");
    document["artifacts"] = serde_json::Value::Array(Vec::new());
    let bytes = serde_json::to_vec_pretty(&document).expect("state serializes");
    store
        .write_state_bytes(&session_id(), &bytes)
        .await
        .expect("corrupted state writes");

    let error = SessionState::load_from(&store, &session_id())
        .await
        .expect_err("corrupted context evidence is rejected");
    assert!(error.to_string().contains("session document is invalid"));
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
async fn session_state_load_rejects_missing_user_source_artifact() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());
    session
        .record_test_user_message_body("exact source must survive resume")
        .expect("user source records");
    session.save_to(&store).await.expect("session saves");

    let mut document: serde_json::Value = serde_json::from_slice(
        &store
            .read_state_bytes(&session_id())
            .await
            .expect("state reads"),
    )
    .expect("state is json");
    document["artifacts"] = serde_json::Value::Array(Vec::new());
    store
        .write_state_bytes(
            &session_id(),
            &serde_json::to_vec_pretty(&document).expect("corrupt document serializes"),
        )
        .await
        .expect("corrupt state writes");

    let error = SessionState::load_from(&store, &session_id())
        .await
        .expect_err("missing transcript source artifact must reject resume");
    assert!(matches!(
        error,
        crate::SessionStoreError::InvalidDocument { .. }
    ));
}

#[tokio::test]
async fn session_state_load_rejects_user_message_cross_linked_to_readable_text_artifact() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut document = current_document();
    document["artifacts"] = serde_json::json!([{
        "artifact": ArtifactRef::new(artifact_id("unrelated-user-source"), ArtifactKind::Text),
        "content": ArtifactContent::text("readable but not the stable user source")
    }]);
    document["transcript"] = serde_json::json!({
        "items": [{
            "type": "user_message",
            "id": 0,
            "model_turn_id": 1,
            "artifact_id": "unrelated-user-source",
            "image_artifact_ids": [],
            "origin": "external_user"
        }],
        "next_id": 1,
        "model_turns": { "1": "completed" },
        "next_model_turn_id": 2
    });
    store
        .write_state_bytes(
            &session_id(),
            &serde_json::to_vec_pretty(&document).expect("current document serializes"),
        )
        .await
        .expect("corrupted current state writes");

    let error = SessionState::load_from(&store, &session_id())
        .await
        .expect_err("cross-linked user source artifact must reject resume");

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
        ("empty map with advanced counter", serde_json::json!({}), 2),
        (
            "gap in positive turn ids",
            serde_json::json!({ "1": "completed", "3": "completed" }),
            4,
        ),
        (
            "counter jump after contiguous turns",
            serde_json::json!({ "1": "completed" }),
            3,
        ),
    ];

    for (case, model_turns, next_model_turn_id) in cases {
        let mut document = current_document();
        document["transcript"]["model_turns"] = model_turns;
        document["transcript"]["next_model_turn_id"] = serde_json::Value::from(next_model_turn_id);
        store
            .write_state_bytes(
                &session_id(),
                &serde_json::to_vec_pretty(&document).expect("current document serializes"),
            )
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
    let base: serde_json::Value = serde_json::from_slice(
        &store
            .read_state_bytes(&session_id())
            .await
            .expect("state reads"),
    )
    .expect("state is json");

    let mut in_progress = base.clone();
    in_progress["transcript"] = serde_json::json!({
        "items": [],
        "next_id": 0,
        "model_turns": { "1": "in_progress" },
        "next_model_turn_id": 2
    });
    store
        .write_state_bytes(
            &session_id(),
            &serde_json::to_vec_pretty(&in_progress).expect("document serializes"),
        )
        .await
        .expect("in-progress state writes");
    assert!(matches!(
        SessionState::load_from(&store, &session_id())
            .await
            .expect_err("in-progress turn must reject resume"),
        crate::SessionStoreError::InvalidDocument { .. }
    ));

    let call = pending_tool_call("unresolved-completed-call");
    let mut unresolved = base;
    unresolved["transcript"] = serde_json::json!({
        "items": [{
            "type": "tool_call",
            "id": 0,
            "model_turn_id": 1,
            "call": call,
            "prompt_projection": "full"
        }],
        "next_id": 1,
        "model_turns": { "1": "completed" },
        "next_model_turn_id": 2
    });
    store
        .write_state_bytes(
            &session_id(),
            &serde_json::to_vec_pretty(&unresolved).expect("document serializes"),
        )
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
            ToolCallResult::succeeded(
                call.id().clone(),
                ArtifactRef::new(artifact_id("persisted-result-artifact"), ArtifactKind::Text),
            ),
            ArtifactContent::text("result"),
        )
        .expect("result records");
    session.save_to(&store).await.expect("session saves");

    let mut document: serde_json::Value = serde_json::from_slice(
        &store
            .read_state_bytes(&session_id())
            .await
            .expect("state reads"),
    )
    .expect("state is json");
    let valid_document = document.clone();
    document["transcript"]["items"][1]["result"]["call_id"] =
        serde_json::Value::String("different-result-call".to_owned());
    store
        .write_state_bytes(
            &session_id(),
            &serde_json::to_vec_pretty(&document).expect("document serializes"),
        )
        .await
        .expect("mismatched result state writes");

    assert!(matches!(
        SessionState::load_from(&store, &session_id())
            .await
            .expect_err("mismatched result identity must reject resume"),
        crate::SessionStoreError::InvalidDocument { .. }
    ));

    let mut duplicate_result = valid_document;
    let mut repeated = duplicate_result["transcript"]["items"][1].clone();
    repeated["id"] = serde_json::Value::from(2);
    duplicate_result["transcript"]["items"]
        .as_array_mut()
        .expect("transcript items are an array")
        .push(repeated);
    duplicate_result["transcript"]["next_id"] = serde_json::Value::from(3);
    store
        .write_state_bytes(
            &session_id(),
            &serde_json::to_vec_pretty(&duplicate_result).expect("document serializes"),
        )
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
        "format_version": 99,
        "session_id": session_id()
    });
    store
        .write_state_bytes(
            &session_id(),
            &serde_json::to_vec(&document).expect("header serializes"),
        )
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
