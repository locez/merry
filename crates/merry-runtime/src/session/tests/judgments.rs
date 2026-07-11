use super::*;

#[test]
fn summary_draft_judgment_rejects_missing_evidence_without_registry_record() {
    let mut session = SessionState::new(session_id());
    let request = summary_draft_request(vec![judgment_evidence(
        "missing source",
        "missing-judgment-artifact",
        EvidenceLocator::whole_artifact(),
    )]);
    let outcome = summary_draft_outcome(vec![judgment_evidence(
        "missing source",
        "missing-judgment-artifact",
        EvidenceLocator::whole_artifact(),
    )]);

    let error = session
        .record_summary_draft_judgment(request, outcome)
        .expect_err("missing summary draft evidence should reject before registry write");

    assert!(matches!(
        error,
        JudgmentError::UnreadableEvidence {
            artifact_id,
            source: ArtifactError::MissingArtifact { .. },
        } if artifact_id.as_str() == "missing-judgment-artifact"
    ));
    assert!(session.judgment_records().is_empty());
}

#[test]
fn summary_draft_judgment_rejects_bad_evidence_locator_without_registry_record() {
    let mut session = SessionState::new(session_id());
    session
        .record_artifact_state(
            ArtifactRef::new(artifact_id("short-judgment-artifact"), ArtifactKind::Text),
            ArtifactContent::text("one line\n"),
        )
        .expect("artifact records");
    let request = summary_draft_request(vec![judgment_evidence(
        "bad line",
        "short-judgment-artifact",
        EvidenceLocator::line_range(4, 4).expect("valid locator shape"),
    )]);
    let outcome = summary_draft_outcome(vec![judgment_evidence(
        "whole source",
        "short-judgment-artifact",
        EvidenceLocator::whole_artifact(),
    )]);

    let error = session
        .record_summary_draft_judgment(request, outcome)
        .expect_err("bad summary draft evidence locator should reject before registry write");

    assert!(matches!(
        error,
        JudgmentError::UnreadableEvidence {
            artifact_id,
            source: ArtifactError::InvalidEvidenceLocator { .. },
        } if artifact_id.as_str() == "short-judgment-artifact"
    ));
    assert!(session.judgment_records().is_empty());
}

#[test]
fn summary_draft_judgment_success_is_readable_from_internal_registry() {
    let mut session = SessionState::new(session_id());
    session
        .record_artifact_state(
            ArtifactRef::new(artifact_id("judgment-source"), ArtifactKind::Text),
            ArtifactContent::text("first line\nsecond line\n"),
        )
        .expect("artifact records");
    let request = summary_draft_request(vec![judgment_evidence(
        "selected line",
        "judgment-source",
        EvidenceLocator::line_range(1, 1).expect("valid line locator"),
    )]);
    let outcome = summary_draft_outcome(vec![judgment_evidence(
        "whole source",
        "judgment-source",
        EvidenceLocator::whole_artifact(),
    )]);

    let record = session
        .record_summary_draft_judgment(request, outcome)
        .expect("readable summary draft evidence should record");
    let records = session.judgment_records();

    assert_eq!(records, vec![record.clone()]);
    assert_eq!(record.id().as_str(), "judgment-record-00000000000000000000");
    assert_eq!(record.request().purpose(), JudgmentPurpose::SummaryDraft);
    assert_eq!(record.outcome().purpose(), JudgmentPurpose::SummaryDraft);
    assert!(
        record
            .artifacts()
            .request()
            .content()
            .contains("artifact=request\n")
    );
    assert!(
        record
            .artifacts()
            .outcome()
            .content()
            .contains("artifact=outcome\n")
    );
    assert!(
        record
            .artifacts()
            .request()
            .content()
            .contains("evidence.0.locator=line:1-1\n")
    );
}

#[test]
fn summary_draft_judgment_does_not_enter_context_ledger_events_or_tools() {
    let mut session = SessionState::new(session_id());
    let started = session
        .record_session_started_if_needed()
        .expect("session should start");
    session
        .record_artifact_state(
            ArtifactRef::new(artifact_id("summary-audit-source"), ArtifactKind::Text),
            ArtifactContent::text("source text for advisory draft\n"),
        )
        .expect("artifact records");
    let request = summary_draft_request(vec![judgment_evidence(
        "summary source",
        "summary-audit-source",
        EvidenceLocator::whole_artifact(),
    )]);
    let draft = "Internal advisory summary draft that must not enter context.";
    let outcome = summary_draft_outcome_with_draft(
        vec![judgment_evidence(
            "summary source",
            "summary-audit-source",
            EvidenceLocator::whole_artifact(),
        )],
        draft,
    );
    let projection_before = session.ledger_projection();
    let next_sequence_before = session.next_sequence();
    let pending_tools_before = session.pending_tool_calls();
    let compiled_before = ContextCompiler::new()
        .compile(&session.context_snapshot())
        .expect("empty context compiles before judgment");

    let record = session
        .record_summary_draft_judgment(request, outcome)
        .expect("summary draft judgment records internally");
    let compiled_after = ContextCompiler::new()
        .compile(&session.context_snapshot())
        .expect("empty context compiles after judgment");
    let completed = session.record_step_completed();

    assert_eq!(session.judgment_records(), vec![record]);
    assert!(compiled_after.sections().is_empty());
    assert_eq!(compiled_after.to_snapshot(), "");
    assert_eq!(compiled_after, compiled_before);
    assert!(!compiled_after.to_snapshot().contains(draft));
    assert_eq!(session.pending_tool_calls(), pending_tools_before);
    assert_eq!(session.ledger_projection().entries().len(), 2);
    assert_eq!(
        session.ledger_projection().entries()[0],
        projection_before.entries()[0]
    );
    assert_eq!(next_sequence_before, 1);
    assert_eq!(started.sequence, 0);
    assert_eq!(completed.sequence, 1);
}

#[test]
fn summary_draft_judgment_rejects_non_summary_draft_request_without_registry_record() {
    let mut session = SessionState::new(session_id());
    let outcome = JudgmentOutcome::new(
        JudgmentPurpose::MemoryRelevance,
        JudgmentRecommendation::NoRecommendation,
        judgment_confidence(0.1),
        Vec::new(),
        "No summary draft was produced.",
        "Only the helper boundary was exercised.",
        judgment_provenance(),
    )
    .expect("valid no recommendation outcome");

    let error = session
        .record_summary_draft_judgment(memory_relevance_request(Vec::new()), outcome)
        .expect_err("non-summary request is rejected by the narrow helper");

    assert_eq!(
        error,
        JudgmentError::SummaryDraftPurposeRequired {
            field: "judgment request",
            actual_purpose: JudgmentPurpose::MemoryRelevance,
        }
    );
    assert!(session.judgment_records().is_empty());
}

#[test]
fn summary_draft_judgment_rejects_non_summary_draft_outcome_without_registry_record() {
    let mut session = SessionState::new(session_id());
    session
        .record_artifact_state(
            ArtifactRef::new(artifact_id("summary-outcome-source"), ArtifactKind::Text),
            ArtifactContent::text("source text for advisory draft\n"),
        )
        .expect("artifact records");
    let request = summary_draft_request(vec![judgment_evidence(
        "summary source",
        "summary-outcome-source",
        EvidenceLocator::whole_artifact(),
    )]);

    let error = session
        .record_summary_draft_judgment(request, high_tool_risk_outcome())
        .expect_err("non-summary outcome is rejected by the narrow helper");

    assert_eq!(
        error,
        JudgmentError::SummaryDraftPurposeRequired {
            field: "judgment outcome",
            actual_purpose: JudgmentPurpose::ToolRiskReview,
        }
    );
    assert!(session.judgment_records().is_empty());
}

#[test]
fn accepted_summary_draft_promotion_writes_one_compiled_context_summary_only() {
    let mut session = SessionState::new(session_id());
    session
        .record_artifact_state(
            ArtifactRef::new(artifact_id("promotion-source"), ArtifactKind::Text),
            ArtifactContent::text("first source line\nsecond source line\n"),
        )
        .expect("artifact records");
    let started = session
        .record_session_started_if_needed()
        .expect("session starts");
    let pending = pending_tool_call("promotion-pending-call");
    let pending_event = session
        .record_test_tool_call_pending(pending.clone())
        .expect("pending tool call records");
    let evidence = judgment_evidence(
        "selected promotion line",
        "promotion-source",
        EvidenceLocator::line_range(1, 1).expect("valid line locator"),
    );
    let request = summary_draft_request(vec![evidence.clone()]);
    let outcome = summary_draft_outcome_with_draft(
        vec![judgment_evidence(
            "whole promotion source",
            "promotion-source",
            EvidenceLocator::whole_artifact(),
        )],
        "Accepted summary draft.",
    );
    let projection_before = session.ledger_projection();
    let next_sequence_before = session.next_sequence();
    let pending_tools_before = session.pending_tool_calls();

    session
        .promote_summary_draft_to_context(
            &request,
            &outcome,
            promotion_input(
                "accepted-summary",
                "Accepted summary draft.",
                vec![evidence],
            ),
        )
        .expect("accepted summary draft promotes");

    let compiled = ContextCompiler::new()
        .compile(&session.context_snapshot())
        .expect("promoted context compiles");
    assert_eq!(compiled.sections().len(), 1);
    assert_eq!(
        compiled.to_snapshot(),
        [
            "summary:accepted-summary",
            "text:Accepted summary draft.",
            "evidence:selected promotion line:promotion-source:line:1-1",
        ]
        .join("\n")
    );
    assert_eq!(session.ledger_projection(), projection_before);
    assert_eq!(session.next_sequence(), next_sequence_before);
    assert_eq!(session.pending_tool_calls(), pending_tools_before);
    assert_eq!(session.pending_tool_calls(), vec![pending]);
    assert_eq!(started.sequence, 0);
    assert_eq!(pending_event.sequence, 1);
    assert!(session.judgment_records().is_empty());
    assert_single_promotion_record(
        &session,
        "accepted-summary",
        SummaryDraftPromotionState::Promoted,
        None,
    );
}

#[test]
fn checked_context_entry_rejects_invalid_candidate_without_context_mutation() {
    let mut session = SessionState::new(session_id());
    session
        .record_artifact_state(
            ArtifactRef::new(artifact_id("checked-existing-source"), ArtifactKind::Text),
            ArtifactContent::text("existing source text\n"),
        )
        .expect("existing artifact records");
    session
        .record_artifact_state(
            ArtifactRef::new(artifact_id("checked-short-source"), ArtifactKind::Text),
            ArtifactContent::text("one line\n"),
        )
        .expect("short artifact records");
    session
        .record_context_entry(ContextEntry::summary(
            ContextSummary::new(
                "checked-existing-summary",
                "Existing checked summary.",
                vec![
                    ContextEvidence::new(
                        "existing source",
                        EvidenceRef::new(
                            artifact_id("checked-existing-source"),
                            EvidenceLocator::whole_artifact(),
                        ),
                    )
                    .expect("valid context evidence"),
                ],
            )
            .expect("valid context summary"),
        ))
        .expect("existing context records");
    let context_before = ContextCompiler::new()
        .compile(&session.context_snapshot())
        .expect("existing context compiles")
        .to_snapshot();

    let error = session
        .record_checked_context_entry(ContextEntry::summary(
            ContextSummary::new(
                "checked-invalid-summary",
                "Invalid checked summary.",
                vec![
                    ContextEvidence::new(
                        "invalid source",
                        EvidenceRef::new(
                            artifact_id("checked-short-source"),
                            EvidenceLocator::line_range(2, 2).expect("valid locator shape"),
                        ),
                    )
                    .expect("valid context evidence"),
                ],
            )
            .expect("valid context summary"),
        ))
        .expect_err("checked append rejects unreadable evidence");

    assert!(matches!(
        error,
        ContextError::UnreadableEvidence {
            summary_id,
            artifact_id,
            source: ArtifactError::InvalidEvidenceLocator { .. },
        } if summary_id == "checked-invalid-summary"
            && artifact_id.as_str() == "checked-short-source"
    ));
    assert_eq!(
        ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("existing context still compiles")
            .to_snapshot(),
        context_before
    );
}

#[test]
fn summary_draft_promotion_exact_replay_after_promoted_is_idempotent_without_context_change() {
    let mut session = SessionState::new(session_id());
    session
        .record_artifact_state(
            ArtifactRef::new(
                artifact_id("duplicate-promotion-source"),
                ArtifactKind::Text,
            ),
            ArtifactContent::text("duplicate source text\n"),
        )
        .expect("artifact records");
    let source_record_id =
        JudgmentRecordId::new("source-summary-draft-record").expect("valid judgment record id");
    let evidence = judgment_evidence(
        "duplicate source",
        "duplicate-promotion-source",
        EvidenceLocator::whole_artifact(),
    );
    let request = summary_draft_request(vec![evidence.clone()]);
    let outcome =
        summary_draft_outcome_with_draft(vec![evidence.clone()], "Duplicate summary draft.");
    session
        .promote_summary_draft_to_context(
            &request,
            &outcome,
            promotion_input(
                "duplicate-summary",
                "Duplicate summary draft.",
                vec![evidence.clone()],
            ),
        )
        .expect("first promotion succeeds");
    assert_single_promotion_record(
        &session,
        "duplicate-summary",
        SummaryDraftPromotionState::Promoted,
        None,
    );
    let context_before = ContextCompiler::new()
        .compile(&session.context_snapshot())
        .expect("context compiles after first promotion");
    let projection_before = session.ledger_projection();
    let next_sequence_before = session.next_sequence();
    let pending_before = session.pending_tool_calls();

    session
        .promote_summary_draft_to_context(
            &request,
            &outcome,
            promotion_input(
                "duplicate-summary",
                "Duplicate summary draft.",
                vec![evidence],
            ),
        )
        .expect("exact promoted replay is idempotent");

    let context_after = ContextCompiler::new()
        .compile(&session.context_snapshot())
        .expect("context still compiles after exact replay");
    assert_eq!(context_after, context_before);
    assert_eq!(context_after.sections().len(), 1);
    assert_eq!(
        context_after.to_snapshot(),
        [
            "summary:duplicate-summary",
            "text:Duplicate summary draft.",
            "evidence:duplicate source:duplicate-promotion-source:whole",
        ]
        .join("\n")
    );
    assert_eq!(session.ledger_projection(), projection_before);
    assert_eq!(session.next_sequence(), next_sequence_before);
    assert_eq!(session.pending_tool_calls(), pending_before);
    assert_single_promotion_record(
        &session,
        "duplicate-summary",
        SummaryDraftPromotionState::Promoted,
        None,
    );

    let conflict = session
        .promote_summary_draft_to_context(
            &request,
            &outcome,
            promotion_input_with_source_record_id(
                "duplicate-summary",
                "Duplicate summary draft.",
                vec![judgment_evidence(
                    "duplicate source",
                    "duplicate-promotion-source",
                    EvidenceLocator::whole_artifact(),
                )],
                Some(source_record_id),
            ),
        )
        .expect_err("same summary id with different source record conflicts");

    assert_eq!(
        conflict,
        SummaryDraftPromotionError::PromotionPayloadConflict {
            summary_id: "duplicate-summary".to_owned(),
        }
    );
    assert_eq!(
        ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("context still compiles")
            .to_snapshot(),
        context_before.to_snapshot()
    );
    assert_eq!(session.ledger_projection(), projection_before);
    assert_eq!(session.next_sequence(), next_sequence_before);
    assert_eq!(session.pending_tool_calls(), pending_before);
    assert_single_promotion_record(
        &session,
        "duplicate-summary",
        SummaryDraftPromotionState::Promoted,
        None,
    );
}

#[test]
fn summary_draft_promotion_pre_existing_context_duplicate_does_not_write_registry() {
    let mut session = SessionState::new(session_id());
    session
        .record_artifact_state(
            ArtifactRef::new(
                artifact_id("pre-existing-summary-source"),
                ArtifactKind::Text,
            ),
            ArtifactContent::text("pre-existing source text\n"),
        )
        .expect("artifact records");
    let evidence = judgment_evidence(
        "pre-existing source",
        "pre-existing-summary-source",
        EvidenceLocator::whole_artifact(),
    );
    session
        .record_context_entry(crate::context::ContextEntry::summary(
            crate::context::ContextSummary::new(
                "pre-existing-summary",
                "Already recorded summary.",
                vec![
                    crate::context::ContextEvidence::new(
                        "pre-existing source",
                        EvidenceRef::new(
                            artifact_id("pre-existing-summary-source"),
                            EvidenceLocator::whole_artifact(),
                        ),
                    )
                    .expect("valid context evidence"),
                ],
            )
            .expect("valid context summary"),
        ))
        .expect("pre-existing context records");
    let context_before = ContextCompiler::new()
        .compile(&session.context_snapshot())
        .expect("pre-existing context compiles");
    let projection_before = session.ledger_projection();
    let next_sequence_before = session.next_sequence();
    let pending_before = session.pending_tool_calls();
    let request = summary_draft_request(vec![evidence.clone()]);
    let outcome = summary_draft_outcome_with_draft(vec![evidence.clone()], "New duplicate draft.");

    let error = session
        .promote_summary_draft_to_context(
            &request,
            &outcome,
            promotion_input(
                "pre-existing-summary",
                "New duplicate draft.",
                vec![evidence],
            ),
        )
        .expect_err("external context summary id duplicate is rejected");

    assert_eq!(
        error,
        SummaryDraftPromotionError::DuplicateSummaryId {
            summary_id: "pre-existing-summary".to_owned(),
        }
    );
    assert_eq!(
        ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("context still compiles after duplicate rejection"),
        context_before
    );
    assert!(
        session
            .summary_draft_promotion_snapshot()
            .records()
            .is_empty()
    );
    assert_eq!(session.ledger_projection(), projection_before);
    assert_eq!(session.next_sequence(), next_sequence_before);
    assert_eq!(session.pending_tool_calls(), pending_before);
}

#[test]
fn summary_draft_promotion_same_summary_id_different_draft_conflicts_without_context_change() {
    let mut session = SessionState::new(session_id());
    session
        .record_artifact_state(
            ArtifactRef::new(artifact_id("draft-conflict-source"), ArtifactKind::Text),
            ArtifactContent::text("draft conflict source text\n"),
        )
        .expect("artifact records");
    let first_evidence = judgment_evidence(
        "draft conflict source",
        "draft-conflict-source",
        EvidenceLocator::whole_artifact(),
    );
    let first_request = summary_draft_request(vec![first_evidence.clone()]);
    let first_outcome =
        summary_draft_outcome_with_draft(vec![first_evidence.clone()], "Original draft.");
    session
        .promote_summary_draft_to_context(
            &first_request,
            &first_outcome,
            promotion_input(
                "conflicting-summary",
                "Original draft.",
                vec![first_evidence],
            ),
        )
        .expect("first promotion succeeds");
    let context_before = ContextCompiler::new()
        .compile(&session.context_snapshot())
        .expect("context compiles after first promotion");
    let projection_before = session.ledger_projection();
    let next_sequence_before = session.next_sequence();
    let pending_before = session.pending_tool_calls();
    let conflict_evidence = judgment_evidence(
        "draft conflict source",
        "draft-conflict-source",
        EvidenceLocator::whole_artifact(),
    );
    let conflict_request = summary_draft_request(vec![conflict_evidence.clone()]);
    let conflict_outcome =
        summary_draft_outcome_with_draft(vec![conflict_evidence.clone()], "Changed draft.");

    let error = session
        .promote_summary_draft_to_context(
            &conflict_request,
            &conflict_outcome,
            promotion_input(
                "conflicting-summary",
                "Changed draft.",
                vec![conflict_evidence],
            ),
        )
        .expect_err("same summary id different draft conflicts");

    assert_eq!(
        error,
        SummaryDraftPromotionError::PromotionPayloadConflict {
            summary_id: "conflicting-summary".to_owned(),
        }
    );
    assert_eq!(
        ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("context still compiles after draft conflict"),
        context_before
    );
    assert_eq!(session.ledger_projection(), projection_before);
    assert_eq!(session.next_sequence(), next_sequence_before);
    assert_eq!(session.pending_tool_calls(), pending_before);
    assert_single_promotion_record(
        &session,
        "conflicting-summary",
        SummaryDraftPromotionState::Promoted,
        None,
    );
}

#[test]
fn summary_draft_promotion_same_summary_id_different_evidence_conflicts_without_context_change() {
    let mut session = SessionState::new(session_id());
    session
        .record_artifact_state(
            ArtifactRef::new(artifact_id("evidence-conflict-source"), ArtifactKind::Text),
            ArtifactContent::text("first line\nsecond line\n"),
        )
        .expect("artifact records");
    let first_evidence = judgment_evidence(
        "first evidence line",
        "evidence-conflict-source",
        EvidenceLocator::line_range(1, 1).expect("valid line locator"),
    );
    let first_request = summary_draft_request(vec![first_evidence.clone()]);
    let first_outcome =
        summary_draft_outcome_with_draft(vec![first_evidence.clone()], "Evidence conflict draft.");
    session
        .promote_summary_draft_to_context(
            &first_request,
            &first_outcome,
            promotion_input(
                "evidence-conflicting-summary",
                "Evidence conflict draft.",
                vec![first_evidence],
            ),
        )
        .expect("first promotion succeeds");
    let context_before = ContextCompiler::new()
        .compile(&session.context_snapshot())
        .expect("context compiles after first promotion");
    let projection_before = session.ledger_projection();
    let next_sequence_before = session.next_sequence();
    let pending_before = session.pending_tool_calls();
    let conflict_evidence = judgment_evidence(
        "second evidence line",
        "evidence-conflict-source",
        EvidenceLocator::line_range(2, 2).expect("valid line locator"),
    );
    let conflict_request = summary_draft_request(vec![conflict_evidence.clone()]);
    let conflict_outcome = summary_draft_outcome_with_draft(
        vec![conflict_evidence.clone()],
        "Evidence conflict draft.",
    );

    let error = session
        .promote_summary_draft_to_context(
            &conflict_request,
            &conflict_outcome,
            promotion_input(
                "evidence-conflicting-summary",
                "Evidence conflict draft.",
                vec![conflict_evidence],
            ),
        )
        .expect_err("same summary id different evidence conflicts");

    assert_eq!(
        error,
        SummaryDraftPromotionError::PromotionPayloadConflict {
            summary_id: "evidence-conflicting-summary".to_owned(),
        }
    );
    assert_eq!(
        ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("context still compiles after evidence conflict"),
        context_before
    );
    assert_eq!(session.ledger_projection(), projection_before);
    assert_eq!(session.next_sequence(), next_sequence_before);
    assert_eq!(session.pending_tool_calls(), pending_before);
    assert_single_promotion_record(
        &session,
        "evidence-conflicting-summary",
        SummaryDraftPromotionState::Promoted,
        None,
    );
}

#[test]
fn summary_draft_promotion_compile_failure_rejects_record_and_exact_replay() {
    let mut session = SessionState::new(session_id());
    session
        .record_artifact_state(
            ArtifactRef::new(artifact_id("short-promotion-source"), ArtifactKind::Text),
            ArtifactContent::text("one line\n"),
        )
        .expect("artifact records");
    let bad_evidence = judgment_evidence(
        "bad promotion line",
        "short-promotion-source",
        EvidenceLocator::line_range(3, 3).expect("valid locator shape"),
    );
    let request = summary_draft_request(vec![bad_evidence.clone()]);
    let outcome =
        summary_draft_outcome_with_draft(vec![bad_evidence.clone()], "Bad summary draft.");
    let input = promotion_input("bad-summary", "Bad summary draft.", vec![bad_evidence]);
    let context_before = ContextCompiler::new()
        .compile(&session.context_snapshot())
        .expect("empty context compiles before failed promotion")
        .to_snapshot();
    let projection_before = session.ledger_projection();
    let next_sequence_before = session.next_sequence();
    let pending_before = session.pending_tool_calls();

    let error = session
        .promote_summary_draft_to_context(&request, &outcome, input.clone())
        .expect_err("compile validation rejects unreadable evidence");

    assert!(matches!(
        error,
        SummaryDraftPromotionError::Context {
            source: ContextError::UnreadableEvidence {
                summary_id,
                artifact_id,
                source: ArtifactError::InvalidEvidenceLocator { .. },
            },
        } if summary_id == "bad-summary" && artifact_id.as_str() == "short-promotion-source"
    ));
    assert_eq!(
        ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("context still compiles after failed promotion")
            .to_snapshot(),
        context_before
    );
    assert_eq!(session.ledger_projection(), projection_before);
    assert_eq!(session.next_sequence(), next_sequence_before);
    assert_eq!(session.pending_tool_calls(), pending_before);
    assert_single_promotion_record(
        &session,
        "bad-summary",
        SummaryDraftPromotionState::Rejected,
        None,
    );

    let replay_error = session
        .promote_summary_draft_to_context(&request, &outcome, input)
        .expect_err("exact rejected replay stays rejected");

    assert_eq!(
        replay_error,
        SummaryDraftPromotionError::PromotionAlreadyRejected {
            summary_id: "bad-summary".to_owned(),
        }
    );
    assert_eq!(
        ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("context still compiles after rejected replay")
            .to_snapshot(),
        context_before
    );
    assert_eq!(session.ledger_projection(), projection_before);
    assert_eq!(session.next_sequence(), next_sequence_before);
    assert_eq!(session.pending_tool_calls(), pending_before);
    assert_single_promotion_record(
        &session,
        "bad-summary",
        SummaryDraftPromotionState::Rejected,
        None,
    );
}

#[test]
fn high_tool_risk_review_does_not_mutate_pending_tool_or_context_state() {
    let mut session = SessionState::new(session_id());
    let call = pending_tool_call("risky-call");
    session
        .record_test_tool_call_pending(call.clone())
        .expect("pending tool call records");
    let pending_before = session.pending_tool_calls();
    let projection_before = session.ledger_projection();
    let context_before = ContextCompiler::new()
        .compile(&session.context_snapshot())
        .expect("empty context compiles")
        .to_snapshot();

    session
        .record_judgment(high_tool_risk_request(), high_tool_risk_outcome())
        .expect("high tool risk review records internally");

    assert_eq!(session.pending_tool_calls(), pending_before);
    assert_eq!(session.ledger_projection(), projection_before);
    let context_after = ContextCompiler::new()
        .compile(&session.context_snapshot())
        .expect("empty context compiles after judgment")
        .to_snapshot();
    assert_eq!(context_after, context_before);
    assert_eq!(session.judgment_records().len(), 1);
}
