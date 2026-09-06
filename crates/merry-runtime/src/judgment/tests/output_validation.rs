use crate::judgment::{
    JudgmentError, JudgmentPurpose, JudgmentRecommendation, JudgmentRiskLevel, JudgmentSourceKind,
    MODEL_JUDGMENT_OUTPUT_SCHEMA_VERSION, MODEL_JUDGMENT_TOOL_RISK_EXPECTED_RISK,
    parse_tool_risk_review_model_judgment_output,
    tests::{
        evidence, memory_relevance_request, model_tool_risk_output,
        model_tool_risk_output_with_extra, model_tool_risk_output_with_recommendation_extra,
        tool_risk_request, tool_risk_request_with_evidence,
    },
};
use serde_json::json;

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
            field: "recommendation.payload",
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
