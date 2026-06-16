#![cfg_attr(not(test), allow(dead_code))]

use super::{
    error::JudgmentError,
    payload::{canonicalize_label_text, validate_non_blank},
};
use merry_core::EvidenceRef;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Semantic purpose for an internal advisory judgment request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum JudgmentPurpose {
    /// Assess whether memory is relevant to a step or runtime state.
    MemoryRelevance,
    /// Draft summary text from exact artifact-backed evidence.
    SummaryDraft,
    /// Review semantic risk for a tool-related path without authorizing it.
    ToolRiskReview,
}

impl JudgmentPurpose {
    pub(super) fn requires_request_evidence(self) -> bool {
        matches!(self, Self::SummaryDraft)
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::MemoryRelevance => "memory_relevance",
            Self::SummaryDraft => "summary_draft",
            Self::ToolRiskReview => "tool_risk_review",
        }
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

impl JudgmentSourceKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Deterministic => "deterministic",
            Self::Llm => "llm",
            Self::Human => "human",
            Self::Test => "test",
        }
    }
}

/// Validated confidence in the inclusive finite 0.0..=1.0 range.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

impl JudgmentRiskLevel {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Unknown => "unknown",
        }
    }
}

/// Structured advisory recommendation from a judgment source.
///
/// These variants deliberately describe semantic recommendations only. They do
/// not grant permission to execute tools, mutate context, or perform actions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
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
    pub(super) fn kind(&self) -> &'static str {
        match self {
            Self::MemoryRelevant | Self::MemoryNotRelevant => "memory relevance",
            Self::SummaryDraft { .. } => "summary draft",
            Self::ToolRiskReview { .. } => "tool risk review",
            Self::NoRecommendation => "no recommendation",
        }
    }

    pub(super) fn matches_purpose(&self, purpose: JudgmentPurpose) -> bool {
        matches!(
            (purpose, self),
            (JudgmentPurpose::MemoryRelevance, Self::MemoryRelevant)
                | (JudgmentPurpose::MemoryRelevance, Self::MemoryNotRelevant)
                | (JudgmentPurpose::SummaryDraft, Self::SummaryDraft { .. })
                | (JudgmentPurpose::ToolRiskReview, Self::ToolRiskReview { .. })
                | (_, Self::NoRecommendation)
        )
    }

    pub(super) fn requires_outcome_evidence(&self) -> bool {
        matches!(self, Self::SummaryDraft { .. })
    }

    pub(super) fn as_str(&self) -> &'static str {
        match self {
            Self::MemoryRelevant => "memory_relevant",
            Self::MemoryNotRelevant => "memory_not_relevant",
            Self::SummaryDraft { .. } => "summary_draft",
            Self::ToolRiskReview { .. } => "tool_risk_review",
            Self::NoRecommendation => "no_recommendation",
        }
    }
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

/// Advisory outcome from an internal judgment source.
///
/// An outcome is evidence, confidence, rationale, uncertainty, and provenance
/// for runtime policy to inspect. It cannot encode direct permission for a
/// tool/action/context mutation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
