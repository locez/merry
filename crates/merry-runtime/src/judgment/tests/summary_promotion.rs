use crate::judgment::{
    JudgmentOutcome, JudgmentPurpose, JudgmentRecommendation, JudgmentRecordId, JudgmentSourceKind,
    SummaryDraftAcceptance, SummaryDraftAcceptanceAuthority, SummaryDraftPromotionError,
    SummaryDraftPromotionInput, context_summary_from_accepted_summary_draft,
    tests::{
        acceptance, artifact_id, confidence, evidence, high_tool_risk_outcome,
        memory_relevance_request, promotion_input, provenance, summary_draft_outcome,
        summary_draft_request,
    },
};
use merry_core::{EvidenceLocator, EvidenceRef};

#[test]
fn summary_draft_acceptance_authority_has_no_llm_route() {
    fn authority_name(authority: SummaryDraftAcceptanceAuthority) -> &'static str {
        match authority {
            SummaryDraftAcceptanceAuthority::HardPolicy => "hard_policy",
            SummaryDraftAcceptanceAuthority::Human => "human",
            SummaryDraftAcceptanceAuthority::DeterministicReview => "deterministic_review",
        }
    }

    let authorities = [
        SummaryDraftAcceptanceAuthority::HardPolicy,
        SummaryDraftAcceptanceAuthority::Human,
        SummaryDraftAcceptanceAuthority::DeterministicReview,
    ];
    assert_eq!(
        authorities
            .iter()
            .copied()
            .map(authority_name)
            .collect::<Vec<_>>(),
        vec!["hard_policy", "human", "deterministic_review"]
    );

    let acceptance = SummaryDraftAcceptance::new(
        SummaryDraftAcceptanceAuthority::DeterministicReview,
        " deterministic review ",
        "Review accepted the draft for context promotion.",
    )
    .expect("explicit deterministic acceptance is valid");
    assert_eq!(
        acceptance.authority(),
        SummaryDraftAcceptanceAuthority::DeterministicReview
    );
    assert_eq!(acceptance.source_label(), "deterministic review");
    assert_eq!(
        acceptance.rationale(),
        "Review accepted the draft for context promotion."
    );
}

#[test]
fn summary_draft_acceptance_and_input_reject_blank_or_empty_fields() {
    assert_eq!(
        SummaryDraftAcceptance::new(
            SummaryDraftAcceptanceAuthority::Human,
            " ",
            "Human accepted the draft.",
        )
        .expect_err("blank acceptance source label rejects"),
        SummaryDraftPromotionError::BlankField {
            field: "summary draft acceptance source label"
        }
    );
    assert_eq!(
        SummaryDraftAcceptance::new(SummaryDraftAcceptanceAuthority::Human, "reviewer", " ",)
            .expect_err("blank acceptance rationale rejects"),
        SummaryDraftPromotionError::BlankField {
            field: "summary draft acceptance rationale"
        }
    );

    let acceptance = acceptance();
    assert_eq!(
        SummaryDraftPromotionInput::new(
            " ",
            "Summary draft from exact evidence.",
            vec![evidence("source", "summary-source")],
            acceptance.clone(),
            None,
        )
        .expect_err("blank summary id rejects"),
        SummaryDraftPromotionError::BlankField {
            field: "summary draft promotion summary id"
        }
    );
    assert_eq!(
        SummaryDraftPromotionInput::new(
            "summary-id",
            " ",
            vec![evidence("source", "summary-source")],
            acceptance.clone(),
            None,
        )
        .expect_err("blank draft text rejects"),
        SummaryDraftPromotionError::BlankField {
            field: "summary draft promotion draft text"
        }
    );
    assert_eq!(
        SummaryDraftPromotionInput::new(
            "summary-id",
            "Summary draft from exact evidence.",
            Vec::new(),
            acceptance,
            None,
        )
        .expect_err("empty selected evidence rejects"),
        SummaryDraftPromotionError::EmptySelectedEvidence
    );
}

#[test]
fn accepted_summary_draft_promotes_to_context_summary_with_selected_evidence() {
    let request = summary_draft_request();
    let outcome = summary_draft_outcome();
    let input = SummaryDraftPromotionInput::new(
        "accepted-summary",
        "Summary draft from exact evidence.",
        vec![evidence("source", "summary-source")],
        acceptance(),
        Some(JudgmentRecordId::new("audit-record").expect("valid audit record id")),
    )
    .expect("valid promotion input");

    let summary = context_summary_from_accepted_summary_draft(&request, &outcome, &input)
        .expect("accepted summary draft promotes to context summary");

    assert_eq!(summary.id(), "accepted-summary");
    assert_eq!(summary.text(), "Summary draft from exact evidence.");
    assert_eq!(summary.evidence().len(), 1);
    assert_eq!(summary.evidence()[0].label(), "source");
    assert_eq!(
        summary.evidence()[0].reference(),
        &EvidenceRef::new(
            artifact_id("summary-source"),
            EvidenceLocator::whole_artifact()
        )
    );
}

#[test]
fn summary_draft_promotion_rejects_non_summary_draft_request() {
    let error = context_summary_from_accepted_summary_draft(
        &memory_relevance_request(),
        &summary_draft_outcome(),
        &promotion_input("accepted-summary", "Summary draft from exact evidence."),
    )
    .expect_err("non-summary request rejects");

    assert_eq!(
        error,
        SummaryDraftPromotionError::SummaryDraftPurposeRequired {
            field: "judgment request",
            actual_purpose: JudgmentPurpose::MemoryRelevance,
        }
    );
}

#[test]
fn summary_draft_promotion_rejects_non_summary_draft_outcome() {
    let error = context_summary_from_accepted_summary_draft(
        &summary_draft_request(),
        &high_tool_risk_outcome(),
        &promotion_input("accepted-summary", "Summary draft from exact evidence."),
    )
    .expect_err("non-summary outcome rejects");

    assert_eq!(
        error,
        SummaryDraftPromotionError::SummaryDraftPurposeRequired {
            field: "judgment outcome",
            actual_purpose: JudgmentPurpose::ToolRiskReview,
        }
    );
}

#[test]
fn summary_draft_promotion_rejects_no_recommendation() {
    let request = summary_draft_request();
    let outcome = JudgmentOutcome::new(
        JudgmentPurpose::SummaryDraft,
        JudgmentRecommendation::NoRecommendation,
        confidence(0.0),
        Vec::new(),
        "No summary draft was produced.",
        "The advisory source produced no recommendation.",
        provenance(JudgmentSourceKind::Test),
    )
    .expect("summary draft no recommendation outcome is valid");

    let error = context_summary_from_accepted_summary_draft(
        &request,
        &outcome,
        &promotion_input("accepted-summary", "Summary draft from exact evidence."),
    )
    .expect_err("no recommendation rejects");

    assert_eq!(error, SummaryDraftPromotionError::NoRecommendation);
}

#[test]
fn summary_draft_promotion_rejects_draft_mismatch() {
    let error = context_summary_from_accepted_summary_draft(
        &summary_draft_request(),
        &summary_draft_outcome(),
        &promotion_input("accepted-summary", "Different summary text."),
    )
    .expect_err("draft mismatch rejects");

    assert_eq!(
        error,
        SummaryDraftPromotionError::DraftMismatch {
            recommended: "Summary draft from exact evidence.".to_owned(),
            accepted: "Different summary text.".to_owned(),
        }
    );
}

#[test]
fn summary_draft_promotion_rejects_selected_evidence_not_in_request_or_outcome() {
    let input = SummaryDraftPromotionInput::new(
        "accepted-summary",
        "Summary draft from exact evidence.",
        vec![evidence("external source", "external-source")],
        acceptance(),
        None,
    )
    .expect("input shape is valid before membership check");

    let error = context_summary_from_accepted_summary_draft(
        &summary_draft_request(),
        &summary_draft_outcome(),
        &input,
    )
    .expect_err("unrelated selected evidence rejects");

    assert_eq!(
        error,
        SummaryDraftPromotionError::SelectedEvidenceNotInJudgment {
            artifact_id: artifact_id("external-source"),
            locator: EvidenceLocator::whole_artifact(),
        }
    );
}

#[test]
fn summary_draft_promotion_rejects_selected_evidence_with_unmatched_label() {
    let input = SummaryDraftPromotionInput::new(
        "accepted-summary",
        "Summary draft from exact evidence.",
        vec![evidence("renamed source", "summary-source")],
        acceptance(),
        None,
    )
    .expect("input shape is valid before membership check");

    let error = context_summary_from_accepted_summary_draft(
        &summary_draft_request(),
        &summary_draft_outcome(),
        &input,
    )
    .expect_err("selected evidence with unmatched label rejects");

    assert_eq!(
        error,
        SummaryDraftPromotionError::SelectedEvidenceNotInJudgment {
            artifact_id: artifact_id("summary-source"),
            locator: EvidenceLocator::whole_artifact(),
        }
    );
}

#[test]
fn summary_draft_promotion_helper_defensively_rejects_empty_selected_evidence() {
    let input = SummaryDraftPromotionInput::new_unchecked_for_test(
        "accepted-summary",
        "Summary draft from exact evidence.",
        Vec::new(),
        acceptance(),
        None,
    );

    let error = context_summary_from_accepted_summary_draft(
        &summary_draft_request(),
        &summary_draft_outcome(),
        &input,
    )
    .expect_err("empty selected evidence rejects");

    assert_eq!(error, SummaryDraftPromotionError::EmptySelectedEvidence);
}
