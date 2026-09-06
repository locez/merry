use crate::session::tests::{
    CitationCompactionPolicy, RuntimeError, SessionId, SessionState, SessionStateTestExt,
    TaskAnchor, pending_tool_call,
};

#[test]
fn prepared_checkpoint_install_is_read_only_until_infallible_commit() {
    let new_session = || {
        let mut session = SessionState::new(
            SessionId::new("prepared-checkpoint-install").expect("valid session id"),
        );
        session
            .record_test_user_message_body("old user")
            .expect("old user records");
        session
            .record_test_user_message_body("retained user")
            .expect("retained user records");
        session
    };
    let policy = CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy");
    let candidate_json = r#"{
      "confirmed_decisions": [],
      "rejected_approaches": [],
      "constraints_preferences_boundaries": [],
      "corrected_misunderstandings": [],
      "durable_conclusions": [{
        "id": "c1",
        "text": "The old user turn was compacted.",
        "refs": ["h0"]
      }],
      "open_questions": [],
      "current_progress_and_next_steps": [],
      "exact_details": [],
      "handoffs": []
    }"#;

    let mut expected = new_session();
    let expected_input = expected
        .build_test_citation_compaction_input(policy)
        .expect("expected input builds")
        .expect("expected input exists");
    let expected_outcome = expected
        .install_citation_compaction_candidate(expected_input, candidate_json)
        .expect("compatibility install succeeds");

    let mut session = new_session();
    let input = session
        .build_test_citation_compaction_input(policy)
        .expect("prepared input builds")
        .expect("prepared input exists");
    let bundle_before = session
        .persistable_bundle()
        .expect("session is persistable before prepare")
        .document_bytes;
    let transcript_before = session.transcript.persisted();
    let projection_before = session.prompt_history_projection();
    let checkpoint_before = session.compacted_checkpoint.clone();
    let archive_manifest_before = session.archived_ref_manifest.clone();
    let fingerprint_before = session
        .compaction_window_fingerprint()
        .expect("fingerprint builds before prepare");

    let prepared = session
        .prepare_citation_compaction_install(input, candidate_json)
        .expect("checkpoint install prepares");

    assert_eq!(session.transcript.persisted(), transcript_before);
    assert_eq!(session.prompt_history_projection(), projection_before);
    assert_eq!(session.compacted_checkpoint, checkpoint_before);
    assert_eq!(session.archived_ref_manifest, archive_manifest_before);
    assert_eq!(
        session
            .persistable_bundle()
            .expect("session remains persistable after prepare")
            .document_bytes,
        bundle_before
    );
    assert_eq!(prepared.original_fingerprint(), fingerprint_before);
    assert_eq!(
        prepared.transcript().persisted(),
        expected.transcript.persisted()
    );
    assert_eq!(
        prepared.prompt_history_projection(),
        expected.prompt_history_projection()
    );
    assert_eq!(
        prepared.compacted_checkpoint(),
        expected.compacted_checkpoint.as_ref()
    );
    assert_eq!(
        prepared.archived_ref_manifest(),
        &expected.archived_ref_manifest
    );
    assert_eq!(prepared.outcome(), Some(&expected_outcome));
    session
        .revalidate_prepared_compaction_install(&prepared)
        .expect("unchanged session revalidates");

    let outcome = session
        .commit_prepared_compaction_install(prepared)
        .expect("replacement commit returns its outcome");

    assert_eq!(outcome, expected_outcome);
    assert_eq!(
        session
            .persistable_bundle()
            .expect("committed session is persistable")
            .document_bytes,
        expected
            .persistable_bundle()
            .expect("compatibility session is persistable")
            .document_bytes
    );
}

#[test]
fn prepared_checkpoint_install_revalidation_rejects_changed_fingerprint() {
    let mut session =
        SessionState::new(SessionId::new("prepared-checkpoint-stale").expect("valid session id"));
    session
        .record_test_user_message_body("old user")
        .expect("old user records");
    session
        .record_test_user_message_body("retained user")
        .expect("retained user records");
    let policy = CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy");
    let input = session
        .build_test_citation_compaction_input(policy)
        .expect("input builds")
        .expect("input exists");
    let prepared = session
        .prepare_citation_compaction_install(
            input,
            r#"{
              "confirmed_decisions": [],
              "rejected_approaches": [],
              "constraints_preferences_boundaries": [],
              "corrected_misunderstandings": [],
              "durable_conclusions": [{
                "id": "c1",
                "text": "The old user turn was compacted.",
                "refs": ["h0"]
              }],
              "open_questions": [],
              "current_progress_and_next_steps": [],
              "exact_details": [],
              "handoffs": []
            }"#,
        )
        .expect("install prepares");
    session.set_task_anchor(TaskAnchor::new("changed objective").expect("valid anchor"));

    let error = session
        .revalidate_prepared_compaction_install(&prepared)
        .expect_err("changed fingerprint rejects prepared install");

    assert!(matches!(
        error,
        RuntimeError::Compaction {
            source: crate::CompactionError::StaleWindow
        }
    ));
}

#[test]
fn prepared_checkpoint_install_revalidation_rejects_pending_tool_calls() {
    let mut session =
        SessionState::new(SessionId::new("prepared-checkpoint-pending").expect("valid session id"));
    session
        .record_test_user_message_body("old user")
        .expect("old user records");
    session
        .record_test_user_message_body("retained user")
        .expect("retained user records");
    let policy = CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy");
    let input = session
        .build_test_citation_compaction_input(policy)
        .expect("input builds")
        .expect("input exists");
    let prepared = session
        .prepare_citation_compaction_install(
            input,
            r#"{
              "confirmed_decisions": [],
              "rejected_approaches": [],
              "constraints_preferences_boundaries": [],
              "corrected_misunderstandings": [],
              "durable_conclusions": [{
                "id": "c1",
                "text": "The old user turn was compacted.",
                "refs": ["h0"]
              }],
              "open_questions": [],
              "current_progress_and_next_steps": [],
              "exact_details": [],
              "handoffs": []
            }"#,
        )
        .expect("install prepares");
    session
        .pending_tool_calls
        .push(pending_tool_call("late-pending-call"));

    let error = session
        .revalidate_prepared_compaction_install(&prepared)
        .expect_err("pending tool call rejects prepared install");

    assert!(matches!(
        error,
        RuntimeError::Compaction {
            source: crate::CompactionError::StaleWindow
        }
    ));
}
