use super::source::{JudgmentContext, JudgmentFuture, JudgmentSource};
use super::{
    JudgmentConfidence, JudgmentError, JudgmentEvidence, JudgmentOutcome, JudgmentProvenance,
    JudgmentPurpose, JudgmentRecommendation, JudgmentRequest, JudgmentRiskLevel,
    JudgmentSourceKind, push_evidence, push_field, push_list,
};
use futures_util::StreamExt;
use merry_llm::{
    FinishReason, GenerationConfig, ModelContent, ModelError, ModelEvent, ModelMessage,
    ModelMessageRole, ModelName, ModelOutput, ModelProvider, ModelRequest, ModelResponse,
    ModelStreamContext, ProviderErrorKind,
};
use serde::Deserialize;
use std::{collections::BTreeSet, sync::Arc};

pub(crate) const MODEL_JUDGMENT_OUTPUT_SCHEMA_VERSION: &str = "merry.model_judgment_output.v1";
const MODEL_JUDGMENT_TOOL_RISK_RECOMMENDATION_KIND: &str = "tool_risk_review";
pub(crate) const MODEL_JUDGMENT_TOOL_RISK_EXPECTED_RISK: &str = "low, medium, high, or unknown";
pub(crate) const MODEL_BACKED_JUDGMENT_MAX_OUTPUT_TOKENS: u64 = 512;

/// Parse strict model-produced JSON for a tool risk review advisory outcome.
///
/// This is a pure converter. It does not record the judgment, mutate session
/// state, inspect context, emit events, or grant runtime authority.
pub(crate) fn parse_tool_risk_review_model_judgment_output(
    output: &str,
    request: &JudgmentRequest,
    source_label: &str,
) -> Result<JudgmentOutcome, JudgmentError> {
    if request.purpose() != JudgmentPurpose::ToolRiskReview {
        return Err(JudgmentError::ModelJudgmentPurposeRequired {
            actual_purpose: request.purpose(),
        });
    }

    let output = serde_json::from_str::<ModelJudgmentOutput>(output)
        .map_err(|_| JudgmentError::InvalidModelJudgmentOutput)?;

    output.into_tool_risk_review_outcome(request, source_label)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelJudgmentOutput {
    schema_version: String,
    purpose: String,
    recommendation: ModelToolRiskReviewRecommendation,
    confidence: f32,
    evidence: Vec<ModelJudgmentEvidenceCitation>,
    rationale: String,
    uncertainty: String,
}

impl ModelJudgmentOutput {
    fn into_tool_risk_review_outcome(
        self,
        request: &JudgmentRequest,
        source_label: &str,
    ) -> Result<JudgmentOutcome, JudgmentError> {
        validate_model_judgment_literal(
            "schema_version",
            &self.schema_version,
            MODEL_JUDGMENT_OUTPUT_SCHEMA_VERSION,
        )?;
        validate_model_judgment_literal(
            "purpose",
            &self.purpose,
            JudgmentPurpose::ToolRiskReview.as_str(),
        )?;
        validate_model_judgment_literal(
            "recommendation.kind",
            &self.recommendation.kind,
            MODEL_JUDGMENT_TOOL_RISK_RECOMMENDATION_KIND,
        )?;

        let risk = parse_model_judgment_tool_risk_level(&self.recommendation.risk)?;
        let evidence = select_model_judgment_evidence(self.evidence, request)?;
        let provenance = JudgmentProvenance::new(JudgmentSourceKind::Llm, source_label)?;

        JudgmentOutcome::new(
            JudgmentPurpose::ToolRiskReview,
            JudgmentRecommendation::ToolRiskReview {
                risk,
                concerns: self.recommendation.concerns,
            },
            JudgmentConfidence::new(self.confidence)?,
            evidence,
            self.rationale,
            self.uncertainty,
            provenance,
        )
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelToolRiskReviewRecommendation {
    kind: String,
    risk: String,
    concerns: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelJudgmentEvidenceCitation {
    index: usize,
    label: String,
}

/// Provider-neutral model-backed advisory source for tool risk review.
pub(crate) struct ModelBackedJudgmentSource {
    provider: Arc<dyn ModelProvider>,
    model: ModelName,
    source_label: String,
    generation_config: GenerationConfig,
}

impl ModelBackedJudgmentSource {
    pub(crate) fn new(
        provider: Arc<dyn ModelProvider>,
        model: ModelName,
        source_label: impl Into<String>,
    ) -> Result<Self, JudgmentError> {
        let provenance = JudgmentProvenance::new(JudgmentSourceKind::Llm, source_label)?;
        let generation_config =
            GenerationConfig::new(Some(MODEL_BACKED_JUDGMENT_MAX_OUTPUT_TOKENS), false)
                .map_err(map_model_judgment_request_error)?;

        Ok(Self {
            provider,
            model,
            source_label: provenance.source_label().to_owned(),
            generation_config,
        })
    }
}

impl JudgmentSource for ModelBackedJudgmentSource {
    fn judge<'a>(
        &'a self,
        request: JudgmentRequest,
        context: JudgmentContext,
    ) -> JudgmentFuture<'a> {
        Box::pin(async move {
            if request.purpose() != JudgmentPurpose::ToolRiskReview {
                return Err(JudgmentError::ModelJudgmentPurposeRequired {
                    actual_purpose: request.purpose(),
                });
            }

            let token = context.cancellation_token().clone();
            if token.is_cancelled() {
                return Err(JudgmentError::Cancelled);
            }

            let model_request = compile_model_backed_judgment_request(
                &request,
                &self.model,
                self.generation_config.clone(),
            )?;
            let stream_context = ModelStreamContext::new(token.clone());
            let stream_result = tokio::select! {
                biased;
                () = token.cancelled() => return Err(JudgmentError::Cancelled),
                result = self.provider.stream_model(model_request, stream_context) => result,
            };
            let mut stream = stream_result.map_err(map_model_judgment_setup_error)?;

            loop {
                let item = tokio::select! {
                    biased;
                    () = token.cancelled() => return Err(JudgmentError::Cancelled),
                    item = stream.next() => item,
                };

                match item {
                    Some(Ok(ModelEvent::Started | ModelEvent::OutputTextDelta { .. })) => {}
                    Some(Ok(ModelEvent::ToolCallRequested { .. })) => {
                        return Err(JudgmentError::InvalidModelJudgmentResponseShape {
                            reason: "model judgment stream must not request tools",
                        });
                    }
                    Some(Ok(ModelEvent::Completed { response })) => {
                        let text = model_judgment_text_from_completed_response(&response)?;
                        return parse_tool_risk_review_model_judgment_output(
                            text,
                            &request,
                            &self.source_label,
                        );
                    }
                    Some(Err(error)) => {
                        return Err(map_model_judgment_stream_error(error));
                    }
                    None => {
                        return Err(JudgmentError::InvalidModelJudgmentResponseShape {
                            reason: "model judgment stream ended before completed event",
                        });
                    }
                }
            }
        })
    }
}

fn validate_model_judgment_literal(
    field: &'static str,
    actual: &str,
    expected: &'static str,
) -> Result<(), JudgmentError> {
    if actual != expected {
        return Err(JudgmentError::InvalidModelJudgmentLiteral {
            field,
            expected,
            actual: actual.to_owned(),
        });
    }

    Ok(())
}

fn parse_model_judgment_tool_risk_level(value: &str) -> Result<JudgmentRiskLevel, JudgmentError> {
    match value {
        "low" => Ok(JudgmentRiskLevel::Low),
        "medium" => Ok(JudgmentRiskLevel::Medium),
        "high" => Ok(JudgmentRiskLevel::High),
        "unknown" => Ok(JudgmentRiskLevel::Unknown),
        actual => Err(JudgmentError::InvalidModelJudgmentLiteral {
            field: "recommendation.risk",
            expected: MODEL_JUDGMENT_TOOL_RISK_EXPECTED_RISK,
            actual: actual.to_owned(),
        }),
    }
}

fn select_model_judgment_evidence(
    citations: Vec<ModelJudgmentEvidenceCitation>,
    request: &JudgmentRequest,
) -> Result<Vec<JudgmentEvidence>, JudgmentError> {
    let mut selected = Vec::with_capacity(citations.len());
    let mut seen = BTreeSet::new();

    for citation in citations {
        if !seen.insert(citation.index) {
            return Err(JudgmentError::DuplicateModelJudgmentEvidenceCitation {
                index: citation.index,
            });
        }

        let request_evidence = request.evidence().get(citation.index).ok_or(
            JudgmentError::ModelJudgmentEvidenceIndexOutOfRange {
                index: citation.index,
            },
        )?;

        if request_evidence.label() != citation.label {
            return Err(JudgmentError::ModelJudgmentEvidenceLabelMismatch {
                index: citation.index,
                expected: request_evidence.label().to_owned(),
                actual: citation.label,
            });
        }

        selected.push(request_evidence.clone());
    }

    Ok(selected)
}

fn compile_model_backed_judgment_request(
    request: &JudgmentRequest,
    model: &ModelName,
    generation_config: GenerationConfig,
) -> Result<ModelRequest, JudgmentError> {
    let messages = vec![
        ModelMessage::new(
            ModelMessageRole::System,
            ModelContent::text(&model_backed_judgment_system_prompt())
                .map_err(map_model_judgment_request_error)?,
        )
        .map_err(map_model_judgment_request_error)?,
        ModelMessage::new(
            ModelMessageRole::User,
            ModelContent::text(&model_backed_judgment_user_prompt(request))
                .map_err(map_model_judgment_request_error)?,
        )
        .map_err(map_model_judgment_request_error)?,
    ];

    ModelRequest::new(model.clone(), messages, Vec::new(), generation_config)
        .map_err(map_model_judgment_request_error)
}

fn model_backed_judgment_system_prompt() -> String {
    format!(
        concat!(
            "You are a provider-neutral internal advisory judgment source.\n",
            "Return exactly one JSON object and no other text.\n",
            "The result is advisory only and must not authorize tools, actions, context mutation, ledger writes, or events.\n",
            "Use schema_version {schema_version} and purpose {purpose}.\n",
            "Required JSON shape: ",
            "{{\"schema_version\":\"{schema_version}\",\"purpose\":\"{purpose}\",",
            "\"recommendation\":{{\"kind\":\"{purpose}\",\"risk\":\"low|medium|high|unknown\",\"concerns\":[\"...\"]}},",
            "\"confidence\":0.0,\"evidence\":[{{\"index\":0,\"label\":\"exact supplied label\"}}],",
            "\"rationale\":\"...\",\"uncertainty\":\"...\"}}.\n",
            "Cite only supplied evidence by exact index and label."
        ),
        schema_version = MODEL_JUDGMENT_OUTPUT_SCHEMA_VERSION,
        purpose = JudgmentPurpose::ToolRiskReview.as_str(),
    )
}

fn model_backed_judgment_user_prompt(request: &JudgmentRequest) -> String {
    let mut prompt = String::new();
    push_field(
        &mut prompt,
        "schema_version",
        MODEL_JUDGMENT_OUTPUT_SCHEMA_VERSION,
    );
    push_field(
        &mut prompt,
        "purpose",
        JudgmentPurpose::ToolRiskReview.as_str(),
    );
    push_field(&mut prompt, "subject", request.subject());
    push_field(&mut prompt, "input", request.input());
    push_list(&mut prompt, "constraints", request.constraints());
    push_evidence(&mut prompt, "evidence", request.evidence());
    prompt
}

fn model_judgment_text_from_completed_response(
    response: &ModelResponse,
) -> Result<&str, JudgmentError> {
    if response.finish_reason() == FinishReason::Cancelled {
        return Err(JudgmentError::Cancelled);
    }

    if response.finish_reason() != FinishReason::Stop {
        return Err(JudgmentError::InvalidModelJudgmentResponseShape {
            reason: "model judgment completed without stop finish reason",
        });
    }

    let [ModelOutput::Text { text }] = response.outputs() else {
        return Err(JudgmentError::InvalidModelJudgmentResponseShape {
            reason: "model judgment stop output must contain exactly one text item",
        });
    };

    Ok(text)
}

fn map_model_judgment_request_error(error: ModelError) -> JudgmentError {
    if is_cancelled_model_judgment_error(&error) {
        return JudgmentError::Cancelled;
    }

    let (kind, message) = model_error_parts(error);
    JudgmentError::ModelJudgmentRequest { kind, message }
}

fn map_model_judgment_setup_error(error: ModelError) -> JudgmentError {
    if is_cancelled_model_judgment_error(&error) {
        return JudgmentError::Cancelled;
    }

    let (kind, message) = model_error_parts(error);
    JudgmentError::ModelJudgmentProviderSetup { kind, message }
}

fn map_model_judgment_stream_error(error: ModelError) -> JudgmentError {
    if is_cancelled_model_judgment_error(&error) {
        return JudgmentError::Cancelled;
    }

    let (kind, message) = model_error_parts(error);
    JudgmentError::ModelJudgmentProviderStream { kind, message }
}

fn is_cancelled_model_judgment_error(error: &ModelError) -> bool {
    matches!(error, ModelError::Cancelled)
        || matches!(
            error,
            ModelError::Provider {
                kind: ProviderErrorKind::Cancelled,
                ..
            }
        )
}

fn model_error_parts(error: ModelError) -> (ProviderErrorKind, String) {
    match error {
        ModelError::InvalidRequest { reason } => (ProviderErrorKind::InvalidRequest, reason),
        ModelError::Cancelled => (
            ProviderErrorKind::Cancelled,
            "model stream cancelled".to_owned(),
        ),
        ModelError::Provider { kind, message, .. } => (kind, message),
    }
}
