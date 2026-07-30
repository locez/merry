use super::*;
use crate::{
    ContextCompiler, ContextEntry, ContextEvidence, ContextSummary, FileSessionStore, PlanError,
    PlanPersistenceLocation, ProjectRules, SkillCatalog, TaskAnchor, UserImageInput,
    UserMessageInput,
    artifact::ArtifactContent,
    plan::{
        ControlPlanAttemptInput, PlanChangeInput, PlanExecutionIntent, PlanNodeInput, PlanState,
        ReportPlanProgressInput, UpdatePlanInput, execution::PlanAttemptActor,
    },
    session::PromptHistoryProjection,
};
use merry_core::{
    ArtifactKind, ArtifactRef, EvidenceLocator, EvidenceRef, PlanActivationSource,
    PlanAttemptOutcome, PlanDirectiveConstraints, PlanDirectiveKind, PlanExecutorPolicy,
    PlanHarnessSnapshot, PlanId, PlanNodeStatus, PlanRecoveryPolicySnapshot,
    PlanResourcePolicySnapshot, ToolCallResult, ToolCallResultStatus,
};
use std::sync::Arc;

fn persisted_image_message() -> UserMessageInput {
    UserMessageInput::new(
        "resume [Image #1] and [Image #2]",
        vec![
            UserImageInput::png(
                "[Image #1]",
                Arc::<[u8]>::from([137, 80, 78, 71, 13, 10, 26, 10, 11]),
                6,
                7,
            )
            .expect("valid first image"),
            UserImageInput::png(
                "[Image #2]",
                Arc::<[u8]>::from([137, 80, 78, 71, 13, 10, 26, 10, 22]),
                8,
                9,
            )
            .expect("valid second image"),
        ],
    )
    .expect("valid image message")
}

#[tokio::test]
async fn session_state_round_trip_preserves_user_images_for_provider_history() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());
    let turn_id = session.begin_model_turn().expect("turn should begin");
    let message = persisted_image_message();
    session
        .record_user_message(turn_id, &message)
        .expect("image message should record");
    session
        .close_model_response(turn_id, false)
        .expect("turn should close");
    session.save_to(&store).await.expect("session should save");

    let loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("image session should load");
    let snapshot = loaded
        .provider_transcript_snapshot()
        .expect("provider history should compile");
    let [crate::session::TranscriptItemSnapshot::UserMessage { text, images, .. }] =
        snapshot.as_slice()
    else {
        panic!("provider history should contain one image message");
    };
    assert_eq!(text, message.text());
    assert_eq!(images.len(), message.images().len());
    for (actual, expected) in images.iter().zip(message.images()) {
        assert_eq!(actual.input(), expected);
    }
}

#[tokio::test]
async fn session_state_v2_user_message_without_image_ids_defaults_to_text_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());
    let turn_id = session.begin_model_turn().expect("turn should begin");
    session
        .record_user_message_body(turn_id, "legacy v2 text")
        .expect("text message should record");
    session
        .close_model_response(turn_id, false)
        .expect("turn should close");
    session.save_to(&store).await.expect("session should save");

    let mut document: serde_json::Value = serde_json::from_slice(
        &store
            .read_state_bytes(&session_id())
            .await
            .expect("state should read"),
    )
    .expect("state should be JSON");
    document["transcript"]["items"][0]
        .as_object_mut()
        .expect("transcript item should be an object")
        .remove("image_artifact_ids");
    store
        .write_state_bytes(
            &session_id(),
            &serde_json::to_vec_pretty(&document).expect("state should serialize"),
        )
        .await
        .expect("compatibility fixture should write");

    let loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("old v2 user message should load");
    let snapshot = loaded
        .full_transcript_snapshot()
        .expect("transcript should compile");
    let [crate::session::TranscriptItemSnapshot::UserMessage { images, .. }] = snapshot.as_slice()
    else {
        panic!("transcript should contain one user message");
    };
    assert!(images.is_empty());
}

#[tokio::test]
async fn session_state_v2_round_trip_preserves_turns_user_artifact_and_projections() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());
    let text_turn = session.begin_model_turn().expect("text turn should begin");
    session
        .record_user_message_body(text_turn, "  exact persisted user text\n")
        .expect("user message should record");
    session
        .record_assistant_text_output(text_turn, "persisted assistant".to_owned())
        .expect("assistant output should record");
    session
        .close_model_response(text_turn, false)
        .expect("text response should close");

    let final_turn = session
        .begin_model_turn()
        .expect("final-output turn should begin");
    let final_call = pending_tool_call("persisted-final-output");
    session
        .record_tool_call_pending(final_turn, final_call.clone())
        .expect("final-output call should record");
    session
        .close_model_response(final_turn, true)
        .expect("final-output response should close");
    session
        .record_final_output(final_call.id().clone(), r#"{"ok":true}"#.to_owned())
        .expect("final output should record");

    let aborted_turn = session
        .begin_model_turn()
        .expect("aborted turn should begin");
    session
        .abort_model_turn(aborted_turn)
        .expect("aborted turn should close");
    let transcript_before = session.transcript.persisted();

    session.save_to(&store).await.expect("session should save");
    let stored: serde_json::Value = serde_json::from_slice(
        &store
            .read_state_bytes(&session_id())
            .await
            .expect("saved state should read"),
    )
    .expect("saved state should be JSON");
    assert_eq!(
        stored["prompt_history_projection"]["compacted_through"],
        serde_json::Value::Null
    );
    assert!(
        stored["transcript"]
            .get("prompt_history_projection")
            .is_none(),
        "the provider projection belongs to session state, not the transcript"
    );
    let loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("session should load");

    assert_eq!(loaded.transcript.persisted(), transcript_before);
    assert_eq!(
        loaded
            .full_transcript_snapshot()
            .expect("full transcript should remain readable")
            .len(),
        4,
        "hidden final-output exchange remains in the full transcript view"
    );
    assert_eq!(
        loaded
            .read_artifact_content(&artifact_id("user-message-0"))
            .expect("user artifact should load")
            .as_text(),
        Some("  exact persisted user text\n")
    );
    assert_eq!(
        loaded.model_turn_status(text_turn),
        Some(ModelTurnStatus::Completed)
    );
    assert_eq!(
        loaded.model_turn_status(final_turn),
        Some(ModelTurnStatus::Completed)
    );
    assert_eq!(
        loaded.model_turn_status(aborted_turn),
        Some(ModelTurnStatus::Aborted)
    );
    assert!(matches!(
        &transcript_before.items[2],
        crate::session::transcript::PersistedTranscriptItem::ToolCall {
            prompt_projection: crate::session::transcript::ToolCallPromptProjection::Hidden,
            ..
        }
    ));
    assert!(matches!(
        &transcript_before.items[3],
        crate::session::transcript::PersistedTranscriptItem::ToolResult {
            prompt_projection: crate::session::transcript::ToolResultPromptProjection::Hidden,
            ..
        }
    ));
}

#[tokio::test]
async fn session_state_v1_without_compaction_migrates_to_legacy_turn_and_user_artifact() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let document = legacy_v1_document(vec![serde_json::json!({
        "type": "user_message",
        "id": 0,
        "text": "  exact legacy user text\n",
        "origin": "external_user"
    })]);
    store
        .write_state_bytes(
            &session_id(),
            &serde_json::to_vec_pretty(&document).expect("legacy document should serialize"),
        )
        .await
        .expect("legacy state should write");

    let mut loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("uncompacted V1 state should migrate");

    assert_eq!(
        loaded.model_turn_status(crate::session::ModelTurnId::new(0)),
        Some(ModelTurnStatus::Completed)
    );
    assert_eq!(
        loaded
            .read_artifact_content(&artifact_id("user-message-0"))
            .expect("migrated user artifact should load")
            .as_text(),
        Some("  exact legacy user text\n")
    );
    let next_turn = loaded.transcript.persisted().next_model_turn_id;
    assert_eq!(next_turn, 1);
    assert!(matches!(
        &loaded.transcript.persisted().items[0],
        crate::session::transcript::PersistedTranscriptItem::UserMessage {
            model_turn_id,
            artifact_id,
            ..
        } if *model_turn_id == crate::session::ModelTurnId::new(0)
            && artifact_id.as_str() == "user-message-0"
    ));

    let post_migration_turn = loaded
        .begin_model_turn()
        .expect("post-migration turn should begin at one");
    loaded
        .record_user_message_body(post_migration_turn, "post-migration user text")
        .expect("post-migration user message should record");
    loaded
        .record_assistant_text_output(post_migration_turn, "post-migration response".to_owned())
        .expect("post-migration assistant output should record");
    loaded
        .close_model_response(post_migration_turn, false)
        .expect("post-migration turn should complete");
    loaded
        .save_to(&store)
        .await
        .expect("migrated session should save as V2");

    let reloaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("V2 session with legacy turn zero should reload");
    assert_eq!(
        reloaded.model_turn_status(crate::session::ModelTurnId::new(0)),
        Some(ModelTurnStatus::Completed)
    );
    assert_eq!(
        reloaded.model_turn_status(post_migration_turn),
        Some(ModelTurnStatus::Completed)
    );
    assert_eq!(reloaded.transcript.persisted().next_model_turn_id, 2);
}

#[tokio::test]
async fn session_state_v1_with_compacted_checkpoint_is_rejected_as_exact_history_unavailable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut document = legacy_v1_document(Vec::new());
    document["compacted_checkpoint"] = serde_json::to_value(
        citation_plain_runtime_checkpoint_for_tests("legacy-checkpoint", "deleted history")
            .persisted(),
    )
    .expect("checkpoint should serialize");
    store
        .write_state_bytes(
            &session_id(),
            &serde_json::to_vec_pretty(&document).expect("legacy document should serialize"),
        )
        .await
        .expect("legacy state should write");

    let error = SessionState::load_from(&store, &session_id())
        .await
        .expect_err("compacted V1 state should be rejected");

    assert!(matches!(
        error,
        crate::SessionStoreError::LegacyCompactedHistoryUnavailable { .. }
    ));
    assert!(error.to_string().contains("physically deleted"));
}

#[tokio::test]
async fn session_state_v1_user_artifact_collision_is_rejected_without_overwrite() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut document = legacy_v1_document(vec![serde_json::json!({
        "type": "user_message",
        "id": 0,
        "text": "legacy source",
        "origin": "external_user"
    })]);
    document["artifacts"] = serde_json::json!([{
        "artifact": ArtifactRef::new(artifact_id("user-message-0"), ArtifactKind::Text),
        "content": ArtifactContent::text("collision")
    }]);
    store
        .write_state_bytes(
            &session_id(),
            &serde_json::to_vec_pretty(&document).expect("legacy document should serialize"),
        )
        .await
        .expect("legacy state should write");

    let error = SessionState::load_from(&store, &session_id())
        .await
        .expect_err("colliding V1 user artifact should be rejected");

    assert!(matches!(
        error,
        crate::SessionStoreError::LegacyUserArtifactCollision { .. }
    ));
}

fn legacy_v1_document(items: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({
        "format_version": 1,
        "session_id": session_id(),
        "next_sequence": 0,
        "session_started": false,
        "ledger": [],
        "artifacts": [],
        "compacted_checkpoint": null,
        "context_entries": [],
        "transcript": {
            "items": items,
            "next_id": 1
        },
        "resolved_tool_calls": [],
        "usage": null,
        "task_anchor": null,
        "registries": {
            "judgments": { "records": [] },
            "summary_draft_promotions": { "records": [] },
            "action_audits": { "records": [] }
        }
    })
}

fn literal_v2_document() -> serde_json::Value {
    serde_json::json!({
        "format_version": 2,
        "session_id": session_id(),
        "next_sequence": 0,
        "session_started": false,
        "ledger": [],
        "artifacts": [],
        "compacted_checkpoint": null,
        "context_entries": [],
        "transcript": {
            "items": [],
            "next_id": 0,
            "model_turns": {},
            "next_model_turn_id": 1
        },
        "resolved_tool_calls": [],
        "usage": null,
        "task_anchor": null,
        "registries": {
            "judgments": { "records": [] },
            "summary_draft_promotions": { "records": [] },
            "action_audits": { "records": [] }
        }
    })
}

#[tokio::test]
async fn session_state_v2_without_projection_or_checkpoint_migrates_on_next_save() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let document = literal_v2_document();
    assert!(document.get("prompt_history_projection").is_none());
    store
        .write_state_bytes(
            &session_id(),
            &serde_json::to_vec_pretty(&document).expect("literal V2 document should serialize"),
        )
        .await
        .expect("legacy V2 state should write");

    let loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("uncompacted V2 state should migrate");
    assert_eq!(
        loaded.prompt_history_projection(),
        PromptHistoryProjection::default()
    );
    loaded
        .save_to(&store)
        .await
        .expect("migrated V2 state should save");

    let rewritten: serde_json::Value = serde_json::from_slice(
        &store
            .read_state_bytes(&session_id())
            .await
            .expect("rewritten state should read"),
    )
    .expect("rewritten state should be JSON");
    assert_eq!(
        rewritten["prompt_history_projection"]["compacted_through"],
        serde_json::Value::Null
    );
}

#[tokio::test]
async fn session_state_v2_with_checkpoint_but_without_projection_is_rejected() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut document = literal_v2_document();
    document["compacted_checkpoint"] = serde_json::to_value(
        citation_plain_runtime_checkpoint_for_tests(
            "projection-missing-checkpoint",
            "the old body may already have been physically deleted",
        )
        .persisted(),
    )
    .expect("checkpoint should serialize");
    assert!(document.get("prompt_history_projection").is_none());
    store
        .write_state_bytes(
            &session_id(),
            &serde_json::to_vec_pretty(&document).expect("literal V2 document should serialize"),
        )
        .await
        .expect("legacy compacted V2 state should write");

    let error = SessionState::load_from(&store, &session_id())
        .await
        .expect_err("compacted V2 state without a projection must reject resume");

    assert!(matches!(
        error,
        crate::SessionStoreError::InvalidDocument { .. }
    ));
}

#[tokio::test]
async fn session_state_save_load_round_trips_next_reasoning_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());

    session
        .record_test_user_message_body("remember this user fact")
        .expect("user message records");
    let artifact = ArtifactRef::new(artifact_id("resume-source"), ArtifactKind::Text);
    session
        .record_artifact_events(artifact.clone(), ArtifactContent::text("exact evidence"))
        .expect("artifact records");
    let evidence = EvidenceRef::new(artifact.id().clone(), EvidenceLocator::whole_artifact());
    session
        .record_context_entry(ContextEntry::summary(
            ContextSummary::new(
                "resume-summary",
                "A grounded summary for resume.",
                vec![ContextEvidence::new("source", evidence).expect("context evidence")],
            )
            .expect("summary"),
        ))
        .expect("context records");
    session
        .record_artifact_events(
            ArtifactRef::new(artifact_id("checkpoint-test-source"), ArtifactKind::Text),
            ArtifactContent::text("resume checkpoint exact source"),
        )
        .expect("checkpoint source records");
    session.set_compacted_checkpoint(citation_plain_runtime_checkpoint_for_tests(
        "resume-checkpoint",
        "resume checkpoint text",
    ));
    session
        .record_test_tool_call_pending(pending_tool_call("call-resume"))
        .expect("pending call records");
    session
        .submit_tool_result(
            ToolCallResult::new(
                tool_call_id("call-resume"),
                ToolCallResultStatus::Succeeded,
                ArtifactRef::new(artifact_id("manual-tool-result"), ArtifactKind::Text),
                None,
            )
            .expect("tool result"),
            ArtifactContent::text("manual result"),
        )
        .expect("tool result records");

    session.save_to(&store).await.expect("session saves");
    let loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("session loads");

    assert_eq!(loaded.session_id(), &session_id());
    assert_eq!(loaded.next_sequence(), session.next_sequence());
    assert!(!loaded.has_pending_tool_calls());
    assert_eq!(
        loaded.transcript_items_for_tests(),
        session.transcript_items_for_tests()
    );

    let compiled = ContextCompiler::new()
        .compile(&loaded.context_snapshot())
        .expect("loaded context compiles");
    let snapshot = compiled.to_snapshot();
    assert!(snapshot.contains("resume-summary"));
    assert!(snapshot.contains("resume checkpoint text"));
    assert!(loaded.compacted_checkpoint_summary().is_some());
}

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
async fn session_state_v2_load_rejects_missing_user_source_artifact() {
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
async fn session_state_v2_load_rejects_user_message_cross_linked_to_readable_text_artifact() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut document = literal_v2_document();
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
            "origin": "external_user"
        }],
        "next_id": 1,
        "model_turns": { "1": "completed" },
        "next_model_turn_id": 2
    });
    store
        .write_state_bytes(
            &session_id(),
            &serde_json::to_vec_pretty(&document).expect("literal V2 document serializes"),
        )
        .await
        .expect("corrupted V2 state writes");

    let error = SessionState::load_from(&store, &session_id())
        .await
        .expect_err("cross-linked user source artifact must reject resume");

    assert!(matches!(
        error,
        crate::SessionStoreError::InvalidDocument { .. }
    ));
}

#[tokio::test]
async fn session_state_v2_load_rejects_unreachable_model_turn_sequences() {
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
        let mut document = literal_v2_document();
        document["transcript"]["model_turns"] = model_turns;
        document["transcript"]["next_model_turn_id"] = serde_json::Value::from(next_model_turn_id);
        store
            .write_state_bytes(
                &session_id(),
                &serde_json::to_vec_pretty(&document).expect("literal V2 document serializes"),
            )
            .await
            .expect("corrupted V2 state writes");

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
async fn session_state_v2_load_rejects_nonterminal_or_unresolved_turns() {
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
async fn session_state_v2_load_rejects_mismatched_tool_result_identity() {
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
async fn session_state_save_loads_inline_artifacts_without_payload_files() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());
    let long_id = "a".repeat(128);
    let artifact = ArtifactRef::new(
        ArtifactId::new(&long_id).expect("max length artifact id is valid"),
        ArtifactKind::Text,
    );
    session
        .record_artifact_events(artifact.clone(), ArtifactContent::text("inline payload"))
        .expect("artifact records");

    session.save_to(&store).await.expect("session saves");
    assert!(
        !store.artifacts_dir(&session_id()).exists(),
        "single-file resume state should not create artifact payload files"
    );
    let json = String::from_utf8(
        store
            .read_state_bytes(&session_id())
            .await
            .expect("state reads"),
    )
    .expect("state is utf8 json");
    assert!(json.contains("inline payload"));

    let loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("session loads from single state file");
    assert_eq!(
        loaded
            .read_artifact_content(artifact.id())
            .expect("inline artifact resumes")
            .as_text(),
        Some("inline payload")
    );
}

#[tokio::test]
async fn session_state_save_load_round_trips_recoverable_registries() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());
    session
        .record_artifact_state(
            ArtifactRef::new(artifact_id("registry-source"), ArtifactKind::Text),
            ArtifactContent::text("registry source text\n"),
        )
        .expect("artifact records");

    let evidence = judgment_evidence(
        "registry source",
        "registry-source",
        EvidenceLocator::whole_artifact(),
    );
    let request = summary_draft_request(vec![evidence.clone()]);
    let outcome = summary_draft_outcome_with_draft(vec![evidence.clone()], "Registry draft.");
    let record = session
        .record_summary_draft_judgment(request.clone(), outcome.clone())
        .expect("judgment records");
    session
        .promote_summary_draft_to_context(
            &request,
            &outcome,
            promotion_input_with_source_record_id(
                "registry-summary",
                "Registry draft.",
                vec![evidence.clone()],
                Some(record.id().clone()),
            ),
        )
        .expect("promotion records");

    let executed_call = pending_tool_call("registry-executed-call");
    session
        .record_test_tool_call_pending(executed_call.clone())
        .expect("pending executed call records");
    let proposal_evidence = ActionProposalEvidence::WorkspacePatch(
        WorkspacePatchProposal::new(
            "note.txt",
            3,
            5,
            16,
            18,
            "fnv1a64:0000000000000100",
            "fnv1a64:0000000000000101",
        )
        .expect("valid workspace patch proposal"),
    );
    let proposal = ActionProposal::new(
        &executed_call,
        crate::ToolActionKind::WorkspaceWrite,
        "workspace patch",
        "note.txt",
        "Replace one preimage in note.txt.",
        proposal_evidence,
    )
    .expect("valid action proposal");
    let execution_evidence = ActionExecutionEvidence::WorkspacePatch(
        WorkspacePatchExecutionEvidence::new(
            "note.txt",
            3,
            5,
            16,
            18,
            "fnv1a64:0000000000000100",
            "fnv1a64:0000000000000101",
        )
        .expect("valid execution evidence"),
    );
    let allow_policy = ActionAuditPolicy::new(
        ActionRiskTier::EditLow,
        ActionPolicyDisposition::Allow,
        "test low-risk workspace patch allow",
    );
    session
        .submit_proposed_tool_execution_outcome(
            proposal,
            merry_core::ToolCallResultStatus::Succeeded,
            ArtifactContent::json(r#"{"ok":true}"#),
            None,
            Some(execution_evidence.clone()),
            allow_policy,
        )
        .expect("proposed execution records");

    let call = pending_tool_call("registry-denied-call");
    let decision = DefaultActionPolicy.decide(crate::ToolActionKind::WorkspaceWrite);
    let diagnostic = ErrorInfo::new("action_policy_denied", "blocked by persistence test")
        .expect("valid diagnostic");
    session
        .record_test_tool_call_pending(call.clone())
        .expect("pending call records");
    session
        .submit_denied_tool_action(
            &call,
            &decision,
            None,
            ArtifactContent::json(r#"{"ok":false}"#),
            diagnostic,
        )
        .expect("denial records");

    session.save_to(&store).await.expect("session saves");
    let mut loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("session loads");

    assert_eq!(loaded.judgment_records().len(), 1);
    assert_eq!(
        loaded.judgment_records()[0].id().as_str(),
        "judgment-record-00000000000000000000"
    );
    assert_single_promotion_record(
        &loaded,
        "registry-summary",
        SummaryDraftPromotionState::Promoted,
        Some("judgment-record-00000000000000000000"),
    );
    let audit_snapshot = loaded.action_audit_snapshot();
    assert_eq!(audit_snapshot.records().len(), 3);
    assert_eq!(
        audit_snapshot.records()[0].status(),
        ActionAuditStatus::Proposed
    );
    assert!(audit_snapshot.records()[0].proposal().is_some());
    assert!(audit_snapshot.records()[0].execution_evidence().is_none());
    assert_eq!(
        audit_snapshot.records()[1].status(),
        ActionAuditStatus::Executed
    );
    assert!(audit_snapshot.records()[1].proposal().is_none());
    assert_eq!(
        audit_snapshot.records()[1]
            .execution_evidence()
            .expect("executed audit should include evidence"),
        &execution_evidence
    );
    assert_eq!(
        audit_snapshot.records()[2].status(),
        ActionAuditStatus::Denied
    );

    loaded
        .promote_summary_draft_to_context(
            &request,
            &outcome,
            promotion_input_with_source_record_id(
                "registry-summary",
                "Registry draft.",
                vec![evidence],
                Some(record.id().clone()),
            ),
        )
        .expect("restored promotion record keeps replay idempotent");
    assert_single_promotion_record(
        &loaded,
        "registry-summary",
        SummaryDraftPromotionState::Promoted,
        Some("judgment-record-00000000000000000000"),
    );
}

#[tokio::test]
async fn session_state_saved_document_omits_construction_context_and_memory_store() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());
    session.set_project_rules(ProjectRules::new("AGENTS.md", "private rules").expect("rules"));
    session.set_skill_catalog(SkillCatalog::from_metadata(Vec::new()).expect("empty catalog"));
    session.set_task_anchor(TaskAnchor::new("resume task").expect("task anchor"));

    session.save_to(&store).await.expect("session saves");
    let bytes = store
        .read_state_bytes(&session_id())
        .await
        .expect("state reads");
    let json = String::from_utf8(bytes).expect("state is utf8 json");

    assert!(!json.contains("project_rules"));
    assert!(!json.contains("skill_catalog"));
    assert!(!json.contains("memory_store"));
    assert!(!json.contains("activated_memories"));
    assert!(json.contains("resume task"));
}

fn persisted_test_plan() -> PlanState {
    let mut plan = PlanState::empty(
        PlanId::new("persisted-plan").expect("valid plan id"),
        PlanActivationSource::Coordinator {
            reason: "persist the plan".to_owned(),
            governing_skill_id: None,
        },
        PlanResourcePolicySnapshot::default(),
    );
    plan.update(UpdatePlanInput {
        reason: "define persisted root".to_owned(),
        execution_intent: PlanExecutionIntent::ContinuePlanning,
        coordinator_node_id: None,
        max_concurrency_hint: Some(2),
        change: PlanChangeInput::DefinePlan {
            expected_plan_revision: 0,
            root: PlanNodeInput {
                id: None,
                client_key: Some("root".to_owned()),
                objective: "Persist and resume the plan".to_owned(),
                acceptance: vec!["same ids and revisions after load".to_owned()],
                status: None,
                executor_policy: PlanExecutorPolicy::Local,
                harness: PlanHarnessSnapshot::default(),
                recovery_policy: PlanRecoveryPolicySnapshot::default(),
                depends_on: Vec::new(),
                children: Vec::new(),
            },
        },
    })
    .expect("valid persisted plan");
    plan
}

#[tokio::test]
async fn session_state_current_format_without_plan_loads_with_none_and_rewrites_current_format() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let document = literal_v2_document();
    store
        .write_state_bytes(
            &session_id(),
            &serde_json::to_vec_pretty(&document).expect("literal V2 document serializes"),
        )
        .await
        .expect("V2 state writes");

    let loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("V2 state loads without a plan");
    assert!(loaded.active_plan().is_none());
    loaded.save_to(&store).await.expect("migrated state saves");

    let rewritten: serde_json::Value = serde_json::from_slice(
        &store
            .read_state_bytes(&session_id())
            .await
            .expect("rewritten state reads"),
    )
    .expect("rewritten state is JSON");
    assert_eq!(rewritten["format_version"], 3);
    assert_eq!(rewritten["active_plan"], serde_json::Value::Null);
    assert_eq!(rewritten["terminal_plans"], serde_json::json!([]));
}

#[tokio::test]
async fn session_state_round_trip_preserves_active_plan_snapshot() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());
    let plan = persisted_test_plan();
    let expected = plan.snapshot().clone();
    session.set_active_plan(plan);

    session.save_to(&store).await.expect("plan session saves");
    let loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("plan session loads");

    assert_eq!(
        loaded.active_plan().expect("active plan").snapshot(),
        &expected
    );
    assert!(loaded.terminal_plans().is_empty());
}

#[tokio::test]
async fn session_load_preserves_typed_plan_validation_error_context() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());
    session.set_active_plan(persisted_test_plan());
    session.save_to(&store).await.expect("plan session saves");

    let mut document: serde_json::Value = serde_json::from_slice(
        &store
            .read_state_bytes(&session_id())
            .await
            .expect("state reads"),
    )
    .expect("state is JSON");
    document["active_plan"]["snapshot"]["nodes"][0]["recovery_policy"]["max_transient_attempts"] =
        serde_json::json!(9);
    store
        .write_state_bytes(
            &session_id(),
            &serde_json::to_vec_pretty(&document).expect("corrupt state serializes"),
        )
        .await
        .expect("corrupt state writes");

    let error = SessionState::load_from(&store, &session_id())
        .await
        .expect_err("invalid persisted retry policy must reject load");
    let message = error.to_string();
    assert!(matches!(
        error,
        crate::SessionStoreError::InvalidPlan {
            location: PlanPersistenceLocation::Active,
            source: PlanError::TooManyTransientAttempts {
                actual: 9,
                maximum: 8
            }
        }
    ));
    assert!(message.contains("active plan"));
    assert!(message.contains("9"));
}

#[tokio::test]
async fn session_load_preserves_oversized_plan_snapshot_error_context() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());
    session.set_active_plan(persisted_test_plan());
    session.save_to(&store).await.expect("plan session saves");

    let mut document: serde_json::Value = serde_json::from_slice(
        &store
            .read_state_bytes(&session_id())
            .await
            .expect("state reads"),
    )
    .expect("state is JSON");
    document["active_plan"]["snapshot"]["execution_authorization_refs"] =
        serde_json::to_value(vec!["x".repeat(1024); 300]).expect("refs serialize");
    store
        .write_state_bytes(
            &session_id(),
            &serde_json::to_vec_pretty(&document).expect("corrupt state serializes"),
        )
        .await
        .expect("corrupt state writes");

    let error = SessionState::load_from(&store, &session_id())
        .await
        .expect_err("oversized persisted snapshot must reject load");
    match error {
        crate::SessionStoreError::InvalidPlan {
            location: PlanPersistenceLocation::Active,
            source: PlanError::SnapshotTooLarge { actual, maximum },
        } => {
            assert!(actual > maximum);
            assert_eq!(maximum, 256 * 1024);
        }
        other => panic!("unexpected persisted plan error: {other:?}"),
    }
}

#[tokio::test]
async fn session_state_round_trip_preserves_plan_attempt_lease_directive_recovery_and_history() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut plan = PlanState::empty(
        PlanId::new("complex-persisted-plan").expect("valid plan id"),
        PlanActivationSource::Coordinator {
            reason: "persist execution state".to_owned(),
            governing_skill_id: None,
        },
        PlanResourcePolicySnapshot::default(),
    );
    let update = plan
        .update(UpdatePlanInput {
            reason: "define a plan with live and recoverable work".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: Some(2),
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: PlanNodeInput {
                    id: None,
                    client_key: Some("root".to_owned()),
                    objective: "Persist all execution records".to_owned(),
                    acceptance: vec!["records survive reload".to_owned()],
                    status: None,
                    executor_policy: PlanExecutorPolicy::Local,
                    harness: PlanHarnessSnapshot::default(),
                    recovery_policy: PlanRecoveryPolicySnapshot::default(),
                    depends_on: Vec::new(),
                    children: vec![
                        persisted_plan_leaf("live-initial", "Keep live work"),
                        persisted_plan_leaf("expired-initial", "Recover expired work"),
                        persisted_plan_leaf("obsolete", "Obsolete work"),
                    ],
                },
            },
        })
        .expect("complex plan definition succeeds");
    let _initial_update = update;
    plan.update(UpdatePlanInput {
        reason: "supersede the obsolete authored history".to_owned(),
        execution_intent: PlanExecutionIntent::ContinuePlanning,
        coordinator_node_id: None,
        max_concurrency_hint: Some(2),
        change: PlanChangeInput::DefinePlan {
            expected_plan_revision: plan.snapshot().revision,
            root: PlanNodeInput {
                id: None,
                client_key: Some("root-current".to_owned()),
                objective: "Persist all current execution records".to_owned(),
                acceptance: vec!["current records survive reload".to_owned()],
                status: None,
                executor_policy: PlanExecutorPolicy::Local,
                harness: PlanHarnessSnapshot::default(),
                recovery_policy: PlanRecoveryPolicySnapshot::default(),
                depends_on: Vec::new(),
                children: vec![
                    persisted_plan_leaf("live-current", "Keep live work"),
                    persisted_plan_leaf("expired-current", "Recover expired work"),
                ],
            },
        },
    })
    .expect("subtree replacement succeeds");
    plan.enter_execution(
        Default::default(),
        vec!["persist test authorization".to_owned()],
    )
    .expect("execution starts");

    let live_node_id = persisted_plan_node_id(&plan, "live-current");
    let expired_node_id = persisted_plan_node_id(&plan, "expired-current");
    let live_actor = PlanAttemptActor {
        executor_session_id: session_id_with_suffix("persist-live-executor"),
    };
    let live = plan
        .start_attempt(&live_node_id, live_actor.clone(), 10_000)
        .expect("live attempt starts");
    let live_directive = plan
        .issue_directive(
            ControlPlanAttemptInput {
                attempt_id: live.attempt.attempt_id.clone(),
                kind: PlanDirectiveKind::Steer,
                reason: "persist this live directive".to_owned(),
                instruction: Some("keep the current verification path".to_owned()),
                constraints: Some(PlanDirectiveConstraints::default()),
                requested_output: vec!["current checkpoint".to_owned()],
            },
            10_100,
        )
        .expect("live directive queues");
    plan.report_progress(
        &live_actor,
        ReportPlanProgressInput {
            summary: "live progress is durable".to_owned(),
            evidence_refs: Vec::new(),
            artifact_refs: Vec::new(),
            next_action: Some("continue verification".to_owned()),
            checkpoint_ref: Some("persisted-checkpoint".to_owned()),
            acknowledged_directive_ids: vec![live_directive.directive.directive_id],
            applied_directive_ids: Vec::new(),
            request_coordinator_review: Some(true),
        },
        10_200,
    )
    .expect("live progress records");

    let expired_actor = PlanAttemptActor {
        executor_session_id: session_id_with_suffix("persist-expired-executor"),
    };
    let expired = plan
        .start_attempt(&expired_node_id, expired_actor, 2_000)
        .expect("recoverable attempt starts");
    plan.issue_directive(
        ControlPlanAttemptInput {
            attempt_id: expired.attempt.attempt_id,
            kind: PlanDirectiveKind::RequestStatus,
            reason: "persist this expiring directive".to_owned(),
            instruction: None,
            constraints: None,
            requested_output: Vec::new(),
        },
        2_100,
    )
    .expect("expiring directive queues");
    plan.interrupt_expired_leases(expired.lease.lease_expires_at_ms)
        .expect("expired lease is interrupted");

    let expected = plan.snapshot().clone();
    assert!(
        expected
            .attempts
            .iter()
            .any(|attempt| attempt.outcome.is_none())
    );
    assert!(
        expected
            .attempts
            .iter()
            .any(|attempt| attempt.outcome == Some(PlanAttemptOutcome::Interrupted))
    );
    assert!(
        expected
            .nodes
            .iter()
            .any(|node| node.status == PlanNodeStatus::Superseded)
    );
    assert!(
        expected
            .directives
            .iter()
            .any(|directive| directive.status == merry_core::PlanDirectiveStatus::Acknowledged)
    );
    assert!(
        expected
            .directives
            .iter()
            .any(|directive| directive.status == merry_core::PlanDirectiveStatus::Expired)
    );

    let mut session = SessionState::new(session_id());
    session.set_active_plan(plan);
    session.save_to(&store).await.expect("complex plan saves");
    let loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("complex plan loads");

    assert_eq!(
        loaded.active_plan().expect("active plan").snapshot(),
        &expected
    );
}

fn persisted_plan_leaf(client_key: &str, objective: &str) -> PlanNodeInput {
    PlanNodeInput {
        id: None,
        client_key: Some(client_key.to_owned()),
        objective: objective.to_owned(),
        acceptance: vec![format!("{objective} is verified")],
        status: None,
        executor_policy: PlanExecutorPolicy::Delegate,
        harness: PlanHarnessSnapshot::default(),
        recovery_policy: PlanRecoveryPolicySnapshot::default(),
        depends_on: Vec::new(),
        children: Vec::new(),
    }
}

fn session_id_with_suffix(value: &str) -> merry_core::SessionId {
    merry_core::SessionId::new(value).expect("valid executor session id")
}

fn persisted_plan_node_id(plan: &PlanState, client_key: &str) -> merry_core::PlanNodeId {
    plan.snapshot()
        .nodes
        .iter()
        .find(|node| node.client_key.as_deref() == Some(client_key))
        .map(|node| node.id.clone())
        .expect("client key remains in snapshot")
}
