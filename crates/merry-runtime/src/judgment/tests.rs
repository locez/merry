use super::*;
use merry_core::{ArtifactId, EvidenceLocator, EvidenceRef, ProviderName, ToolName};
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelMessageRole,
    ModelName, ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse,
    ModelStreamContext, ModelToolCall, ModelToolCallId, ProviderErrorKind, ToolArguments,
    testing::FakeModelProvider,
};
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

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
fn model_judgment_tool_risk_output_parses_each_risk_level() {
    for (value, expected) in [
        ("low", JudgmentRiskLevel::Low),
        ("medium", JudgmentRiskLevel::Medium),
        ("high", JudgmentRiskLevel::High),
        ("unknown", JudgmentRiskLevel::Unknown),
    ] {
        let request = tool_risk_request();
        let outcome = parse_tool_risk_review_model_judgment_output(
            &model_tool_risk_output(value, Vec::new()),
            &request,
            "test llm source",
        )
        .expect("valid tool risk model output parses");

        assert_eq!(outcome.purpose(), JudgmentPurpose::ToolRiskReview);
        assert_eq!(
            outcome.recommendation(),
            &JudgmentRecommendation::ToolRiskReview {
                risk: expected,
                concerns: vec!["The pending tool path may affect external state.".to_owned()],
            }
        );
        assert_eq!(outcome.confidence().as_f32(), 0.75);
        assert!(outcome.evidence().is_empty());
    }
}

#[test]
fn model_judgment_tool_risk_output_clones_request_evidence_and_builds_llm_provenance() {
    let first = evidence("tool call", "tool-call");
    let second = evidence("policy note", "policy-note");
    let request = tool_risk_request_with_evidence(vec![first.clone(), second.clone()]);

    let outcome = parse_tool_risk_review_model_judgment_output(
        &model_tool_risk_output(
            "high",
            vec![
                json!({ "index": 1, "label": "policy note" }),
                json!({ "index": 0, "label": "tool call" }),
            ],
        ),
        &request,
        "openai risk reviewer",
    )
    .expect("valid cited evidence parses");

    assert_eq!(outcome.evidence(), &[second, first]);
    assert_eq!(outcome.provenance().source_kind(), JudgmentSourceKind::Llm);
    assert_eq!(outcome.provenance().source_label(), "openai risk reviewer");
}

#[test]
fn model_judgment_tool_risk_output_allows_empty_evidence() {
    let request = tool_risk_request();
    let outcome = parse_tool_risk_review_model_judgment_output(
        &model_tool_risk_output("medium", Vec::new()),
        &request,
        "test llm source",
    )
    .expect("tool risk review allows empty evidence");

    assert!(outcome.evidence().is_empty());
}

#[test]
fn model_judgment_tool_risk_output_allows_empty_concerns() {
    let request = tool_risk_request();
    let output = model_tool_risk_output_with_recommendation_extra(json!({ "concerns": [] }));
    let outcome =
        parse_tool_risk_review_model_judgment_output(&output, &request, "test llm source")
            .expect("tool risk review allows empty concerns");

    assert_eq!(
        outcome.recommendation(),
        &JudgmentRecommendation::ToolRiskReview {
            risk: JudgmentRiskLevel::Low,
            concerns: Vec::new(),
        }
    );
}

#[test]
fn model_judgment_output_rejects_wrapped_or_non_object_json() {
    let request = tool_risk_request();
    let valid = model_tool_risk_output("low", Vec::new());

    for output in [
        format!("```json\n{valid}\n```"),
        format!("review result:\n{valid}"),
        format!("{valid}\nreview complete"),
        format!("{valid}\n{valid}"),
        String::new(),
        "   ".to_owned(),
        "[]".to_owned(),
        "null".to_owned(),
    ] {
        assert_eq!(
            parse_tool_risk_review_model_judgment_output(&output, &request, "test llm source",)
                .expect_err("non-strict model output rejects"),
            JudgmentError::InvalidModelJudgmentOutput
        );
    }
}

#[test]
fn model_judgment_output_rejects_unknown_or_missing_top_level_fields() {
    let request = tool_risk_request();

    for output in [
        model_tool_risk_output_with_extra(json!({ "extra": "field" })),
        json!({
            "purpose": "tool_risk_review",
            "recommendation": {
                "kind": "tool_risk_review",
                "risk": "low",
                "concerns": ["Concern text."]
            },
            "confidence": 0.75,
            "evidence": [],
            "rationale": "Rationale is present.",
            "uncertainty": "Uncertainty is present."
        })
        .to_string(),
    ] {
        assert_eq!(
            parse_tool_risk_review_model_judgment_output(&output, &request, "test llm source",)
                .expect_err("unknown or missing model field rejects"),
            JudgmentError::InvalidModelJudgmentOutput
        );
    }
}

#[test]
fn model_judgment_output_rejects_unknown_nested_fields() {
    let request = tool_risk_request_with_evidence(vec![evidence("tool call", "tool-call")]);

    let unknown_recommendation_field = model_tool_risk_output_with_recommendation_extra(json!({
        "explanation": "not part of the strict recommendation schema"
    }));
    assert_eq!(
        parse_tool_risk_review_model_judgment_output(
            &unknown_recommendation_field,
            &request,
            "test llm source",
        )
        .expect_err("unknown recommendation field rejects"),
        JudgmentError::InvalidModelJudgmentOutput
    );

    let unknown_evidence_field = model_tool_risk_output(
        "low",
        vec![json!({
            "index": 0,
            "label": "tool call",
            "excerpt": "not part of the strict evidence citation schema"
        })],
    );
    assert_eq!(
        parse_tool_risk_review_model_judgment_output(
            &unknown_evidence_field,
            &request,
            "test llm source",
        )
        .expect_err("unknown evidence citation field rejects"),
        JudgmentError::InvalidModelJudgmentOutput
    );
}

#[test]
fn model_judgment_output_rejects_non_array_evidence() {
    let request = tool_risk_request();
    let output = model_tool_risk_output_with_extra(json!({
        "evidence": {
            "index": 0,
            "label": "tool call"
        }
    }));

    assert_eq!(
        parse_tool_risk_review_model_judgment_output(&output, &request, "test llm source")
            .expect_err("non-array evidence rejects"),
        JudgmentError::InvalidModelJudgmentOutput
    );
}

#[test]
fn model_judgment_output_rejects_bad_schema_purpose_kind_and_risk() {
    let request = tool_risk_request();

    let bad_schema = model_tool_risk_output_with_extra(json!({
        "schema_version": "merry.model_judgment_output.v2"
    }));
    assert_eq!(
        parse_tool_risk_review_model_judgment_output(&bad_schema, &request, "test llm source",)
            .expect_err("bad schema version rejects"),
        JudgmentError::InvalidModelJudgmentLiteral {
            field: "schema_version",
            expected: MODEL_JUDGMENT_OUTPUT_SCHEMA_VERSION,
            actual: "merry.model_judgment_output.v2".to_owned(),
        }
    );

    let purpose_mismatch = model_tool_risk_output_with_extra(json!({
        "purpose": "summary_draft"
    }));
    assert_eq!(
        parse_tool_risk_review_model_judgment_output(
            &purpose_mismatch,
            &request,
            "test llm source",
        )
        .expect_err("purpose mismatch rejects"),
        JudgmentError::InvalidModelJudgmentLiteral {
            field: "purpose",
            expected: "tool_risk_review",
            actual: "summary_draft".to_owned(),
        }
    );

    let wrong_kind = model_tool_risk_output_with_recommendation_extra(json!({
        "kind": "summary_draft"
    }));
    assert_eq!(
        parse_tool_risk_review_model_judgment_output(&wrong_kind, &request, "test llm source",)
            .expect_err("wrong recommendation kind rejects"),
        JudgmentError::InvalidModelJudgmentLiteral {
            field: "recommendation.kind",
            expected: "tool_risk_review",
            actual: "summary_draft".to_owned(),
        }
    );

    let unknown_risk = model_tool_risk_output_with_recommendation_extra(json!({
        "risk": "critical"
    }));
    assert_eq!(
        parse_tool_risk_review_model_judgment_output(&unknown_risk, &request, "test llm source",)
            .expect_err("unknown risk rejects"),
        JudgmentError::InvalidModelJudgmentLiteral {
            field: "recommendation.risk",
            expected: MODEL_JUDGMENT_TOOL_RISK_EXPECTED_RISK,
            actual: "critical".to_owned(),
        }
    );
}

#[test]
fn model_judgment_output_rejects_invalid_confidence_and_blank_fields() {
    let request = tool_risk_request();

    let invalid_confidence = model_tool_risk_output_with_extra(json!({ "confidence": 1.01 }));
    assert_eq!(
        parse_tool_risk_review_model_judgment_output(
            &invalid_confidence,
            &request,
            "test llm source",
        )
        .expect_err("invalid confidence rejects"),
        JudgmentError::InvalidConfidence { value: 1.01 }
    );

    let blank_rationale = model_tool_risk_output_with_extra(json!({ "rationale": " " }));
    assert_eq!(
            parse_tool_risk_review_model_judgment_output(
                &blank_rationale,
                &request,
                "test llm source",
            )
            .expect_err("blank rationale rejects"),
            JudgmentError::BlankField {
                field: "judgment outcome rationale"
            }
        );

    let blank_uncertainty = model_tool_risk_output_with_extra(json!({ "uncertainty": " " }));
    assert_eq!(
        parse_tool_risk_review_model_judgment_output(
            &blank_uncertainty,
            &request,
            "test llm source",
        )
        .expect_err("blank uncertainty rejects"),
        JudgmentError::BlankField {
            field: "judgment outcome uncertainty"
        }
    );

    let blank_concern = model_tool_risk_output_with_recommendation_extra(json!({
        "concerns": [" "]
    }));
    assert_eq!(
        parse_tool_risk_review_model_judgment_output(&blank_concern, &request, "test llm source",)
            .expect_err("blank concern rejects"),
        JudgmentError::BlankField {
            field: "judgment tool risk concern"
        }
    );
}

#[test]
fn model_judgment_output_rejects_bad_evidence_citations() {
    let request = tool_risk_request_with_evidence(vec![
        evidence("tool call", "tool-call"),
        evidence("policy note", "policy-note"),
    ]);

    let out_of_range = model_tool_risk_output(
        "low",
        vec![json!({ "index": 2, "label": "missing evidence" })],
    );
    assert_eq!(
        parse_tool_risk_review_model_judgment_output(&out_of_range, &request, "test llm source",)
            .expect_err("out-of-range evidence citation rejects"),
        JudgmentError::ModelJudgmentEvidenceIndexOutOfRange { index: 2 }
    );

    let duplicate = model_tool_risk_output(
        "low",
        vec![
            json!({ "index": 0, "label": "tool call" }),
            json!({ "index": 0, "label": "tool call" }),
        ],
    );
    assert_eq!(
        parse_tool_risk_review_model_judgment_output(&duplicate, &request, "test llm source",)
            .expect_err("duplicate evidence citation rejects"),
        JudgmentError::DuplicateModelJudgmentEvidenceCitation { index: 0 }
    );

    let label_mismatch = model_tool_risk_output(
        "low",
        vec![json!({ "index": 1, "label": "renamed evidence" })],
    );
    assert_eq!(
        parse_tool_risk_review_model_judgment_output(&label_mismatch, &request, "test llm source",)
            .expect_err("evidence label mismatch rejects"),
        JudgmentError::ModelJudgmentEvidenceLabelMismatch {
            index: 1,
            expected: "policy note".to_owned(),
            actual: "renamed evidence".to_owned(),
        }
    );
}

#[test]
fn model_judgment_output_rejects_authority_fields_as_unknown() {
    let request = tool_risk_request();

    for output in [
        model_tool_risk_output_with_extra(json!({
            "provenance": {
                "source_kind": "llm",
                "source_label": "model supplied"
            }
        })),
        model_tool_risk_output_with_extra(json!({ "action": "run_tool" })),
        model_tool_risk_output_with_extra(json!({ "allow": true })),
        model_tool_risk_output_with_extra(json!({ "deny": false })),
    ] {
        assert_eq!(
            parse_tool_risk_review_model_judgment_output(&output, &request, "test llm source",)
                .expect_err("authority field rejects"),
            JudgmentError::InvalidModelJudgmentOutput
        );
    }
}

#[test]
fn model_judgment_output_rejects_non_tool_risk_review_requests() {
    let error = parse_tool_risk_review_model_judgment_output(
        &model_tool_risk_output("low", Vec::new()),
        &memory_relevance_request(),
        "test llm source",
    )
    .expect_err("non-tool-risk request rejects");

    assert_eq!(
        error,
        JudgmentError::ModelJudgmentPurposeRequired {
            actual_purpose: JudgmentPurpose::MemoryRelevance,
        }
    );
}

#[test]
fn model_judgment_output_parser_is_pure_and_non_authoritative() {
    let request = tool_risk_request();
    let outcome = parse_tool_risk_review_model_judgment_output(
        &model_tool_risk_output("high", Vec::new()),
        &request,
        "test llm source",
    )
    .expect("valid tool risk model output parses");

    assert_eq!(request.evidence(), &[]);
    assert_eq!(outcome.purpose(), JudgmentPurpose::ToolRiskReview);
    assert!(outcome.evidence().is_empty());
    assert_eq!(outcome.provenance().source_kind(), JudgmentSourceKind::Llm);
}

#[test]
fn registry_generates_stable_record_ids_and_snapshot_order() {
    let mut registry = JudgmentRegistry::default();
    let first = registry
        .record_completed(memory_relevance_request(), memory_relevant_outcome())
        .expect("first record should commit");
    let second = registry
        .record_completed(tool_risk_request(), high_tool_risk_outcome())
        .expect("second record should commit");

    assert_eq!(first.id().as_str(), "judgment-record-00000000000000000000");
    assert_eq!(second.id().as_str(), "judgment-record-00000000000000000001");
    assert_eq!(first.commit_order(), 0);
    assert_eq!(second.commit_order(), 1);

    let snapshot = registry.snapshot();
    assert_eq!(
        snapshot
            .records()
            .iter()
            .map(|record| record.id().as_str())
            .collect::<Vec<_>>(),
        vec![
            "judgment-record-00000000000000000000",
            "judgment-record-00000000000000000001",
        ]
    );
}

#[test]
fn registry_payloads_include_schema_version_and_core_fields() {
    let mut registry = JudgmentRegistry::default();
    let record = registry
        .record_completed(summary_draft_request(), summary_draft_outcome())
        .expect("summary draft record should commit");
    let request_payload = record.artifacts().request().content();
    let outcome_payload = record.artifacts().outcome().content();

    assert_eq!(
        record.artifacts().request().id().as_str(),
        "judgment-record-00000000000000000000-request"
    );
    assert_eq!(
        record.artifacts().outcome().id().as_str(),
        "judgment-record-00000000000000000000-outcome"
    );
    assert!(request_payload.contains("schema_version=merry.judgment.audit.v1\n"));
    assert!(request_payload.contains("artifact=request\n"));
    assert!(request_payload.contains("purpose=summary_draft\n"));
    assert!(request_payload.contains("subject=session summary\n"));
    assert!(request_payload.contains("input=draft a compact summary\\nwith evidence\n"));
    assert!(request_payload.contains("constraints.0=advisory semantic signal only\n"));
    assert!(request_payload.contains("evidence.0.artifact_id=summary-source\n"));
    assert!(request_payload.contains("evidence.0.locator=whole\n"));

    assert!(outcome_payload.contains("schema_version=merry.judgment.audit.v1\n"));
    assert!(outcome_payload.contains("artifact=outcome\n"));
    assert!(outcome_payload.contains("purpose=summary_draft\n"));
    assert!(outcome_payload.contains("recommendation.kind=summary_draft\n"));
    assert!(outcome_payload.contains("recommendation.draft=Summary draft from exact evidence.\n"));
    assert!(outcome_payload.contains("confidence=0.750000\n"));
    assert!(outcome_payload.contains("rationale=The draft uses the supplied artifact evidence.\n"));
    assert!(outcome_payload.contains("uncertainty=Coverage is partial.\n"));
    assert!(outcome_payload.contains("provenance.kind=test\n"));
    assert!(outcome_payload.contains("provenance.label=test source\n"));
}

#[test]
fn registry_rejects_record_purpose_mismatch() {
    let mut registry = JudgmentRegistry::default();
    let error = registry
        .record_completed(memory_relevance_request(), high_tool_risk_outcome())
        .expect_err("mismatched request and outcome purposes should be rejected");

    assert_eq!(
        error,
        JudgmentError::RecordPurposeMismatch {
            request_purpose: JudgmentPurpose::MemoryRelevance,
            outcome_purpose: JudgmentPurpose::ToolRiskReview,
        }
    );
    assert!(registry.is_empty());
}

#[test]
fn registry_rejects_duplicate_manual_record_id() {
    let mut registry = JudgmentRegistry::default();
    let id = JudgmentRecordId::new("manual-record").expect("manual id is valid");
    registry
        .record_completed_with_id(
            id.clone(),
            memory_relevance_request(),
            memory_relevant_outcome(),
        )
        .expect("first manual id record should commit");

    let error = registry
        .record_completed_with_id(
            id.clone(),
            memory_relevance_request(),
            memory_relevant_outcome(),
        )
        .expect_err("duplicate manual id should be rejected");

    assert_eq!(error, JudgmentError::DuplicateRecordId { id });
    assert_eq!(registry.snapshot().records().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn source_trait_can_be_called_through_arc_dyn() {
    let source: Arc<dyn JudgmentSource> = Arc::new(NoopJudgmentSource);

    let outcome = source
        .judge(
            memory_relevance_request(),
            JudgmentContext::new(CancellationToken::new()),
        )
        .await
        .expect("noop source returns an advisory outcome");

    assert_eq!(outcome.purpose(), JudgmentPurpose::MemoryRelevance);
    assert_eq!(
        outcome.recommendation(),
        &JudgmentRecommendation::NoRecommendation
    );
    assert_eq!(outcome.confidence().as_f32(), 0.0);
}

#[tokio::test(flavor = "current_thread")]
async fn noop_source_returns_advisory_result_only() {
    let source = NoopJudgmentSource;

    let outcome = source
        .judge(
            memory_relevance_request(),
            JudgmentContext::new(CancellationToken::new()),
        )
        .await
        .expect("noop source returns an advisory outcome");

    assert_eq!(
        outcome.recommendation(),
        &JudgmentRecommendation::NoRecommendation
    );
    assert!(outcome.evidence().is_empty());
    assert_eq!(
        outcome.provenance().source_kind(),
        JudgmentSourceKind::Deterministic
    );
    assert_eq!(outcome.provenance().source_label(), "noop judgment source");
    assert!(outcome.rationale().contains("runtime policy"));
    assert_eq!(
        outcome.uncertainty(),
        "No semantic recommendation was produced."
    );
}

#[test]
fn cancellation_token_is_carried_in_context() {
    let token = CancellationToken::new();
    let context = JudgmentContext::new(token.clone());

    assert!(!context.cancellation_token().is_cancelled());
    token.cancel();
    assert!(context.cancellation_token().is_cancelled());
}

#[tokio::test(flavor = "current_thread")]
async fn noop_source_observes_pre_cancelled_context() {
    let token = CancellationToken::new();
    token.cancel();
    let source = NoopJudgmentSource;

    let error = source
        .judge(memory_relevance_request(), JudgmentContext::new(token))
        .await
        .expect_err("pre-cancelled context is rejected");

    assert_eq!(error, JudgmentError::Cancelled);
}

#[tokio::test(flavor = "current_thread")]
async fn model_backed_judgment_happy_path_returns_tool_risk_with_llm_provenance_and_evidence() {
    let first = evidence("tool call", "tool-call");
    let second = evidence("policy note", "policy-note");
    let request = tool_risk_request_with_evidence(vec![first.clone(), second.clone()]);
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::text(&model_tool_risk_output(
            "high",
            vec![json!({ "index": 1, "label": "policy note" })],
        ))],
        FinishReason::Stop,
    ))]);
    let source = model_backed_source(provider.clone());

    let outcome = source
        .judge(request, JudgmentContext::new(CancellationToken::new()))
        .await
        .expect("valid model-backed judgment returns an outcome");

    assert_eq!(outcome.purpose(), JudgmentPurpose::ToolRiskReview);
    assert_eq!(
        outcome.recommendation(),
        &JudgmentRecommendation::ToolRiskReview {
            risk: JudgmentRiskLevel::High,
            concerns: vec!["The pending tool path may affect external state.".to_owned()],
        }
    );
    assert_eq!(outcome.evidence(), &[second]);
    assert_eq!(outcome.provenance().source_kind(), JudgmentSourceKind::Llm);
    assert_eq!(
        outcome.provenance().source_label(),
        "test model judgment source"
    );
    assert_eq!(provider.recorded_requests().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn model_backed_judgment_records_expected_model_request_shape() {
    let request = tool_risk_request_with_evidence(vec![
        JudgmentEvidence::new(
            "tool call",
            EvidenceRef::new(
                artifact_id("tool-call"),
                EvidenceLocator::line_range(3, 9).expect("valid line range"),
            ),
        )
        .expect("judgment evidence is valid"),
        JudgmentEvidence::new(
            "policy note",
            EvidenceRef::new(
                artifact_id("policy-note"),
                EvidenceLocator::json_pointer("/risk").expect("valid json pointer"),
            ),
        )
        .expect("judgment evidence is valid"),
    ]);
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::text(&model_tool_risk_output(
            "low",
            vec![json!({ "index": 0, "label": "tool call" })],
        ))],
        FinishReason::Stop,
    ))]);
    let source = model_backed_source(provider.clone());

    source
        .judge(request, JudgmentContext::new(CancellationToken::new()))
        .await
        .expect("valid model-backed judgment returns an outcome");

    let recorded = provider.recorded_requests();
    let [model_request] = recorded.as_slice() else {
        panic!("expected exactly one recorded model request");
    };
    assert_eq!(model_request.model(), &model_name());
    assert_eq!(model_request.messages().len(), 2);
    assert_eq!(model_request.messages()[0].role(), ModelMessageRole::System);
    assert_eq!(model_request.messages()[1].role(), ModelMessageRole::User);
    assert!(model_request.tools().is_empty());
    assert!(model_request.continuations().is_empty());
    assert_eq!(
        model_request.generation().max_output_tokens(),
        Some(MODEL_BACKED_JUDGMENT_MAX_OUTPUT_TOKENS)
    );
    assert!(!model_request.generation().allow_parallel_tool_calls());

    let system = model_request.messages()[0].content().as_text();
    let user = model_request.messages()[1].content().as_text();
    assert!(system.contains(MODEL_JUDGMENT_OUTPUT_SCHEMA_VERSION));
    assert!(system.contains("purpose tool_risk_review"));
    assert!(system.contains("Return exactly one JSON object"));
    assert!(user.contains("schema_version=merry.model_judgment_output.v1\n"));
    assert!(user.contains("purpose=tool_risk_review\n"));
    assert!(user.contains("subject=lookup tool call\n"));
    assert!(user.contains("input=Review whether the pending tool request has semantic risk.\n"));
    assert!(user.contains("constraints.0=advisory semantic signal only\n"));
    assert!(user.contains("evidence.0.label=tool call\n"));
    assert!(user.contains("evidence.0.artifact_id=tool-call\n"));
    assert!(user.contains("evidence.0.locator=line:3-9\n"));
    assert!(user.contains("evidence.1.label=policy note\n"));
    assert!(user.contains("evidence.1.artifact_id=policy-note\n"));
    assert!(user.contains("evidence.1.locator=json:/risk\n"));
}

#[tokio::test(flavor = "current_thread")]
async fn model_backed_judgment_rejects_non_tool_risk_before_provider_call() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::text(&model_tool_risk_output(
            "low",
            Vec::new(),
        ))],
        FinishReason::Stop,
    ))]);
    let source = model_backed_source(provider.clone());

    let error = source
        .judge(
            memory_relevance_request(),
            JudgmentContext::new(CancellationToken::new()),
        )
        .await
        .expect_err("non-tool-risk request rejects");

    assert_eq!(
        error,
        JudgmentError::ModelJudgmentPurposeRequired {
            actual_purpose: JudgmentPurpose::MemoryRelevance,
        }
    );
    assert!(provider.recorded_requests().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn model_backed_judgment_pre_cancelled_context_records_no_provider_request() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::text(&model_tool_risk_output(
            "low",
            Vec::new(),
        ))],
        FinishReason::Stop,
    ))]);
    let source = model_backed_source(provider.clone());
    let token = CancellationToken::new();
    token.cancel();

    let error = source
        .judge(tool_risk_request(), JudgmentContext::new(token))
        .await
        .expect_err("pre-cancelled context rejects");

    assert_eq!(error, JudgmentError::Cancelled);
    assert!(provider.recorded_requests().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn model_backed_judgment_stream_cancellation_maps_to_cancelled() {
    let provider = FakeModelProvider::new(vec![Err(ModelError::Cancelled)]);
    let source = model_backed_source(provider.clone());

    let error = source
        .judge(
            tool_risk_request(),
            JudgmentContext::new(CancellationToken::new()),
        )
        .await
        .expect_err("stream cancellation rejects");

    assert_eq!(error, JudgmentError::Cancelled);
    assert_eq!(provider.recorded_requests().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn model_backed_judgment_provider_cancelled_kind_maps_to_cancelled() {
    let provider = FakeModelProvider::new(vec![Err(ModelError::provider(
        ProviderErrorKind::Cancelled,
        "provider cancelled request",
    ))]);
    let source = model_backed_source(provider);

    let error = source
        .judge(
            tool_risk_request(),
            JudgmentContext::new(CancellationToken::new()),
        )
        .await
        .expect_err("provider cancellation rejects");

    assert_eq!(error, JudgmentError::Cancelled);
}

#[tokio::test(flavor = "current_thread")]
async fn model_backed_judgment_provider_setup_error_maps_to_typed_cloneable_error() {
    let source = ModelBackedJudgmentSource::new(
        Arc::new(SetupErrorModelProvider::new(
            ProviderErrorKind::Authentication,
            "provider credentials are unavailable",
        )),
        model_name(),
        "test model judgment source",
    )
    .expect("model-backed judgment source is valid");

    let error = source
        .judge(
            tool_risk_request(),
            JudgmentContext::new(CancellationToken::new()),
        )
        .await
        .expect_err("provider setup error rejects");

    assert_eq!(
        error.clone(),
        JudgmentError::ModelJudgmentProviderSetup {
            kind: ProviderErrorKind::Authentication,
            message: "provider credentials are unavailable".to_owned(),
        }
    );
}

#[tokio::test(flavor = "current_thread")]
async fn model_backed_judgment_stream_error_maps_to_typed_cloneable_error() {
    let provider = FakeModelProvider::new(vec![Err(ModelError::provider(
        ProviderErrorKind::Unavailable,
        "provider stream failed",
    ))]);
    let source = model_backed_source(provider);

    let error = source
        .judge(
            tool_risk_request(),
            JudgmentContext::new(CancellationToken::new()),
        )
        .await
        .expect_err("stream provider error rejects");

    assert_eq!(
        error.clone(),
        JudgmentError::ModelJudgmentProviderStream {
            kind: ProviderErrorKind::Unavailable,
            message: "provider stream failed".to_owned(),
        }
    );
}

#[tokio::test(flavor = "current_thread")]
async fn model_backed_judgment_invalid_response_shapes_reject() {
    for (script, expected_reason) in [
        (
            Vec::new(),
            "model judgment stream ended before completed event",
        ),
        (
            vec![Ok(ModelEvent::ToolCallRequested {
                call: model_tool_call(),
            })],
            "model judgment stream must not request tools",
        ),
        (
            vec![Ok(completed_outputs_event(
                vec![ModelOutput::text(&model_tool_risk_output(
                    "low",
                    Vec::new(),
                ))],
                FinishReason::Length,
            ))],
            "model judgment completed without stop finish reason",
        ),
        (
            vec![Ok(completed_outputs_event(Vec::new(), FinishReason::Stop))],
            "model judgment stop output must contain exactly one text item",
        ),
        (
            vec![Ok(completed_outputs_event(
                vec![
                    ModelOutput::text(&model_tool_risk_output("low", Vec::new())),
                    ModelOutput::text(&model_tool_risk_output("medium", Vec::new())),
                ],
                FinishReason::Stop,
            ))],
            "model judgment stop output must contain exactly one text item",
        ),
        (
            vec![Ok(completed_outputs_event(
                vec![ModelOutput::tool_call(model_tool_call())],
                FinishReason::Stop,
            ))],
            "model judgment stop output must contain exactly one text item",
        ),
        (
            vec![Ok(completed_outputs_event(
                vec![
                    ModelOutput::text(&model_tool_risk_output("low", Vec::new())),
                    ModelOutput::tool_call(model_tool_call()),
                ],
                FinishReason::Stop,
            ))],
            "model judgment stop output must contain exactly one text item",
        ),
    ] {
        let provider = FakeModelProvider::new(script);
        let source = model_backed_source(provider);

        let error = source
            .judge(
                tool_risk_request(),
                JudgmentContext::new(CancellationToken::new()),
            )
            .await
            .expect_err("invalid model response shape rejects");

        assert_eq!(
            error,
            JudgmentError::InvalidModelJudgmentResponseShape {
                reason: expected_reason,
            }
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn model_backed_judgment_rejects_explicit_non_stop_finish_reasons() {
    for finish_reason in [FinishReason::ToolCalls, FinishReason::Error] {
        let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
            vec![ModelOutput::text(&model_tool_risk_output(
                "low",
                Vec::new(),
            ))],
            finish_reason,
        ))]);
        let source = model_backed_source(provider);

        let error = source
            .judge(
                tool_risk_request(),
                JudgmentContext::new(CancellationToken::new()),
            )
            .await
            .expect_err("non-stop finish reason rejects");

        assert_eq!(
            error,
            JudgmentError::InvalidModelJudgmentResponseShape {
                reason: "model judgment completed without stop finish reason",
            }
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn model_backed_judgment_completed_cancelled_finish_maps_to_cancelled() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        Vec::new(),
        FinishReason::Cancelled,
    ))]);
    let source = model_backed_source(provider);

    let error = source
        .judge(
            tool_risk_request(),
            JudgmentContext::new(CancellationToken::new()),
        )
        .await
        .expect_err("cancelled finish rejects");

    assert_eq!(error, JudgmentError::Cancelled);
}

#[tokio::test(flavor = "current_thread")]
async fn model_backed_judgment_invalid_strict_json_propagates_parser_error() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::text("not json")],
        FinishReason::Stop,
    ))]);
    let source = model_backed_source(provider);

    let error = source
        .judge(
            tool_risk_request(),
            JudgmentContext::new(CancellationToken::new()),
        )
        .await
        .expect_err("invalid strict JSON rejects");

    assert_eq!(error, JudgmentError::InvalidModelJudgmentOutput);
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

fn memory_relevance_request() -> JudgmentRequest {
    JudgmentRequest::new(
        JudgmentPurpose::MemoryRelevance,
        "candidate memory",
        "Is this memory relevant to the current step?",
        Vec::new(),
        constraints(),
        "test request",
    )
    .expect("memory relevance request is valid")
}

fn tool_risk_request() -> JudgmentRequest {
    JudgmentRequest::new(
        JudgmentPurpose::ToolRiskReview,
        "lookup tool call",
        "Review whether the pending tool request has semantic risk.",
        Vec::new(),
        constraints(),
        "test request",
    )
    .expect("tool risk request is valid")
}

fn tool_risk_request_with_evidence(evidence: Vec<JudgmentEvidence>) -> JudgmentRequest {
    JudgmentRequest::new(
        JudgmentPurpose::ToolRiskReview,
        "lookup tool call",
        "Review whether the pending tool request has semantic risk.",
        evidence,
        constraints(),
        "test request",
    )
    .expect("tool risk request is valid")
}

fn summary_draft_request() -> JudgmentRequest {
    JudgmentRequest::new(
        JudgmentPurpose::SummaryDraft,
        "session summary",
        "draft a compact summary\nwith evidence",
        vec![evidence("source", "summary-source")],
        constraints(),
        "test request",
    )
    .expect("summary draft request is valid")
}

fn model_backed_source(provider: FakeModelProvider) -> ModelBackedJudgmentSource {
    ModelBackedJudgmentSource::new(
        Arc::new(provider),
        model_name(),
        " test model judgment source ",
    )
    .expect("model-backed judgment source is valid")
}

fn model_name() -> ModelName {
    ModelName::new("fake/model").expect("valid model name")
}

fn completed_outputs_event(outputs: Vec<ModelOutput>, finish_reason: FinishReason) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(outputs, finish_reason, None),
    }
}

fn model_tool_call() -> ModelToolCall {
    ModelToolCall::new(
        ModelToolCallId::new("call-1").expect("valid model tool call id"),
        ToolName::new("lookup").expect("valid tool name"),
        ToolArguments::new(Default::default()),
    )
}

#[derive(Debug)]
struct SetupErrorModelProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    kind: ProviderErrorKind,
    message: String,
}

impl SetupErrorModelProvider {
    fn new(kind: ProviderErrorKind, message: impl Into<String>) -> Self {
        Self {
            name: ProviderName::new("setup-error-model-provider")
                .expect("static provider name is valid"),
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .expect("static capabilities are valid"),
            kind,
            message: message.into(),
        }
    }
}

impl ModelProvider for SetupErrorModelProvider {
    fn name(&self) -> &ProviderName {
        &self.name
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn stream_model<'a>(
        &'a self,
        _request: ModelRequest,
        _context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move { Err(ModelError::provider(self.kind, self.message.clone())) })
    }
}

fn memory_relevant_outcome() -> JudgmentOutcome {
    JudgmentOutcome::new(
        JudgmentPurpose::MemoryRelevance,
        JudgmentRecommendation::MemoryRelevant,
        confidence(0.8),
        Vec::new(),
        "The memory overlaps with the request.",
        "Only the supplied text was inspected.",
        provenance(JudgmentSourceKind::Test),
    )
    .expect("memory relevance outcome is valid")
}

fn high_tool_risk_outcome() -> JudgmentOutcome {
    JudgmentOutcome::new(
        JudgmentPurpose::ToolRiskReview,
        JudgmentRecommendation::ToolRiskReview {
            risk: JudgmentRiskLevel::High,
            concerns: vec!["The request may expose credentials.".to_owned()],
        },
        confidence(0.9),
        Vec::new(),
        "The tool input references credential material.",
        "The review is advisory and does not authorize policy.",
        provenance(JudgmentSourceKind::Test),
    )
    .expect("tool risk outcome is valid")
}

fn summary_draft_outcome() -> JudgmentOutcome {
    JudgmentOutcome::new(
        JudgmentPurpose::SummaryDraft,
        JudgmentRecommendation::SummaryDraft {
            draft: "Summary draft from exact evidence.".to_owned(),
        },
        confidence(0.75),
        vec![evidence("used source", "summary-source")],
        "The draft uses the supplied artifact evidence.",
        "Coverage is partial.",
        provenance(JudgmentSourceKind::Test),
    )
    .expect("summary draft outcome is valid")
}

fn confidence(value: f32) -> JudgmentConfidence {
    JudgmentConfidence::new(value).expect("confidence is valid")
}

fn provenance(kind: JudgmentSourceKind) -> JudgmentProvenance {
    JudgmentProvenance::new(kind, "test source").expect("provenance is valid")
}

fn constraints() -> Vec<String> {
    vec!["advisory semantic signal only".to_owned()]
}

fn evidence(label: &str, id: &str) -> JudgmentEvidence {
    JudgmentEvidence::new(label, evidence_ref(id)).expect("judgment evidence is valid")
}

fn promotion_input(summary_id: &str, draft_text: &str) -> SummaryDraftPromotionInput {
    SummaryDraftPromotionInput::new(
        summary_id,
        draft_text,
        vec![evidence("source", "summary-source")],
        acceptance(),
        None,
    )
    .expect("summary draft promotion input is valid")
}

fn acceptance() -> SummaryDraftAcceptance {
    SummaryDraftAcceptance::new(
        SummaryDraftAcceptanceAuthority::HardPolicy,
        "hard policy",
        "Hard policy accepted the draft for context promotion.",
    )
    .expect("summary draft acceptance is valid")
}

fn evidence_ref(id: &str) -> EvidenceRef {
    EvidenceRef::new(artifact_id(id), EvidenceLocator::whole_artifact())
}

fn artifact_id(value: &str) -> ArtifactId {
    ArtifactId::new(value).expect("artifact id is valid")
}

fn model_tool_risk_output(risk: &str, evidence: Vec<serde_json::Value>) -> String {
    json!({
        "schema_version": MODEL_JUDGMENT_OUTPUT_SCHEMA_VERSION,
        "purpose": "tool_risk_review",
        "recommendation": {
            "kind": "tool_risk_review",
            "risk": risk,
            "concerns": ["The pending tool path may affect external state."]
        },
        "confidence": 0.75,
        "evidence": evidence,
        "rationale": "The requested tool path has semantic risk for policy to consider.",
        "uncertainty": "The review is advisory and does not authorize the tool."
    })
    .to_string()
}

fn model_tool_risk_output_with_extra(extra: serde_json::Value) -> String {
    let mut output = json!({
        "schema_version": MODEL_JUDGMENT_OUTPUT_SCHEMA_VERSION,
        "purpose": "tool_risk_review",
        "recommendation": {
            "kind": "tool_risk_review",
            "risk": "low",
            "concerns": ["The pending tool path may affect external state."]
        },
        "confidence": 0.75,
        "evidence": [],
        "rationale": "The requested tool path has semantic risk for policy to consider.",
        "uncertainty": "The review is advisory and does not authorize the tool."
    });

    merge_json_object(&mut output, extra);
    output.to_string()
}

fn model_tool_risk_output_with_recommendation_extra(extra: serde_json::Value) -> String {
    let mut output = json!({
        "schema_version": MODEL_JUDGMENT_OUTPUT_SCHEMA_VERSION,
        "purpose": "tool_risk_review",
        "recommendation": {
            "kind": "tool_risk_review",
            "risk": "low",
            "concerns": ["The pending tool path may affect external state."]
        },
        "confidence": 0.75,
        "evidence": [],
        "rationale": "The requested tool path has semantic risk for policy to consider.",
        "uncertainty": "The review is advisory and does not authorize the tool."
    });

    merge_json_object(&mut output["recommendation"], extra);
    output.to_string()
}

fn merge_json_object(target: &mut serde_json::Value, patch: serde_json::Value) {
    let target = target.as_object_mut().expect("target is a JSON object");
    let patch = patch.as_object().expect("patch is a JSON object");
    for (key, value) in patch {
        target.insert(key.clone(), value.clone());
    }
}
