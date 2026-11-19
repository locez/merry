use crate::{
    CompactedCheckpoint, ContextCompiler, ContextEntry, ContextSummary, FileSessionStore,
    ProjectRules, SkillCatalog, TaskAnchor,
    session::tests::{
        ModelTurnStatus, SessionState, SessionStateTestExt, pending_tool_call,
        persistence::persisted_image_message, session_id, tool_call_id,
    },
};
use merry_core::{ToolCallResult, ToolCallResultStatus, ToolOutput};

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
    let snapshot = loaded.provider_transcript_snapshot();
    let [crate::session::TranscriptItemSnapshot::UserMessage { text, images, .. }] =
        snapshot.as_slice()
    else {
        panic!("provider history should contain one image message");
    };
    assert_eq!(text, message.text());
    assert_eq!(images.as_slice(), message.images());
}

#[tokio::test]
async fn session_state_round_trip_preserves_turns_user_text_and_projections() {
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
    let stored_bytes = store
        .read_state_bytes(&session_id())
        .await
        .expect("saved state should read");
    let stored = crate::session::last_commit_json(&stored_bytes);
    let stored_text = String::from_utf8_lossy(&stored_bytes);
    assert!(stored_text.contains("\"format_version\":1"));
    assert!(
        !stored_text.contains("trajectory_snapshot"),
        "the journal does not persist a trajectory snapshot"
    );
    assert!(stored.get("prompt_history_projection").is_none());
    assert!(
        stored["items"]
            .as_array()
            .expect("items")
            .iter()
            .all(|item| item.get("prompt_projection").is_none()),
        "prompt projections are derived on load"
    );
    let loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("session should load");

    assert_eq!(loaded.transcript.persisted(), transcript_before);
    let full = loaded.full_transcript_snapshot();
    assert_eq!(
        full.len(),
        4,
        "hidden final-output exchange remains in the full transcript view"
    );
    assert!(matches!(
        &full[0],
        crate::session::TranscriptItemSnapshot::UserMessage { text, .. }
            if text == "  exact persisted user text\n"
    ));
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
async fn session_state_save_load_round_trips_next_reasoning_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());

    session
        .record_test_user_message_body("remember this user fact")
        .expect("user message records");
    session
        .record_context_entry(ContextEntry::summary(
            ContextSummary::new("resume-summary", "A grounded summary for resume.")
                .expect("summary"),
        ))
        .expect("context records");
    session.set_compacted_checkpoint(
        CompactedCheckpoint::new("resume checkpoint text").expect("valid checkpoint"),
    );
    session
        .record_test_tool_call_pending(pending_tool_call("call-resume"))
        .expect("pending call records");
    session
        .submit_tool_result(
            ToolCallResult::new(
                tool_call_id("call-resume"),
                ToolCallResultStatus::Succeeded,
                None,
            )
            .expect("tool result"),
            ToolOutput::text("manual result"),
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
    assert_eq!(
        loaded.read_tool_output(&tool_call_id("call-resume")),
        Some(ToolOutput::text("manual result"))
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

#[tokio::test]
async fn subsequent_journal_commits_only_append_deltas_and_strip_process_admission_metadata() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());

    let turn1 = session.begin_model_turn().expect("turn 1 begins");
    session
        .record_user_message_body(turn1, "turn one input")
        .expect("turn 1 user");
    session
        .record_assistant_text_output(turn1, "turn one answer".to_owned())
        .expect("turn 1 assistant");
    session
        .close_model_response(turn1, false)
        .expect("turn 1 closes");
    session.save_to(&store).await.expect("first commit saves");

    let turn2 = session.begin_model_turn().expect("turn 2 begins");
    session
        .record_user_message_body(turn2, "turn two input")
        .expect("turn 2 user");
    let call = pending_tool_call("call-proc-sanitize");
    let batch = merry_core::PendingToolCallBatch::new(
        merry_core::ToolCallBatchId::new("batch-proc-sanitize").expect("batch id"),
        vec![call.clone()],
    )
    .expect("batch");
    session
        .record_tool_call_batch_pending(turn2, batch)
        .expect("tool call records");
    session
        .close_model_response(turn2, true)
        .expect("turn 2 awaits tool");
    let full_process = ToolOutput::json(
        serde_json::json!({
            "kind": "process_action",
            "ok": true,
            "permission_profile_id": "process.read_only",
            "permission_review": {"rationale": "secret policy note"},
            "status": {"kind": "exited", "code": 0},
            "stdout": {"text": "clean output\n", "bytes": 13, "truncated": false, "utf8": true},
            "stderr": {"text": "", "bytes": 0, "truncated": false, "utf8": true},
            "input_evidence": {
                "kind": "shell_command_script",
                "shell": "bash",
                "flag": "-lc",
                "script": "echo clean output",
                "script_bytes": 17,
                "script_fingerprint": "fnv1a64:deadbeef"
            }
        })
        .to_string(),
    );
    let model_process = ToolOutput::text("exit 0\nstdout:\nclean output\n");
    session
        .submit_tool_result(
            ToolCallResult::succeeded(call.id().clone()),
            crate::tool::ToolResultContent::from(full_process).with_model(model_process.clone()),
        )
        .expect("process result records");
    session.save_to(&store).await.expect("second commit saves");

    let bytes = store
        .read_journal_bytes(&session_id())
        .await
        .expect("journal reads");
    let text = String::from_utf8(bytes).expect("utf8");
    let lines = text.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 3, "expected header + 2 commits");

    let second_commit: serde_json::Value =
        serde_json::from_str(lines[2]).expect("second commit is JSON");
    assert!(
        second_commit["state"]
            .get("external_tool_catalog")
            .is_none(),
        "external_tool_catalog must not be written to journal.jsonl"
    );
    assert!(
        second_commit["state"].get("active_plan").is_none(),
        "unchanged active_plan must not repeat on later commits"
    );
    assert!(
        second_commit["state"].get("terminal_plans").is_none(),
        "empty terminal_plans must not be serialized"
    );
    assert!(
        second_commit["state"].get("context_entries").is_none(),
        "empty context_entries must not be serialized"
    );
    assert!(
        second_commit["state"].get("session_started").is_none(),
        "derivable session_started must not be serialized"
    );
    assert!(
        second_commit["state"].get("next_item_id").is_none(),
        "derivable next_item_id must not be serialized"
    );
    assert!(
        second_commit["state"].get("next_model_turn_id").is_none(),
        "derivable next_model_turn_id must not be serialized"
    );
    let tool_result_item = second_commit["items"]
        .as_array()
        .expect("items array")
        .iter()
        .find(|item| item["type"] == "tool_result")
        .expect("tool_result item");
    assert!(
        tool_result_item.get("result").is_none(),
        "tool_result must flatten status/diagnostic without duplicating call_id"
    );
    assert!(
        tool_result_item.get("diagnostic").is_none(),
        "succeeded tool_result must omit null diagnostic"
    );
    assert_eq!(tool_result_item["status"], "succeeded");
    let turns = second_commit["state"]["model_turns"]
        .as_array()
        .expect("turn delta array");
    assert_eq!(turns.len(), 1, "only new/changed model turn is written");
    assert_eq!(turns[0]["id"], 2);
    assert!(
        !lines[2].contains("permission_profile_id")
            && !lines[2].contains("permission_review")
            && !lines[2].contains("input_evidence")
            && !lines[2].contains("script_fingerprint"),
        "non-durable process admission/fingerprint metadata must not enter journal.jsonl"
    );
    assert!(
        !lines[2].contains("\"model_output\""),
        "derivable process model_output must not be duplicated in journal.jsonl"
    );

    let loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("journal reloads cleanly");
    assert_eq!(
        loaded
            .read_model_tool_preview(call.id(), 4096)
            .map(|preview| preview.content().to_owned()),
        Some(model_process.as_str().to_owned())
    );
}
