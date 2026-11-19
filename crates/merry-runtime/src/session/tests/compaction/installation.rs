use crate::{
    CompactedCheckpoint,
    session::tests::{
        CitationCompactionPolicy, RuntimeError, SessionId, SessionState, SessionStateTestExt,
    },
};

#[test]
fn compaction_install_advances_prompt_boundary_without_deleting_full_transcript() {
    let mut session =
        SessionState::new(SessionId::new("compaction-prompt-boundary").expect("valid session id"));
    let covered_turn = session.begin_model_turn().expect("covered turn begins");
    session
        .record_user_message_body(covered_turn, "covered user")
        .expect("covered user records");
    session
        .record_assistant_text_output(covered_turn, "covered assistant".to_owned())
        .expect("covered assistant records");
    session
        .close_model_response(covered_turn, false)
        .expect("covered turn completes");
    let retained_turn = session.begin_model_turn().expect("retained turn begins");
    session
        .record_user_message_body(retained_turn, "retained user")
        .expect("retained user records");
    session
        .close_model_response(retained_turn, false)
        .expect("retained turn completes");

    let input = session
        .build_test_citation_compaction_input(
            CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy"),
        )
        .expect("input builds")
        .expect("covered turn is compressible");
    session
        .install_summary_compaction(input, "The first complete turn was compacted.")
        .expect("checkpoint installs");

    assert_eq!(
        session.transcript_items_for_tests(),
        vec![
            "user:covered user",
            "assistant:covered assistant",
            "user:retained user",
        ]
    );
    assert_eq!(
        session.provider_transcript_snapshot(),
        vec![crate::session::TranscriptItemSnapshot::UserMessage {
            text: "retained user".to_owned(),
            images: Vec::new(),
            origin: crate::session::UserInputOrigin::ExternalUser,
        }]
    );
}

#[test]
fn compaction_install_advances_boundary_through_trailing_empty_aborted_turn() {
    let mut session = SessionState::new(
        SessionId::new("compaction-empty-aborted-boundary").expect("valid session id"),
    );
    let covered_turn = session.begin_model_turn().expect("covered turn begins");
    session
        .record_user_message_body(covered_turn, "covered before empty turn")
        .expect("covered user records");
    session
        .close_model_response(covered_turn, false)
        .expect("covered turn completes");
    let empty_aborted_turn = session.begin_model_turn().expect("empty turn begins");
    session
        .abort_model_turn(empty_aborted_turn)
        .expect("empty turn aborts");
    let retained_turn = session.begin_model_turn().expect("retained turn begins");
    session
        .record_user_message_body(retained_turn, "retained after empty turn")
        .expect("retained user records");
    session
        .close_model_response(retained_turn, false)
        .expect("retained turn completes");

    let input = session
        .build_test_citation_compaction_input(
            CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy"),
        )
        .expect("input builds")
        .expect("covered prefix is compressible");
    session
        .install_summary_compaction(input, "The prefix before the retained turn was compacted.")
        .expect("checkpoint installs");

    assert_eq!(
        session.prompt_history_projection().compacted_through(),
        Some(empty_aborted_turn)
    );
    assert_eq!(
        session.provider_transcript_snapshot(),
        vec![crate::session::TranscriptItemSnapshot::UserMessage {
            text: "retained after empty turn".to_owned(),
            images: Vec::new(),
            origin: crate::session::UserInputOrigin::ExternalUser,
        }]
    );
}

#[test]
fn rolling_compaction_rejects_old_input_and_starts_after_current_boundary() {
    let mut session =
        SessionState::new(SessionId::new("rolling-compaction-boundary").expect("valid session id"));
    session
        .record_test_user_message_body("first covered user")
        .expect("covered user records");
    session
        .record_test_user_message_body("first retained user")
        .expect("retained user records");
    let policy = CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy");
    let input = session
        .build_test_citation_compaction_input(policy)
        .expect("first input builds")
        .expect("first prefix is compressible");
    let stale_input = input.clone();
    let summary = "The first user turn was compacted.";
    session
        .install_summary_compaction(input, summary)
        .expect("first checkpoint installs");

    let error = session
        .install_summary_compaction(stale_input, summary)
        .expect_err("the old input cannot cover the same turn twice");
    assert!(matches!(
        error,
        RuntimeError::Compaction {
            source: crate::CompactionError::StaleWindow
        }
    ));

    session
        .record_test_user_message_body("second retained user")
        .expect("new retained user records");
    let next_input = session
        .build_test_citation_compaction_input(policy)
        .expect("next input builds")
        .expect("the former tail is now compressible");
    let covered = next_input.render_covered_segment();
    assert!(covered.contains("first retained user"));
    assert!(!covered.contains("first covered user"));
    assert!(!covered.contains("second retained user"));
}

#[test]
fn compaction_window_covers_new_history_without_the_retained_tail() {
    let mut session = SessionState::new(SessionId::new("rolling-input").expect("valid session id"));
    let checkpoint = CompactedCheckpoint::new("The prior direction rejected resource timelines.")
        .expect("valid checkpoint");
    session.set_compacted_checkpoint(checkpoint);
    session
        .record_test_user_message_body("new user message to compact")
        .expect("user records");
    session
        .record_test_user_message_body("retained tail")
        .expect("user records");

    let input = session
        .build_test_citation_compaction_input(
            CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy"),
        )
        .expect("input builds")
        .expect("input exists");
    let covered = input.render_covered_segment();

    assert!(covered.contains("new user message to compact"));
    assert!(!covered.contains("retained tail"));
    assert!(!covered.contains("The prior direction rejected resource timelines."));
}

#[test]
fn installing_valid_checkpoint_hides_only_covered_history_from_provider() {
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

    let policy = CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy");
    let input = session
        .build_test_citation_compaction_input(policy)
        .expect("input builds")
        .expect("input exists");
    let outcome = session
        .install_summary_compaction(
            input,
            "The older user and assistant messages were covered by compaction.",
        )
        .expect("install succeeds");

    assert_eq!(outcome.covered_history_item_count(), 2);
    assert_eq!(
        session.transcript_items_for_tests(),
        vec!["user:old user", "assistant:old assistant", "user:tail user"]
    );
    assert_eq!(
        session.provider_transcript_snapshot(),
        vec![crate::session::TranscriptItemSnapshot::UserMessage {
            text: "tail user".to_owned(),
            images: Vec::new(),
            origin: crate::session::UserInputOrigin::ExternalUser,
        }]
    );
    assert!(
        session
            .context_snapshot()
            .compacted_checkpoint_for_tests()
            .is_some()
    );
}
