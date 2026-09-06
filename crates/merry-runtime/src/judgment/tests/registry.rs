use crate::judgment::{
    JudgmentError, JudgmentPurpose, JudgmentRecordId, JudgmentRegistry,
    tests::{
        high_tool_risk_outcome, memory_relevance_request, memory_relevant_outcome,
        summary_draft_outcome, summary_draft_request, tool_risk_request,
    },
};

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
    assert!(outcome_payload.contains("recommendation.payload=summary_draft\n"));
    assert!(outcome_payload.contains("recommendation.draft=Summary draft from exact evidence.\n"));
    assert!(outcome_payload.contains("confidence=0.750000\n"));
    assert!(outcome_payload.contains("rationale=The draft uses the supplied artifact evidence.\n"));
    assert!(outcome_payload.contains("uncertainty=Coverage is partial.\n"));
    assert!(outcome_payload.contains("provenance.payload=test\n"));
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
