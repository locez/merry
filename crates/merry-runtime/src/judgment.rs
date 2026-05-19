//! Internal advisory judgment boundary.
//!
//! Judgment is a semantic signal source for runtime-owned decisions, not a
//! runtime policy authority. Hard runtime policy still decides whether tools,
//! actions, or context mutations are allowed. Provider wire formats must not
//! enter this module; provider crates adapt external APIs into Merry-owned
//! traits and values before runtime sees them.
//!
//! Summaries remain navigation only. Any exact evidence used to draft or assess
//! a summary must remain available through artifact-backed [`EvidenceRef`]
//! values, and a judgment outcome never replaces those artifacts.

// Staged internal judgment types are compiled before runtime call paths are wired.
#![cfg_attr(not(test), allow(dead_code))]

use merry_core::EvidenceRef;
use std::{fmt, future::Future, pin::Pin};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Semantic purpose for an internal advisory judgment request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JudgmentPurpose {
    /// Assess whether memory is relevant to a step or runtime state.
    MemoryRelevance,
    /// Draft summary text from exact artifact-backed evidence.
    SummaryDraft,
    /// Review semantic risk for a tool-related path without authorizing it.
    ToolRiskReview,
}

impl JudgmentPurpose {
    fn requires_request_evidence(self) -> bool {
        matches!(self, Self::SummaryDraft)
    }
}

impl fmt::Display for JudgmentPurpose {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::MemoryRelevance => "memory relevance",
            Self::SummaryDraft => "summary draft",
            Self::ToolRiskReview => "tool risk review",
        })
    }
}

/// Provenance category for an internal advisory judgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JudgmentSourceKind {
    /// Produced by deterministic runtime code.
    Deterministic,
    /// Produced by a model through a Merry-owned source boundary.
    Llm,
    /// Produced by an explicit human decision or review input.
    Human,
    /// Produced by a test source.
    Test,
}

/// Validated confidence in the inclusive finite 0.0..=1.0 range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct JudgmentConfidence(f32);

impl JudgmentConfidence {
    pub(crate) fn new(value: f32) -> Result<Self, JudgmentError> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(JudgmentError::InvalidConfidence { value });
        }

        let canonical = if value == 0.0 { 0.0 } else { value };
        Ok(Self(canonical))
    }

    #[must_use]
    pub(crate) fn as_f32(self) -> f32 {
        self.0
    }
}

/// Exact evidence supplied to or produced by an advisory judgment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JudgmentEvidence {
    label: String,
    reference: EvidenceRef,
}

impl JudgmentEvidence {
    pub(crate) fn new(
        label: impl Into<String>,
        reference: EvidenceRef,
    ) -> Result<Self, JudgmentError> {
        let label = label.into();
        validate_non_blank("judgment evidence label", &label)?;

        Ok(Self {
            label: canonicalize_label_text(&label),
            reference,
        })
    }

    #[must_use]
    pub(crate) fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub(crate) fn reference(&self) -> &EvidenceRef {
        &self.reference
    }
}

/// Source metadata for an advisory judgment outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JudgmentProvenance {
    source_kind: JudgmentSourceKind,
    source_label: String,
}

impl JudgmentProvenance {
    pub(crate) fn new(
        source_kind: JudgmentSourceKind,
        source_label: impl Into<String>,
    ) -> Result<Self, JudgmentError> {
        let source_label = source_label.into();
        validate_non_blank("judgment provenance source label", &source_label)?;

        Ok(Self {
            source_kind,
            source_label: canonicalize_label_text(&source_label),
        })
    }

    #[must_use]
    pub(crate) fn source_kind(&self) -> JudgmentSourceKind {
        self.source_kind
    }

    #[must_use]
    pub(crate) fn source_label(&self) -> &str {
        &self.source_label
    }
}

/// Crate-internal semantic judgment request.
///
/// Requests are intentionally provider-neutral. `constraints` records the
/// runtime-owned boundary for the semantic judgment; hard policy remains
/// outside this request. `SummaryDraft` requests require exact evidence because
/// summaries must stay grounded in retrievable artifacts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JudgmentRequest {
    purpose: JudgmentPurpose,
    subject: String,
    input: String,
    evidence: Vec<JudgmentEvidence>,
    constraints: Vec<String>,
    source_label: String,
}

impl JudgmentRequest {
    pub(crate) fn new(
        purpose: JudgmentPurpose,
        subject: impl Into<String>,
        input: impl Into<String>,
        evidence: Vec<JudgmentEvidence>,
        constraints: Vec<String>,
        source_label: impl Into<String>,
    ) -> Result<Self, JudgmentError> {
        let subject = subject.into();
        validate_non_blank("judgment request subject", &subject)?;

        let input = input.into();
        validate_non_blank("judgment request input", &input)?;

        if purpose.requires_request_evidence() && evidence.is_empty() {
            return Err(JudgmentError::MissingEvidence {
                purpose,
                field: "judgment request evidence",
            });
        }

        if constraints.is_empty() {
            return Err(JudgmentError::EmptyConstraints);
        }
        let constraints = constraints
            .into_iter()
            .map(|constraint| {
                validate_non_blank("judgment request constraint", &constraint)?;
                Ok(canonicalize_label_text(&constraint))
            })
            .collect::<Result<Vec<_>, JudgmentError>>()?;

        let source_label = source_label.into();
        validate_non_blank("judgment request source label", &source_label)?;

        Ok(Self {
            purpose,
            subject: canonicalize_label_text(&subject),
            input,
            evidence,
            constraints,
            source_label: canonicalize_label_text(&source_label),
        })
    }

    #[must_use]
    pub(crate) fn purpose(&self) -> JudgmentPurpose {
        self.purpose
    }

    #[must_use]
    pub(crate) fn subject(&self) -> &str {
        &self.subject
    }

    #[must_use]
    pub(crate) fn input(&self) -> &str {
        &self.input
    }

    #[must_use]
    pub(crate) fn evidence(&self) -> &[JudgmentEvidence] {
        &self.evidence
    }

    #[must_use]
    pub(crate) fn constraints(&self) -> &[String] {
        &self.constraints
    }

    #[must_use]
    pub(crate) fn source_label(&self) -> &str {
        &self.source_label
    }
}

/// Advisory risk level for [`JudgmentRecommendation::ToolRiskReview`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JudgmentRiskLevel {
    /// The source found no material semantic risk signal.
    Low,
    /// The source found a material risk signal that policy should consider.
    Medium,
    /// The source found a high risk signal that policy should consider.
    High,
    /// The source could not assess risk from the available semantic input.
    Unknown,
}

/// Structured advisory recommendation from a judgment source.
///
/// These variants deliberately describe semantic recommendations only. They do
/// not grant permission to execute tools, mutate context, or perform actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JudgmentRecommendation {
    /// Memory appears semantically relevant.
    MemoryRelevant,
    /// Memory does not appear semantically relevant.
    MemoryNotRelevant,
    /// Draft summary text grounded in exact evidence.
    SummaryDraft { draft: String },
    /// Tool path risk review for runtime policy to consider.
    ToolRiskReview {
        risk: JudgmentRiskLevel,
        concerns: Vec<String>,
    },
    /// The source produced no semantic recommendation.
    NoRecommendation,
}

impl JudgmentRecommendation {
    fn kind(&self) -> &'static str {
        match self {
            Self::MemoryRelevant | Self::MemoryNotRelevant => "memory relevance",
            Self::SummaryDraft { .. } => "summary draft",
            Self::ToolRiskReview { .. } => "tool risk review",
            Self::NoRecommendation => "no recommendation",
        }
    }

    fn matches_purpose(&self, purpose: JudgmentPurpose) -> bool {
        matches!(
            (purpose, self),
            (JudgmentPurpose::MemoryRelevance, Self::MemoryRelevant)
                | (JudgmentPurpose::MemoryRelevance, Self::MemoryNotRelevant)
                | (JudgmentPurpose::SummaryDraft, Self::SummaryDraft { .. })
                | (JudgmentPurpose::ToolRiskReview, Self::ToolRiskReview { .. })
                | (_, Self::NoRecommendation)
        )
    }

    fn requires_outcome_evidence(&self) -> bool {
        matches!(self, Self::SummaryDraft { .. })
    }
}

/// Advisory outcome from an internal judgment source.
///
/// An outcome is evidence, confidence, rationale, uncertainty, and provenance
/// for runtime policy to inspect. It cannot encode direct permission for a
/// tool/action/context mutation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct JudgmentOutcome {
    purpose: JudgmentPurpose,
    recommendation: JudgmentRecommendation,
    confidence: JudgmentConfidence,
    evidence: Vec<JudgmentEvidence>,
    rationale: String,
    uncertainty: String,
    provenance: JudgmentProvenance,
}

impl JudgmentOutcome {
    pub(crate) fn new(
        purpose: JudgmentPurpose,
        recommendation: JudgmentRecommendation,
        confidence: JudgmentConfidence,
        evidence: Vec<JudgmentEvidence>,
        rationale: impl Into<String>,
        uncertainty: impl Into<String>,
        provenance: JudgmentProvenance,
    ) -> Result<Self, JudgmentError> {
        validate_recommendation(purpose, &recommendation)?;

        if recommendation.requires_outcome_evidence() && evidence.is_empty() {
            return Err(JudgmentError::MissingEvidence {
                purpose,
                field: "judgment outcome evidence",
            });
        }

        let rationale = rationale.into();
        validate_non_blank("judgment outcome rationale", &rationale)?;

        let uncertainty = uncertainty.into();
        validate_non_blank("judgment outcome uncertainty", &uncertainty)?;

        Ok(Self {
            purpose,
            recommendation,
            confidence,
            evidence,
            rationale,
            uncertainty,
            provenance,
        })
    }

    #[must_use]
    pub(crate) fn purpose(&self) -> JudgmentPurpose {
        self.purpose
    }

    #[must_use]
    pub(crate) fn recommendation(&self) -> &JudgmentRecommendation {
        &self.recommendation
    }

    #[must_use]
    pub(crate) fn confidence(&self) -> JudgmentConfidence {
        self.confidence
    }

    #[must_use]
    pub(crate) fn evidence(&self) -> &[JudgmentEvidence] {
        &self.evidence
    }

    #[must_use]
    pub(crate) fn rationale(&self) -> &str {
        &self.rationale
    }

    #[must_use]
    pub(crate) fn uncertainty(&self) -> &str {
        &self.uncertainty
    }

    #[must_use]
    pub(crate) fn provenance(&self) -> &JudgmentProvenance {
        &self.provenance
    }
}

/// Result returned by a crate-internal advisory judgment source.
pub(crate) type JudgmentResult = Result<JudgmentOutcome, JudgmentError>;

/// Boxed judgment future used for object-safe async boundaries.
pub(crate) type JudgmentFuture<'a> = Pin<Box<dyn Future<Output = JudgmentResult> + Send + 'a>>;

/// Context passed to an advisory judgment source.
#[derive(Debug, Clone)]
pub(crate) struct JudgmentContext {
    cancellation_token: CancellationToken,
}

impl JudgmentContext {
    #[must_use]
    pub(crate) fn new(cancellation_token: CancellationToken) -> Self {
        Self { cancellation_token }
    }

    #[must_use]
    pub(crate) fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation_token
    }
}

/// Object-safe crate-internal advisory judgment source boundary.
pub(crate) trait JudgmentSource: Send + Sync {
    fn judge<'a>(
        &'a self,
        request: JudgmentRequest,
        context: JudgmentContext,
    ) -> JudgmentFuture<'a>;
}

/// Deterministic placeholder source that produces no semantic recommendation.
#[derive(Debug, Default)]
pub(crate) struct NoopJudgmentSource;

impl JudgmentSource for NoopJudgmentSource {
    fn judge<'a>(
        &'a self,
        request: JudgmentRequest,
        context: JudgmentContext,
    ) -> JudgmentFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(JudgmentError::Cancelled);
            }

            JudgmentOutcome::new(
                request.purpose(),
                JudgmentRecommendation::NoRecommendation,
                JudgmentConfidence::new(0.0)?,
                Vec::new(),
                "No judgment source is configured; runtime policy must make any hard decision without this advisory signal.",
                "No semantic recommendation was produced.",
                JudgmentProvenance::new(JudgmentSourceKind::Deterministic, "noop judgment source")?,
            )
        })
    }
}

/// Errors raised while building or requesting internal advisory judgments.
#[derive(Debug, Clone, PartialEq, Error)]
pub(crate) enum JudgmentError {
    /// A required text field was blank.
    #[error("{field} must not be blank")]
    BlankField {
        /// Name of the invalid field.
        field: &'static str,
    },

    /// Confidence was not finite or outside the valid range.
    #[error("judgment confidence {value} must be finite and inside the inclusive 0.0..=1.0 range")]
    InvalidConfidence {
        /// Rejected confidence value.
        value: f32,
    },

    /// A request had no semantic constraints.
    #[error("judgment request must include at least one runtime-owned constraint")]
    EmptyConstraints,

    /// Evidence was required for this judgment purpose or recommendation.
    #[error("{field} requires at least one exact evidence reference for {purpose}")]
    MissingEvidence {
        /// Judgment purpose that required evidence.
        purpose: JudgmentPurpose,
        /// Field that lacked evidence.
        field: &'static str,
    },

    /// Recommendation kind did not match the requested purpose.
    #[error("judgment recommendation {recommendation} is not valid for {purpose}")]
    RecommendationPurposeMismatch {
        /// Requested judgment purpose.
        purpose: JudgmentPurpose,
        /// Rejected recommendation kind.
        recommendation: &'static str,
    },

    /// Judgment source observed cooperative cancellation before producing output.
    #[error("judgment source cancelled before producing an advisory outcome")]
    Cancelled,
}

fn validate_recommendation(
    purpose: JudgmentPurpose,
    recommendation: &JudgmentRecommendation,
) -> Result<(), JudgmentError> {
    if !recommendation.matches_purpose(purpose) {
        return Err(JudgmentError::RecommendationPurposeMismatch {
            purpose,
            recommendation: recommendation.kind(),
        });
    }

    match recommendation {
        JudgmentRecommendation::SummaryDraft { draft } => {
            validate_non_blank("judgment summary draft", draft)
        }
        JudgmentRecommendation::ToolRiskReview { concerns, .. } => {
            for concern in concerns {
                validate_non_blank("judgment tool risk concern", concern)?;
            }
            Ok(())
        }
        JudgmentRecommendation::MemoryRelevant
        | JudgmentRecommendation::MemoryNotRelevant
        | JudgmentRecommendation::NoRecommendation => Ok(()),
    }
}

fn validate_non_blank(field: &'static str, value: &str) -> Result<(), JudgmentError> {
    if value.trim().is_empty() {
        return Err(JudgmentError::BlankField { field });
    }

    Ok(())
}

fn canonicalize_label_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use merry_core::{ArtifactId, EvidenceLocator};
    use std::sync::Arc;

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

    fn evidence_ref(id: &str) -> EvidenceRef {
        EvidenceRef::new(artifact_id(id), EvidenceLocator::whole_artifact())
    }

    fn artifact_id(value: &str) -> ArtifactId {
        ArtifactId::new(value).expect("artifact id is valid")
    }
}
