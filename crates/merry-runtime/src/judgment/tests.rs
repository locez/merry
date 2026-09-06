use crate::judgment::{
    JudgmentConfidence, JudgmentEvidence, JudgmentOutcome, JudgmentProvenance, JudgmentPurpose,
    JudgmentRecommendation, JudgmentRequest, JudgmentRiskLevel, JudgmentSourceKind,
    MODEL_JUDGMENT_OUTPUT_SCHEMA_VERSION, ModelBackedJudgmentSource, SummaryDraftAcceptance,
    SummaryDraftAcceptanceAuthority, SummaryDraftPromotionInput,
};
use merry_core::{ArtifactId, EvidenceLocator, EvidenceRef, ProviderName, ToolName};
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelName,
    ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse,
    ModelStreamContext, ModelToolCall, ModelToolCallId, ProviderErrorKind, ToolArguments,
    testing::FakeModelProvider,
};
use serde_json::json;
use std::sync::Arc;

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

mod contracts;

mod model_source;

mod output_validation;

mod registry;

mod sources;

mod summary_promotion;
