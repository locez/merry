use super::*;
use crate::session::transcript::{PersistedTranscript, ToolResultPromptProjection};
use crate::session::{ModelTurnId, ModelTurnStatus, Transcript, TranscriptItem, UserInputOrigin};
use std::collections::BTreeMap;

#[test]
fn transcript_assigns_monotonic_model_turn_ids_starting_at_one() {
    let mut transcript = Transcript::new();

    let first = transcript
        .begin_model_turn()
        .expect("first model turn should allocate");
    transcript
        .abort_model_turn(first)
        .expect("first model turn should abort");
    let second = transcript
        .begin_model_turn()
        .expect("second model turn should allocate");

    assert_eq!(first, ModelTurnId::new(1));
    assert_eq!(second, ModelTurnId::new(2));
    assert_eq!(transcript.status(first), Some(ModelTurnStatus::Aborted));
    assert_eq!(transcript.status(second), Some(ModelTurnStatus::InProgress));
}

#[test]
fn model_turn_id_exhaustion_does_not_mutate_transcript() {
    let mut transcript = Transcript::from_persisted(PersistedTranscript {
        items: Vec::new(),
        next_id: 0,
        model_turns: BTreeMap::new(),
        next_model_turn_id: u64::MAX,
    })
    .expect("exhausted turn fixture should restore");
    let before = transcript.persisted();

    let error = transcript
        .begin_model_turn()
        .expect_err("exhausted model turn id should reject allocation");

    assert!(matches!(error, RuntimeError::ModelTurnIdExhausted));
    assert_eq!(transcript.persisted(), before);
}

#[test]
fn unknown_model_turn_mutations_are_rejected() {
    let mut transcript = Transcript::new();
    let unknown = ModelTurnId::new(42);

    for error in [
        transcript
            .close_model_response(unknown, false)
            .expect_err("unknown close should fail"),
        transcript
            .abort_model_turn(unknown)
            .expect_err("unknown abort should fail"),
        transcript
            .push_user_message(
                unknown,
                artifact_id("unknown-turn-user"),
                UserInputOrigin::ExternalUser,
            )
            .expect_err("unknown output should fail"),
    ] {
        assert!(matches!(
            error,
            RuntimeError::UnknownModelTurn { model_turn_id: 42 }
        ));
    }
}

#[test]
fn terminal_model_turn_rejects_reclose_reabort_and_new_output() {
    let mut transcript = Transcript::new();
    let completed = transcript
        .begin_model_turn()
        .expect("completed turn should begin");
    transcript
        .close_model_response(completed, false)
        .expect("turn should complete");

    assert!(matches!(
        transcript.close_model_response(completed, false),
        Err(RuntimeError::InvalidModelTurnTransition { .. })
    ));
    assert!(matches!(
        transcript.abort_model_turn(completed),
        Err(RuntimeError::InvalidModelTurnTransition { .. })
    ));
    assert!(matches!(
        transcript.push_assistant_text(completed, artifact_id("late-assistant")),
        Err(RuntimeError::InvalidModelTurnTransition { .. })
    ));

    let aborted = transcript
        .begin_model_turn()
        .expect("aborted turn should begin");
    transcript
        .abort_model_turn(aborted)
        .expect("turn should abort");
    assert!(matches!(
        transcript.close_model_response(aborted, false),
        Err(RuntimeError::InvalidModelTurnTransition { .. })
    ));
    assert!(matches!(
        transcript.abort_model_turn(aborted),
        Err(RuntimeError::InvalidModelTurnTransition { .. })
    ));
    assert!(matches!(
        transcript.push_user_message(
            aborted,
            artifact_id("late-user"),
            UserInputOrigin::ExternalUser,
        ),
        Err(RuntimeError::InvalidModelTurnTransition { .. })
    ));
}

#[test]
fn session_user_message_keeps_exact_text_only_in_stable_artifact() {
    let mut session = SessionState::new(session_id());
    let turn_id = session.begin_model_turn().expect("model turn should begin");
    let exact_text = "  exact user text\nwith trailing space \n";

    session
        .record_user_message_body(turn_id, exact_text)
        .expect("user message should record");

    let [
        TranscriptItem::UserMessage {
            artifact_id,
            model_turn_id,
            ..
        },
    ] = session.transcript.items()
    else {
        panic!("transcript should contain one artifact-backed user message");
    };
    assert_eq!(*model_turn_id, turn_id);
    assert_eq!(artifact_id.as_str(), "user-message-0");
    assert_eq!(
        session
            .read_artifact_content(artifact_id)
            .expect("user source artifact should be readable")
            .as_text(),
        Some(exact_text)
    );
}

#[test]
fn user_message_artifact_collision_leaves_session_state_unchanged() {
    let mut session = SessionState::new(session_id());
    let turn_id = session.begin_model_turn().expect("model turn should begin");
    let artifact = ArtifactRef::new(artifact_id("user-message-0"), ArtifactKind::Text);
    session
        .record_artifact_state(artifact, ArtifactContent::text("existing"))
        .expect("collision fixture artifact should record");
    let transcript_before = session.transcript.persisted();
    let artifacts_before = session.artifacts.persisted_records();

    let error = session
        .record_user_message_body(turn_id, "must not overwrite")
        .expect_err("reserved user artifact collision should fail");

    assert!(matches!(
        error,
        RuntimeError::Artifact {
            source: ArtifactError::DuplicateId { .. }
        }
    ));
    assert_eq!(session.transcript.persisted(), transcript_before);
    assert_eq!(session.artifacts.persisted_records(), artifacts_before);
}

#[test]
fn user_message_transcript_id_exhaustion_leaves_artifacts_and_ids_unchanged() {
    let mut session = SessionState::new(session_id());
    let turn_id = ModelTurnId::new(1);
    session.transcript = Transcript::from_persisted(PersistedTranscript {
        items: Vec::new(),
        next_id: u64::MAX,
        model_turns: [(turn_id, ModelTurnStatus::InProgress)]
            .into_iter()
            .collect(),
        next_model_turn_id: 2,
    })
    .expect("exhausted transcript fixture should restore");
    let transcript_before = session.transcript.persisted();
    let artifacts_before = session.artifacts.persisted_records();

    let error = session
        .record_user_message_body(turn_id, "cannot allocate")
        .expect_err("transcript item id exhaustion should fail");

    assert!(matches!(error, RuntimeError::TranscriptItemIdExhausted));
    assert_eq!(session.transcript.persisted(), transcript_before);
    assert_eq!(session.artifacts.persisted_records(), artifacts_before);
}

#[test]
fn transcript_assigns_monotonic_ids_and_never_reuses_after_retain() {
    let mut transcript = Transcript::new();
    let turn_id = transcript
        .begin_model_turn()
        .expect("model turn should allocate");

    let first = transcript
        .push_user_message(
            turn_id,
            artifact_id("first-user"),
            UserInputOrigin::ExternalUser,
        )
        .expect("first id should allocate");
    let second = transcript
        .push_user_message(
            turn_id,
            artifact_id("second-user"),
            UserInputOrigin::ExternalUser,
        )
        .expect("second id should allocate");

    assert_eq!(first.as_u64(), 0);
    assert_eq!(second.as_u64(), 1);

    transcript.retain_ids([second].into_iter().collect());

    let third = transcript
        .push_user_message(
            turn_id,
            artifact_id("third-user"),
            UserInputOrigin::ExternalUser,
        )
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
fn compacted_history_removal_keeps_uncovered_batch_pairs_intact() {
    let mut transcript = Transcript::new();
    let turn_id = transcript
        .begin_model_turn()
        .expect("model turn should allocate");
    let call_a = pending_tool_call("batch-a");
    let call_b = pending_tool_call("batch-b");
    transcript
        .push_tool_call(turn_id, call_a.clone())
        .expect("first call records");
    transcript
        .push_tool_call(turn_id, call_b.clone())
        .expect("second call records");
    transcript
        .close_model_response(turn_id, true)
        .expect("tool call response should close");
    let result_b_id = transcript
        .push_tool_result(
            call_b.id().clone(),
            ToolCallResult::succeeded(
                call_b.id().clone(),
                ArtifactRef::new(artifact_id("batch-b-result"), ArtifactKind::Json),
            ),
            artifact_id("batch-b-result"),
            ToolResultPromptProjection::Full,
        )
        .expect("second result records first");
    let result_a_id = transcript
        .push_tool_result(
            call_a.id().clone(),
            ToolCallResult::succeeded(
                call_a.id().clone(),
                ArtifactRef::new(artifact_id("batch-a-result"), ArtifactKind::Json),
            ),
            artifact_id("batch-a-result"),
            ToolResultPromptProjection::Full,
        )
        .expect("first result records second");
    let tail_turn_id = transcript
        .begin_model_turn()
        .expect("tail model turn should allocate");
    let tail_id = transcript
        .push_user_message(
            tail_turn_id,
            artifact_id("tail-user"),
            UserInputOrigin::ExternalUser,
        )
        .expect("tail records");

    transcript.remove_compacted_history(&[result_a_id.as_u64()].into_iter().collect());

    assert_eq!(
        transcript
            .items()
            .iter()
            .map(|item| item.id().as_u64())
            .collect::<Vec<_>>(),
        vec![1, result_b_id.as_u64(), tail_id.as_u64()]
    );
}

#[test]
fn session_records_user_tool_result_assistant_and_second_user_in_transcript_order() {
    let mut session = SessionState::new(session_id());
    let call = pending_tool_call("call-order");
    let first_turn = session
        .begin_model_turn()
        .expect("first model turn should begin");

    session
        .record_user_message_body(first_turn, "first user")
        .expect("first user records");
    session
        .record_tool_call_pending(first_turn, call.clone())
        .expect("tool call should become pending");
    session
        .close_model_response(first_turn, true)
        .expect("tool response should close");
    let artifact = ArtifactRef::new(artifact_id("tool-result-order"), ArtifactKind::Text);
    let result = ToolCallResult::succeeded(call.id().clone(), artifact);
    session
        .submit_tool_result(result, ArtifactContent::text("tool output"))
        .expect("tool result records");
    let assistant_turn = session
        .begin_model_turn()
        .expect("assistant model turn should begin");
    session
        .record_assistant_text_output(assistant_turn, "assistant answer".to_owned())
        .expect("assistant records");
    session
        .close_model_response(assistant_turn, false)
        .expect("assistant response should close");
    let second_user_turn = session
        .begin_model_turn()
        .expect("second user model turn should begin");
    session
        .record_user_message_body(second_user_turn, "second user")
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
