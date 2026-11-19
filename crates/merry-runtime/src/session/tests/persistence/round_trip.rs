use crate::{
    CompactedCheckpoint, ContextCompiler, ContextEntry, ContextSummary, FileSessionStore,
    ProjectRules, SkillCatalog, TaskAnchor,
    session::tests::{
        ActionAuditPolicy, ActionAuditStatus, ActionExecutionEvidence, ActionPolicyDisposition,
        ActionProposal, ActionProposalEvidence, ActionRiskTier, DefaultActionPolicy, ErrorInfo,
        ModelTurnStatus, SessionState, SessionStateTestExt, WorkspacePatchExecutionEvidence,
        WorkspacePatchProposal, pending_tool_call, persistence::persisted_image_message,
        session_id, tool_call_id,
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
    let stored: serde_json::Value = serde_json::from_slice(
        &store
            .read_state_bytes(&session_id())
            .await
            .expect("saved state should read"),
    )
    .expect("saved state should be JSON");
    assert_eq!(stored["format_version"], 6);
    assert!(
        stored.get("artifacts").is_none(),
        "v6 state.json must not contain a top-level artifacts table"
    );
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
async fn session_state_save_load_round_trips_action_audit_registry() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());

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
            ToolOutput::json(r#"{"ok":true}"#),
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
            ToolOutput::json(r#"{"ok":false}"#),
            diagnostic,
        )
        .expect("denial records");

    session.save_to(&store).await.expect("session saves");
    let loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("session loads");

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
