use crate::judgment::{
    JudgmentConfidence, JudgmentError, JudgmentEvidence, JudgmentOutcome, JudgmentProvenance,
    JudgmentPurpose, JudgmentRecommendation, JudgmentRequest, JudgmentRiskLevel,
    JudgmentSourceKind,
    tests::{confidence, constraints, evidence, evidence_ref, provenance},
};

#[test]
fn confidence_rejects_nan_infinity_and_out_of_range_values() {
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.1, 1.1] {
        assert!(matches!(
            JudgmentConfidence::new(value),
            Err(JudgmentError::InvalidConfidence { .. })
        ));
    }

    assert_eq!(
        JudgmentConfidence::new(1.0)
            .expect("confidence is valid")
            .as_f32(),
        1.0
    );
}

#[test]
fn validation_rejects_blank_labels_subject_rationale_and_provenance() {
    assert!(matches!(
        JudgmentEvidence::new(" ", evidence_ref("blank-label")),
        Err(JudgmentError::BlankField {
            field: "judgment evidence label"
        })
    ));

    assert!(matches!(
        JudgmentRequest::new(
            JudgmentPurpose::MemoryRelevance,
            " ",
            "memory candidate",
            Vec::new(),
            constraints(),
            "test request",
        ),
        Err(JudgmentError::BlankField {
            field: "judgment request subject"
        })
    ));

    assert!(matches!(
        JudgmentOutcome::new(
            JudgmentPurpose::MemoryRelevance,
            JudgmentRecommendation::MemoryRelevant,
            confidence(0.5),
            Vec::new(),
            " ",
            "low uncertainty",
            provenance(JudgmentSourceKind::Test),
        ),
        Err(JudgmentError::BlankField {
            field: "judgment outcome rationale"
        })
    ));

    assert!(matches!(
        JudgmentProvenance::new(JudgmentSourceKind::Human, " "),
        Err(JudgmentError::BlankField {
            field: "judgment provenance source label"
        })
    ));
}

#[test]
fn request_rejects_blank_input_source_label_and_constraints() {
    assert!(matches!(
        JudgmentRequest::new(
            JudgmentPurpose::MemoryRelevance,
            "memory candidate",
            " ",
            Vec::new(),
            constraints(),
            "test request",
        ),
        Err(JudgmentError::BlankField {
            field: "judgment request input"
        })
    ));

    assert!(matches!(
        JudgmentRequest::new(
            JudgmentPurpose::MemoryRelevance,
            "memory candidate",
            "input",
            Vec::new(),
            Vec::new(),
            "test request",
        ),
        Err(JudgmentError::EmptyConstraints)
    ));

    assert!(matches!(
        JudgmentRequest::new(
            JudgmentPurpose::MemoryRelevance,
            "memory candidate",
            "input",
            Vec::new(),
            vec![" ".to_owned()],
            "test request",
        ),
        Err(JudgmentError::BlankField {
            field: "judgment request constraint"
        })
    ));

    assert!(matches!(
        JudgmentRequest::new(
            JudgmentPurpose::MemoryRelevance,
            "memory candidate",
            "input",
            Vec::new(),
            constraints(),
            " ",
        ),
        Err(JudgmentError::BlankField {
            field: "judgment request source label"
        })
    ));
}

#[test]
fn summary_draft_request_and_outcome_require_exact_evidence() {
    assert!(matches!(
        JudgmentRequest::new(
            JudgmentPurpose::SummaryDraft,
            "session summary",
            "draft a compact summary",
            Vec::new(),
            constraints(),
            "test request",
        ),
        Err(JudgmentError::MissingEvidence {
            purpose: JudgmentPurpose::SummaryDraft,
            field: "judgment request evidence",
        })
    ));

    assert!(matches!(
        JudgmentOutcome::new(
            JudgmentPurpose::SummaryDraft,
            JudgmentRecommendation::SummaryDraft {
                draft: "summary text".to_owned(),
            },
            confidence(0.7),
            Vec::new(),
            "The draft is grounded in supplied evidence.",
            "Evidence coverage is partial.",
            provenance(JudgmentSourceKind::Test),
        ),
        Err(JudgmentError::MissingEvidence {
            purpose: JudgmentPurpose::SummaryDraft,
            field: "judgment outcome evidence",
        })
    ));

    let request = JudgmentRequest::new(
        JudgmentPurpose::SummaryDraft,
        "session summary",
        "draft a compact summary",
        vec![evidence("source", "summary-source")],
        constraints(),
        "test request",
    )
    .expect("summary draft request with evidence is valid");
    assert_eq!(request.evidence()[0].label(), "source");
    assert!(
        request.evidence()[0]
            .reference()
            .locator
            .is_whole_artifact()
    );
    assert_eq!(request.subject(), "session summary");
    assert_eq!(request.input(), "draft a compact summary");
    assert_eq!(request.constraints(), &["advisory semantic signal only"]);
    assert_eq!(request.source_label(), "test request");
}

#[test]
fn outcome_validates_recommendation_shape_and_purpose() {
    assert!(matches!(
        JudgmentOutcome::new(
            JudgmentPurpose::MemoryRelevance,
            JudgmentRecommendation::SummaryDraft {
                draft: "summary text".to_owned(),
            },
            confidence(0.5),
            vec![evidence("source", "shape-source")],
            "Rationale is present.",
            "Uncertainty is present.",
            provenance(JudgmentSourceKind::Test),
        ),
        Err(JudgmentError::RecommendationPurposeMismatch {
            purpose: JudgmentPurpose::MemoryRelevance,
            recommendation: "summary draft",
        })
    ));

    assert!(matches!(
        JudgmentOutcome::new(
            JudgmentPurpose::SummaryDraft,
            JudgmentRecommendation::SummaryDraft {
                draft: " ".to_owned(),
            },
            confidence(0.5),
            vec![evidence("source", "blank-draft-source")],
            "Rationale is present.",
            "Uncertainty is present.",
            provenance(JudgmentSourceKind::Test),
        ),
        Err(JudgmentError::BlankField {
            field: "judgment summary draft"
        })
    ));

    assert!(matches!(
        JudgmentOutcome::new(
            JudgmentPurpose::ToolRiskReview,
            JudgmentRecommendation::ToolRiskReview {
                risk: JudgmentRiskLevel::Medium,
                concerns: vec![" ".to_owned()],
            },
            confidence(0.5),
            Vec::new(),
            "Rationale is present.",
            "Uncertainty is present.",
            provenance(JudgmentSourceKind::Test),
        ),
        Err(JudgmentError::BlankField {
            field: "judgment tool risk concern"
        })
    ));

    let outcome = JudgmentOutcome::new(
        JudgmentPurpose::MemoryRelevance,
        JudgmentRecommendation::MemoryNotRelevant,
        confidence(0.5),
        Vec::new(),
        "The memory does not match the request.",
        "The source only reviewed the supplied subject and input.",
        provenance(JudgmentSourceKind::Test),
    )
    .expect("memory not relevant outcome is valid");

    assert_eq!(
        outcome.recommendation(),
        &JudgmentRecommendation::MemoryNotRelevant
    );
}

#[test]
fn source_kinds_and_risk_levels_cover_required_internal_cases() {
    assert_eq!(
        [
            JudgmentSourceKind::Deterministic,
            JudgmentSourceKind::Llm,
            JudgmentSourceKind::Human,
            JudgmentSourceKind::Test,
        ]
        .len(),
        4
    );

    assert_eq!(
        [
            JudgmentRiskLevel::Low,
            JudgmentRiskLevel::Medium,
            JudgmentRiskLevel::High,
            JudgmentRiskLevel::Unknown,
        ]
        .len(),
        4
    );
}
