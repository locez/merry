use crate::session::tests::{
    CitationCompactionPolicy, RuntimeError, SessionId, SessionState, SessionStateTestExt,
    citation_plain_runtime_checkpoint_for_tests,
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
        .install_citation_compaction_candidate(
            input,
            r#"{
              "confirmed_decisions": [],
              "rejected_approaches": [],
              "constraints_preferences_boundaries": [],
              "corrected_misunderstandings": [],
              "durable_conclusions": [{
                "id": "c1",
                "text": "The first complete turn was compacted.",
                "refs": ["h0", "h1"]
              }],
              "open_questions": [],
              "current_progress_and_next_steps": [],
              "exact_details": [],
              "handoffs": []
            }"#,
        )
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
        session
            .provider_transcript_snapshot()
            .expect("provider projection builds"),
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
        .install_citation_compaction_candidate(
            input,
            r#"{
              "confirmed_decisions": [],
              "rejected_approaches": [],
              "constraints_preferences_boundaries": [],
              "corrected_misunderstandings": [],
              "durable_conclusions": [{
                "id": "c1",
                "text": "The prefix before the retained turn was compacted.",
                "refs": ["h0"]
              }],
              "open_questions": [],
              "current_progress_and_next_steps": [],
              "exact_details": [],
              "handoffs": []
            }"#,
        )
        .expect("checkpoint installs");

    assert_eq!(
        session.prompt_history_projection().compacted_through(),
        Some(empty_aborted_turn)
    );
    assert_eq!(
        session
            .provider_transcript_snapshot()
            .expect("provider projection builds"),
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
    let candidate = r#"{
      "confirmed_decisions": [],
      "rejected_approaches": [],
      "constraints_preferences_boundaries": [],
      "corrected_misunderstandings": [],
      "durable_conclusions": [{
        "id": "c1",
        "text": "The first user turn was compacted.",
        "refs": ["h0"]
      }],
      "open_questions": [],
      "current_progress_and_next_steps": [],
      "exact_details": [],
      "handoffs": []
    }"#;
    session
        .install_citation_compaction_candidate(input, candidate)
        .expect("first checkpoint installs");

    let error = session
        .install_citation_compaction_candidate(stale_input, candidate)
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
    let next_payload = next_input
        .to_model_payload_json()
        .expect("next payload serializes");
    assert!(next_payload.contains("first retained user"));
    assert!(!next_payload.contains("first covered user"));
    assert!(!next_payload.contains("second retained user"));
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
    let payload = input.to_model_payload_json().expect("payload serializes");

    assert!(payload.contains("previous_checkpoint"));
    assert!(payload.contains("The prior direction rejected resource timelines."));
    assert!(payload.contains("new user message to compact"));
    assert!(!payload.contains("retained tail"));
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
    let candidate_json = r#"{
          "confirmed_decisions": [],
          "rejected_approaches": [],
          "constraints_preferences_boundaries": [],
          "corrected_misunderstandings": [],
          "durable_conclusions": [
            {
              "id": "c1",
              "text": "The older user and assistant messages were covered by compaction.",
              "refs": ["h0", "h1"]
            }
          ],
          "open_questions": [],
          "current_progress_and_next_steps": [],
          "exact_details": [],
          "handoffs": []
        }"#;

    let outcome = session
        .install_citation_compaction_candidate(input, candidate_json)
        .expect("install succeeds");

    assert_eq!(outcome.covered_history_item_count(), 2);
    assert_eq!(
        session.transcript_items_for_tests(),
        vec!["user:old user", "assistant:old assistant", "user:tail user"]
    );
    assert_eq!(
        session
            .provider_transcript_snapshot()
            .expect("provider projection builds"),
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

#[test]
fn failed_checkpoint_install_keeps_history_unchanged() {
    let mut session =
        SessionState::new(SessionId::new("install-checkpoint-rollback").expect("valid session id"));
    session
        .record_test_user_message_body("old user")
        .expect("user records");
    session
        .record_test_user_message_body("tail user")
        .expect("user records");

    let policy = CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy");
    let input = session
        .build_test_citation_compaction_input(policy)
        .expect("input builds")
        .expect("input exists");
    let bad_candidate_json = r#"{
          "confirmed_decisions": [],
          "rejected_approaches": [],
          "constraints_preferences_boundaries": [
            {
              "id": "c1",
              "text": "This cites a missing ref.",
              "refs": ["r-missing"]
            }
          ],
          "corrected_misunderstandings": [],
          "durable_conclusions": [],
          "open_questions": [],
          "current_progress_and_next_steps": [],
          "exact_details": [],
          "handoffs": []
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
