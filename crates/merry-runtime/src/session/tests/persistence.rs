use super::*;
use crate::{
    ContextCompiler, ContextEntry, ContextEvidence, ContextSummary, FileSessionStore, ProjectRules,
    SkillCatalog, TaskAnchor, artifact::ArtifactContent,
};
use merry_core::{
    ArtifactKind, ArtifactRef, EvidenceLocator, EvidenceRef, ToolCallResult, ToolCallResultStatus,
};

#[tokio::test]
async fn session_state_save_load_round_trips_next_reasoning_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());

    session
        .record_user_message_body("remember this user fact")
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
    session.set_compacted_checkpoint(citation_plain_runtime_checkpoint_for_tests(
        "resume-checkpoint",
        "resume checkpoint text",
    ));
    session
        .record_tool_call_pending(pending_tool_call("call-resume"))
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
        .record_tool_call_pending(pending_tool_call("pending-save"))
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
        .record_tool_call_pending(executed_call.clone())
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
        .record_tool_call_pending(call.clone())
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
