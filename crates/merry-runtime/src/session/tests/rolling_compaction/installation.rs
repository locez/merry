use crate::{
    CompactionError,
    compaction::CompactionPreparation,
    session::{
        tests::{
            CitationCompactionPolicy, RuntimeError, SessionId, SessionState, TaskAnchor,
            TranscriptItem,
            rolling_compaction::{
                checkpoint_candidate, policy, record_completed_tool_turn,
                record_completed_user_turn, window_budget,
            },
        },
        transcript::ToolResultPromptProjection,
    },
};

#[test]
fn task_anchor_change_invalidates_prepared_checkpoint_window() {
    let mut session =
        SessionState::new(SessionId::new("rolling-anchor-fingerprint").expect("valid id"));
    record_completed_user_turn(&mut session, "old prefix");
    record_completed_user_turn(&mut session, "retained tail");
    let input = session
        .build_citation_compaction_input(
            policy(1),
            policy(1).resolve(64_000).expect("budget resolves"),
        )
        .expect("input builds")
        .expect("old prefix is compressible");
    session.set_task_anchor(TaskAnchor::new("new objective").expect("valid anchor"));

    let error = session
        .install_citation_compaction_candidate(input, &checkpoint_candidate("h0"))
        .expect_err("anchor change makes the window stale");
    assert!(matches!(
        error,
        RuntimeError::Compaction {
            source: CompactionError::StaleWindow
        }
    ));
}

#[test]
fn invalid_candidate_does_not_apply_planned_tool_archives() {
    let mut session =
        SessionState::new(SessionId::new("rolling-invalid-no-archive").expect("valid id"));
    record_completed_user_turn(&mut session, "old prefix");
    for turn in 1..=5 {
        record_completed_tool_turn(
            &mut session,
            &format!("invalid-call-{turn}"),
            &format!("invalid-result-{turn}"),
            &"x".repeat(1_000),
        );
    }
    let budget = window_budget(1_300);
    let input = session
        .build_citation_compaction_input_with_window_budget(
            policy(5),
            policy(5).resolve(64_000).expect("budget resolves"),
            budget,
        )
        .expect("input builds")
        .expect("old prefix is compressible");
    assert!(!input.window_plan().archived_tool_call_ids().is_empty());

    let error = session
        .install_citation_compaction_candidate(
            input,
            &checkpoint_candidate("missing-checkpoint-ref"),
        )
        .expect_err("invalid candidate is rejected");
    assert!(matches!(error, RuntimeError::Checkpoint { .. }));

    let first_result = session
        .transcript
        .items
        .iter()
        .find_map(|item| match item {
            TranscriptItem::ToolResult {
                call_id,
                prompt_projection,
                ..
            } if call_id.as_str() == "invalid-call-1" => Some(*prompt_projection),
            _ => None,
        })
        .expect("first result exists");
    assert_eq!(first_result, ToolResultPromptProjection::Full);
}

#[test]
fn rendered_checkpoint_over_output_limit_rejects_without_state_mutation() {
    let mut session =
        SessionState::new(SessionId::new("rolling-rendered-too-large").expect("valid id"));
    record_completed_user_turn(&mut session, "old prefix");
    record_completed_user_turn(&mut session, "retained tail");
    let small_policy =
        CitationCompactionPolicy::new(Some(5), Some(10_000), 1).expect("valid small output policy");
    let input = session
        .build_citation_compaction_input(small_policy, small_policy.resolve(64_000).unwrap())
        .expect("input builds")
        .expect("old prefix is compressible");

    let error = session
        .install_citation_compaction_candidate(input, &checkpoint_candidate("h0"))
        .expect_err("rendered checkpoint exceeds token limit");
    assert!(matches!(
        error,
        RuntimeError::Compaction {
            source: CompactionError::RenderedCheckpointTooLarge { max_tokens: 5, .. }
        }
    ));
    assert_eq!(
        session.prompt_history_projection().compacted_through(),
        None
    );
    assert!(session.compacted_checkpoint.is_none());
    assert_eq!(
        session
            .provider_transcript_snapshot()
            .expect("provider projection builds")
            .len(),
        2
    );
}

#[test]
fn prepared_archive_only_install_is_read_only_until_commit() {
    let mut session = SessionState::new(
        SessionId::new("prepared-archive-only-install").expect("valid session id"),
    );
    for turn in 1..=5 {
        record_completed_tool_turn(
            &mut session,
            &format!("prepared-archive-call-{turn}"),
            &format!("prepared-archive-result-{turn}"),
            &format!("prepared archive body {turn} {}", "x".repeat(1_000)),
        );
    }
    let preparation = session
        .build_compaction_preparation_with_window_budget(
            policy(5),
            policy(5).resolve(64_000).expect("budget resolves"),
            window_budget(1_300),
        )
        .expect("preparation builds")
        .expect("archive-only preparation exists");
    let CompactionPreparation::ArchiveToolResults(input) = preparation else {
        panic!("expected archive-only preparation");
    };
    let bundle_before = session
        .persistable_bundle()
        .expect("session is persistable before prepare")
        .document_bytes;
    let transcript_before = session.transcript.persisted();
    let projection_before = session.prompt_history_projection();
    let checkpoint_before = session.compacted_checkpoint.clone();
    let archive_manifest_before = session.archived_ref_manifest.clone();

    let prepared = session
        .prepare_archive_only_compaction_install(input)
        .expect("archive-only install prepares");

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
    assert_ne!(prepared.transcript().persisted(), transcript_before);
    assert_eq!(prepared.prompt_history_projection(), projection_before);
    assert_eq!(prepared.compacted_checkpoint(), checkpoint_before.as_ref());
    assert_ne!(prepared.archived_ref_manifest(), &archive_manifest_before);
    assert_eq!(prepared.outcome(), None);
    session
        .revalidate_prepared_compaction_install(&prepared)
        .expect("unchanged session revalidates");

    assert_eq!(session.commit_prepared_compaction_install(prepared), None);
    assert_ne!(session.transcript.persisted(), transcript_before);
    assert_ne!(session.archived_ref_manifest, archive_manifest_before);
}
