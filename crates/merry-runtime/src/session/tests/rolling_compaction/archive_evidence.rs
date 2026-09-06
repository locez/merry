use crate::{
    CheckpointError, FileSessionStore,
    compaction::{CompactionPreparation, checkpoint_from_candidate_json},
    session::{
        tests::{
            ArtifactContent, ArtifactKind, ArtifactRef, ErrorInfo, PendingToolCallBatch,
            RuntimeError, SessionId, SessionState, ToolCallBatchId, ToolCallResult, TranscriptItem,
            artifact_id, pending_tool_call,
            rolling_compaction::{
                checkpoint_candidate, policy, record_completed_tool_turn,
                record_completed_user_turn, rolling_keep_candidate, window_budget,
            },
            tool_call_id,
        },
        transcript::ToolResultPromptProjection,
    },
};

#[test]
fn reverse_tool_results_archive_by_result_arrival_and_keep_pairs_valid() {
    let mut session =
        SessionState::new(SessionId::new("rolling-reverse-results").expect("valid session id"));
    record_completed_user_turn(&mut session, "old prefix");

    let tool_turn = session.begin_model_turn().expect("tool turn begins");
    let call_a = pending_tool_call("reverse-call-a");
    let call_b = pending_tool_call("reverse-call-b");
    session
        .record_tool_call_batch_pending(
            tool_turn,
            PendingToolCallBatch::new(
                ToolCallBatchId::new("reverse-batch").expect("valid batch id"),
                vec![call_a.clone(), call_b.clone()],
            )
            .expect("valid batch"),
        )
        .expect("calls record");
    session
        .close_model_response(tool_turn, true)
        .expect("tool response closes");
    for (call, artifact, body) in [
        (&call_b, "reverse-result-b", "b".repeat(1_000)),
        (&call_a, "reverse-result-a", "a".repeat(1_000)),
    ] {
        session
            .submit_tool_result(
                ToolCallResult::succeeded(
                    call.id().clone(),
                    ArtifactRef::new(artifact_id(artifact), ArtifactKind::Text),
                ),
                ArtifactContent::text(body),
            )
            .expect("tool result records");
    }
    for turn in 3..=6 {
        record_completed_user_turn(&mut session, &format!("small retained {turn}"));
    }

    let budget = window_budget(450);
    let plan = session
        .plan_compaction_window(policy(5), budget)
        .expect("plan succeeds")
        .expect("old prefix is compressible");
    assert_eq!(
        plan.archived_tool_call_ids_for_tests(),
        vec![call_b.id().clone()],
        "the first arriving result must be archived first"
    );

    let resolved = policy(5).resolve(64_000).expect("budget resolves");
    let input = session
        .build_citation_compaction_input_with_window_budget(policy(5), resolved, budget)
        .expect("input builds")
        .expect("old prefix is compressible");
    let result_b_ref = session
        .transcript
        .items
        .iter()
        .find_map(|item| match item {
            TranscriptItem::ToolResult { id, call_id, .. } if call_id == call_b.id() => {
                Some(format!("h{}", id.as_u64()))
            }
            _ => None,
        })
        .expect("result B ref exists");
    assert!(
        input
            .manifest()
            .refs()
            .iter()
            .any(|reference| reference.id().as_str() == result_b_ref)
    );

    session
        .install_citation_compaction_candidate(input, &checkpoint_candidate("h0"))
        .expect("checkpoint installs");
    let provider = session
        .provider_transcript_snapshot()
        .expect("provider projection builds");
    assert!(matches!(
        &provider[0],
        crate::session::TranscriptItemSnapshot::ToolCall { call }
            if call.id() == call_a.id()
    ));
    assert!(matches!(
        &provider[1],
        crate::session::TranscriptItemSnapshot::ToolCall { call }
            if call.id() == call_b.id()
    ));
    let notice = provider
        .iter()
        .find_map(|item| match item {
            crate::session::TranscriptItemSnapshot::ToolResult {
                call_id, content, ..
            } if call_id == call_b.id() => content.as_text(),
            _ => None,
        })
        .expect("result B notice exists");
    let notice: serde_json::Value = serde_json::from_str(notice).expect("notice is typed JSON");
    assert_eq!(notice["merry_archived"], true);
    assert_eq!(notice["status"], "succeeded");
    assert_eq!(notice["artifact_id"], "reverse-result-b");
    assert_eq!(notice["ref"], result_b_ref);

    let result_a = provider
        .iter()
        .find_map(|item| match item {
            crate::session::TranscriptItemSnapshot::ToolResult {
                call_id, content, ..
            } if call_id == call_a.id() => content.as_text(),
            _ => None,
        })
        .expect("result A remains visible");
    assert_eq!(result_a, "a".repeat(1_000));
}

#[test]
fn retained_archive_ref_stays_pinned_but_hidden_across_rolling_compactions() {
    let mut session =
        SessionState::new(SessionId::new("rolling-existing-notice").expect("valid id"));
    record_completed_user_turn(&mut session, "old prefix");
    record_completed_user_turn(&mut session, "older retained turn");
    record_completed_tool_turn(
        &mut session,
        "existing-notice-call",
        "existing-notice-result",
        &"x".repeat(1_000),
    );
    for turn in 4..=6 {
        record_completed_user_turn(&mut session, &format!("small retained {turn}"));
    }
    let (result_ref, result_projection) = session
        .transcript
        .items
        .iter_mut()
        .find_map(|item| match item {
            TranscriptItem::ToolResult {
                id,
                call_id,
                prompt_projection,
                ..
            } if call_id.as_str() == "existing-notice-call" => {
                Some((format!("h{}", id.as_u64()), prompt_projection))
            }
            _ => None,
        })
        .expect("existing notice result exists");
    *result_projection = ToolResultPromptProjection::ArtifactNotice;

    let input = session
        .build_citation_compaction_input_with_window_budget(
            policy(5),
            policy(5).resolve(64_000).expect("budget resolves"),
            window_budget(10_000),
        )
        .expect("input builds")
        .expect("old prefix is compressible");
    assert_eq!(
        input.window_plan().archived_tool_call_ids_for_tests(),
        vec![tool_call_id("existing-notice-call")]
    );
    assert!(
        input
            .pinned_refs()
            .iter()
            .any(|id| id.as_str() == result_ref)
    );

    let error = checkpoint_from_candidate_json(
        input.manifest().checkpoint_id().clone(),
        &input,
        &checkpoint_candidate(&result_ref),
    )
    .expect_err("a retained-tail ref hidden from the compactor must be rejected");
    assert!(matches!(
        error,
        RuntimeError::Checkpoint {
            source: CheckpointError::UnknownRef { ref ref_id, .. },
        } if ref_id == &result_ref
    ));

    session
        .install_citation_compaction_candidate(input, &checkpoint_candidate("h0"))
        .expect("checkpoint installs");
    let checkpoint = session
        .compacted_checkpoint
        .as_ref()
        .and_then(crate::CompactedCheckpoint::citation_backed)
        .expect("citation checkpoint installed");
    assert!(
        checkpoint
            .manifest()
            .refs()
            .iter()
            .any(|reference| reference.id().as_str() == result_ref)
    );

    record_completed_user_turn(&mut session, "new turn after first compaction");
    let second_input = session
        .build_citation_compaction_input_with_window_budget(
            policy(5),
            policy(5).resolve(64_000).expect("budget resolves"),
            window_budget(10_000),
        )
        .expect("second input builds")
        .expect("the next oldest turn is compressible");
    assert!(
        second_input
            .pinned_refs()
            .iter()
            .any(|id| id.as_str() == result_ref)
    );
    let payload: serde_json::Value = serde_json::from_str(
        &second_input
            .to_model_payload_json()
            .expect("second payload serializes"),
    )
    .expect("second payload parses");
    let previous_ref_ids = payload["previous_checkpoint"]["original_ref_manifest"]["refs"]
        .as_array()
        .expect("previous original refs")
        .iter()
        .map(|reference| reference["id"].as_str().expect("ref id"))
        .collect::<Vec<_>>();
    assert!(previous_ref_ids.contains(&"h0"));
    assert!(
        !previous_ref_ids.contains(&result_ref.as_str()),
        "a pinned-only retained ref must not become previous-checkpoint evidence"
    );
    let available_ref_ids = payload["available_ref_ids"]
        .as_array()
        .expect("available refs")
        .iter()
        .map(|ref_id| ref_id.as_str().expect("available ref id"))
        .collect::<Vec<_>>();
    assert!(available_ref_ids.contains(&"h0"));
    assert!(
        !available_ref_ids.contains(&result_ref.as_str()),
        "a pinned-only retained ref must not become an available model ref"
    );

    let error = checkpoint_from_candidate_json(
        second_input.manifest().checkpoint_id().clone(),
        &second_input,
        &checkpoint_candidate(&result_ref),
    )
    .expect_err("the hidden ref must remain invalid on the next rolling compaction");
    assert!(matches!(
        error,
        RuntimeError::Checkpoint {
            source: CheckpointError::UnknownRef { ref ref_id, .. },
        } if ref_id == &result_ref
    ));
    checkpoint_from_candidate_json(
        second_input.manifest().checkpoint_id().clone(),
        &second_input,
        &rolling_keep_candidate("h0"),
    )
    .expect("a previous entry's supplied original ref remains valid");
}

#[test]
fn archives_tried_in_five_turn_plan_do_not_leak_into_three_turn_fallback() {
    let mut session = SessionState::new(SessionId::new("rolling-archive-reset").expect("valid id"));
    record_completed_user_turn(&mut session, "old prefix");
    for turn in 1..=2 {
        let turn_id = session.begin_model_turn().expect("large tool turn begins");
        session
            .record_user_message_body(turn_id, &"discussion".repeat(200))
            .expect("large discussion records");
        let call = pending_tool_call(&format!("dropped-call-{turn}"));
        session
            .record_tool_call_batch_pending(
                turn_id,
                PendingToolCallBatch::new(
                    ToolCallBatchId::new(&format!("dropped-batch-{turn}")).expect("valid batch id"),
                    vec![call.clone()],
                )
                .expect("valid batch"),
            )
            .expect("tool call records");
        session
            .close_model_response(turn_id, true)
            .expect("tool response closes");
        session
            .submit_tool_result(
                ToolCallResult::succeeded(
                    call.id().clone(),
                    ArtifactRef::new(
                        artifact_id(&format!("dropped-result-{turn}")),
                        ArtifactKind::Text,
                    ),
                ),
                ArtifactContent::text("tool body".repeat(100)),
            )
            .expect("tool result records");
    }
    for turn in 4..=6 {
        record_completed_user_turn(&mut session, &format!("small retained {turn}"));
    }

    let plan = session
        .plan_compaction_window(policy(5), window_budget(400))
        .expect("fallback plan succeeds")
        .expect("three-turn fallback covers the large tool turns");

    assert_eq!(plan.retained_turn_ids_u64(), vec![4, 5, 6]);
    assert!(plan.archived_tool_call_ids().is_empty());
}

#[tokio::test]
async fn archive_only_manifest_resolves_refs_and_round_trips_through_store() {
    let session_id = SessionId::new("rolling-archive-manifest").expect("valid id");
    let mut session = SessionState::new(session_id.clone());
    for turn in 1..=5 {
        record_completed_tool_turn(
            &mut session,
            &format!("manifest-call-{turn}"),
            &format!("manifest-result-{turn}"),
            &format!("manifest exact body {turn} {}", "x".repeat(1_000)),
        );
    }
    let preparation = session
        .build_compaction_preparation_with_window_budget(
            policy(5),
            policy(5).resolve(64_000).expect("budget resolves"),
            window_budget(1_300),
        )
        .expect("preparation builds")
        .expect("archive-only is required");
    let CompactionPreparation::ArchiveToolResults(input) = preparation else {
        panic!("expected archive-only preparation");
    };
    let archived_ref = input
        .archived_refs()
        .first()
        .expect("one archived ref exists")
        .id()
        .clone();
    session
        .install_archive_only_compaction(input)
        .expect("archive-only install succeeds");

    assert!(session.compacted_checkpoint.is_none());
    let page = session
        .read_checkpoint_ref_page(&archived_ref, 0, 2_000)
        .expect("archive manifest resolves exact source");
    assert!(page.content().contains("manifest exact body 1"));

    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    session.save_to(&store).await.expect("session saves");
    let loaded = SessionState::load_from(&store, &session_id)
        .await
        .expect("session loads");
    assert!(loaded.compacted_checkpoint.is_none());
    let loaded_page = loaded
        .read_checkpoint_ref_page(&archived_ref, 0, 2_000)
        .expect("loaded archive manifest resolves exact source");
    assert_eq!(loaded_page.content(), page.content());
}

#[test]
fn failed_archived_result_notice_has_exact_four_json_fields() {
    let mut session = SessionState::new(SessionId::new("rolling-failed-notice").expect("valid id"));
    let turn_id = session.begin_model_turn().expect("failed tool turn begins");
    let failed_call = pending_tool_call("failed-notice-call");
    session
        .record_tool_call_batch_pending(
            turn_id,
            PendingToolCallBatch::new(
                ToolCallBatchId::new("failed-notice-batch").expect("valid batch id"),
                vec![failed_call.clone()],
            )
            .expect("valid batch"),
        )
        .expect("failed call records");
    session
        .close_model_response(turn_id, true)
        .expect("tool response closes");
    session
        .submit_tool_result(
            ToolCallResult::failed(
                failed_call.id().clone(),
                ArtifactRef::new(artifact_id("failed-notice-result"), ArtifactKind::Text),
                ErrorInfo::new("tool_failed", "expected failure").expect("valid diagnostic"),
            ),
            ArtifactContent::text("f".repeat(1_000)),
        )
        .expect("failed result records");
    for turn in 2..=5 {
        record_completed_tool_turn(
            &mut session,
            &format!("failed-tail-call-{turn}"),
            &format!("failed-tail-result-{turn}"),
            &"x".repeat(1_000),
        );
    }

    let preparation = session
        .build_compaction_preparation_with_window_budget(
            policy(5),
            policy(5).resolve(64_000).expect("budget resolves"),
            window_budget(1_300),
        )
        .expect("preparation builds")
        .expect("archive-only is required");
    let CompactionPreparation::ArchiveToolResults(input) = preparation else {
        panic!("expected archive-only preparation");
    };
    session
        .install_archive_only_compaction(input)
        .expect("archive-only install succeeds");

    let notice = session
        .provider_transcript_snapshot()
        .expect("provider projection builds")
        .into_iter()
        .find_map(|item| match item {
            crate::session::TranscriptItemSnapshot::ToolResult {
                call_id, content, ..
            } if call_id == *failed_call.id() => content.as_text().map(str::to_owned),
            _ => None,
        })
        .expect("failed notice exists");
    let notice: serde_json::Value = serde_json::from_str(&notice).expect("notice parses");
    let object = notice.as_object().expect("notice is an object");
    assert_eq!(object.len(), 4);
    assert_eq!(notice["merry_archived"], true);
    assert_eq!(notice["status"], "failed");
    assert_eq!(notice["artifact_id"], "failed-notice-result");
    assert!(
        notice["ref"]
            .as_str()
            .is_some_and(|value| value.starts_with('h'))
    );
}
