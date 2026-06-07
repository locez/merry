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
//!
//! Completed judgments are recorded in a crate-internal audit registry. That
//! registry uses internal artifacts for exact request/outcome payloads; it does
//! not claim public runtime artifacts, emit events, or append ledger facts.

// Staged internal judgment types are compiled before runtime call paths are wired.
#![cfg_attr(not(test), allow(dead_code))]

use crate::{
    artifact::ArtifactError,
    context::{ContextError, ContextEvidence, ContextSummary},
};
use merry_core::{ArtifactId, EvidenceLocator, EvidenceRef};
use merry_llm::ProviderErrorKind;
use std::{
    collections::BTreeMap,
    fmt::{self, Write as _},
};
use thiserror::Error;

mod source;
mod tool_risk_review;

pub(crate) use self::source::{JudgmentContext, JudgmentSource};

#[cfg(test)]
pub(crate) use self::{
    source::{JudgmentFuture, NoopJudgmentSource},
    tool_risk_review::{
        MODEL_BACKED_JUDGMENT_MAX_OUTPUT_TOKENS, MODEL_JUDGMENT_OUTPUT_SCHEMA_VERSION,
        MODEL_JUDGMENT_TOOL_RISK_EXPECTED_RISK, ModelBackedJudgmentSource,
        parse_tool_risk_review_model_judgment_output,
    },
};

const JUDGMENT_PAYLOAD_SCHEMA_VERSION: &str = "merry.judgment.audit.v1";
const JUDGMENT_RECORD_ID_PREFIX: &str = "judgment-record-";
const JUDGMENT_RECORD_ID_ORDER_DIGITS: usize = 20;

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

    fn as_str(self) -> &'static str {
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

impl JudgmentSourceKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Deterministic => "deterministic",
            Self::Llm => "llm",
            Self::Human => "human",
            Self::Test => "test",
        }
    }
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

impl JudgmentRiskLevel {
    fn as_str(self) -> &'static str {
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

    fn as_str(&self) -> &'static str {
        match self {
            Self::MemoryRelevant => "memory_relevant",
            Self::MemoryNotRelevant => "memory_not_relevant",
            Self::SummaryDraft { .. } => "summary_draft",
            Self::ToolRiskReview { .. } => "tool_risk_review",
            Self::NoRecommendation => "no_recommendation",
        }
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

/// Validated internal identifier for completed judgment audit records.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct JudgmentRecordId(String);

impl JudgmentRecordId {
    pub(crate) fn new(value: &str) -> Result<Self, JudgmentError> {
        validate_record_id(value)?;
        Ok(Self(value.to_owned()))
    }

    fn generated(order: u64) -> Self {
        Self(format!(
            "{JUDGMENT_RECORD_ID_PREFIX}{order:0JUDGMENT_RECORD_ID_ORDER_DIGITS$}"
        ))
    }

    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for JudgmentRecordId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Internal artifact identifier for judgment audit payloads.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct JudgmentInternalArtifactId(String);

impl JudgmentInternalArtifactId {
    fn for_record(record_id: &JudgmentRecordId, kind: JudgmentInternalArtifactKind) -> Self {
        Self(format!("{}-{}", record_id.as_str(), kind.as_str()))
    }

    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for JudgmentInternalArtifactId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JudgmentInternalArtifactKind {
    Request,
    Outcome,
}

impl JudgmentInternalArtifactKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Outcome => "outcome",
        }
    }
}

/// Internal exact payload artifact for judgment audit records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JudgmentInternalArtifact {
    id: JudgmentInternalArtifactId,
    content: String,
}

impl JudgmentInternalArtifact {
    fn new(id: JudgmentInternalArtifactId, content: String) -> Self {
        Self { id, content }
    }

    #[must_use]
    pub(crate) fn id(&self) -> &JudgmentInternalArtifactId {
        &self.id
    }

    #[must_use]
    pub(crate) fn content(&self) -> &str {
        &self.content
    }
}

/// Internal request/outcome artifact pair for a completed judgment record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JudgmentRecordArtifacts {
    request: JudgmentInternalArtifact,
    outcome: JudgmentInternalArtifact,
}

impl JudgmentRecordArtifacts {
    fn new(
        record_id: &JudgmentRecordId,
        order: u64,
        request: &JudgmentRequest,
        outcome: &JudgmentOutcome,
    ) -> Self {
        let request = JudgmentInternalArtifact::new(
            JudgmentInternalArtifactId::for_record(
                record_id,
                JudgmentInternalArtifactKind::Request,
            ),
            render_request_payload(record_id, order, request),
        );
        let outcome = JudgmentInternalArtifact::new(
            JudgmentInternalArtifactId::for_record(
                record_id,
                JudgmentInternalArtifactKind::Outcome,
            ),
            render_outcome_payload(record_id, order, outcome),
        );

        Self { request, outcome }
    }

    #[must_use]
    pub(crate) fn request(&self) -> &JudgmentInternalArtifact {
        &self.request
    }

    #[must_use]
    pub(crate) fn outcome(&self) -> &JudgmentInternalArtifact {
        &self.outcome
    }
}

/// Completed internal advisory judgment audit record.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct JudgmentRecord {
    id: JudgmentRecordId,
    request: JudgmentRequest,
    outcome: JudgmentOutcome,
    artifacts: JudgmentRecordArtifacts,
    commit_order: u64,
}

impl JudgmentRecord {
    #[must_use]
    pub(crate) fn id(&self) -> &JudgmentRecordId {
        &self.id
    }

    #[must_use]
    pub(crate) fn request(&self) -> &JudgmentRequest {
        &self.request
    }

    #[must_use]
    pub(crate) fn outcome(&self) -> &JudgmentOutcome {
        &self.outcome
    }

    #[must_use]
    pub(crate) fn artifacts(&self) -> &JudgmentRecordArtifacts {
        &self.artifacts
    }

    #[must_use]
    pub(crate) fn commit_order(&self) -> u64 {
        self.commit_order
    }
}

/// Deterministic snapshot of completed judgment audit records.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct JudgmentRegistrySnapshot {
    records: Vec<JudgmentRecord>,
}

impl JudgmentRegistrySnapshot {
    #[must_use]
    pub(crate) fn records(&self) -> &[JudgmentRecord] {
        &self.records
    }
}

/// Crate-internal completed judgment audit registry.
#[derive(Debug, Clone, Default)]
pub(crate) struct JudgmentRegistry {
    records: BTreeMap<JudgmentRecordId, JudgmentRecord>,
    next_order: u64,
}

impl JudgmentRegistry {
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub(crate) fn record_completed(
        &mut self,
        request: JudgmentRequest,
        outcome: JudgmentOutcome,
    ) -> Result<JudgmentRecord, JudgmentError> {
        let id = self.next_generated_id();
        self.record_completed_with_id(id, request, outcome)
    }

    pub(crate) fn record_completed_with_id(
        &mut self,
        id: JudgmentRecordId,
        request: JudgmentRequest,
        outcome: JudgmentOutcome,
    ) -> Result<JudgmentRecord, JudgmentError> {
        if self.records.contains_key(&id) {
            return Err(JudgmentError::DuplicateRecordId { id });
        }

        validate_record_purpose(&request, &outcome)?;

        let commit_order = self.next_order;
        let artifacts = JudgmentRecordArtifacts::new(&id, commit_order, &request, &outcome);
        let record = JudgmentRecord {
            id: id.clone(),
            request,
            outcome,
            artifacts,
            commit_order,
        };

        self.records.insert(id, record.clone());
        self.next_order += 1;
        Ok(record)
    }

    #[must_use]
    pub(crate) fn snapshot(&self) -> JudgmentRegistrySnapshot {
        let mut records = self.records.values().cloned().collect::<Vec<_>>();
        records.sort_by(|left, right| {
            left.commit_order()
                .cmp(&right.commit_order())
                .then_with(|| left.id().cmp(right.id()))
        });

        JudgmentRegistrySnapshot { records }
    }

    fn next_generated_id(&self) -> JudgmentRecordId {
        let mut order = self.next_order;
        loop {
            let id = JudgmentRecordId::generated(order);
            if !self.records.contains_key(&id) {
                return id;
            }

            order = order
                .checked_add(1)
                .expect("judgment record id space is exhausted");
        }
    }
}

/// Authority allowed to explicitly accept a summary draft for context promotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SummaryDraftAcceptanceAuthority {
    /// Runtime hard policy accepted the promotion.
    HardPolicy,
    /// A human explicitly accepted the promotion.
    Human,
    /// A deterministic, non-LLM review accepted the promotion.
    DeterministicReview,
}

/// Explicit acceptance required before a summary draft can become context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SummaryDraftAcceptance {
    authority: SummaryDraftAcceptanceAuthority,
    source_label: String,
    rationale: String,
}

impl SummaryDraftAcceptance {
    pub(crate) fn new(
        authority: SummaryDraftAcceptanceAuthority,
        source_label: impl Into<String>,
        rationale: impl Into<String>,
    ) -> Result<Self, SummaryDraftPromotionError> {
        let source_label = source_label.into();
        validate_promotion_non_blank("summary draft acceptance source label", &source_label)?;

        let rationale = rationale.into();
        validate_promotion_non_blank("summary draft acceptance rationale", &rationale)?;

        Ok(Self {
            authority,
            source_label: canonicalize_label_text(&source_label),
            rationale,
        })
    }

    #[must_use]
    pub(crate) fn authority(&self) -> SummaryDraftAcceptanceAuthority {
        self.authority
    }

    #[must_use]
    pub(crate) fn source_label(&self) -> &str {
        &self.source_label
    }

    #[must_use]
    pub(crate) fn rationale(&self) -> &str {
        &self.rationale
    }
}

/// Explicit input for turning an accepted summary draft into context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SummaryDraftPromotionInput {
    summary_id: String,
    draft_text: String,
    selected_evidence: Vec<JudgmentEvidence>,
    acceptance: SummaryDraftAcceptance,
    source_record_id: Option<JudgmentRecordId>,
}

impl SummaryDraftPromotionInput {
    pub(crate) fn new(
        summary_id: impl Into<String>,
        draft_text: impl Into<String>,
        selected_evidence: Vec<JudgmentEvidence>,
        acceptance: SummaryDraftAcceptance,
        source_record_id: Option<JudgmentRecordId>,
    ) -> Result<Self, SummaryDraftPromotionError> {
        let summary_id = summary_id.into();
        validate_promotion_non_blank("summary draft promotion summary id", &summary_id)?;

        let draft_text = draft_text.into();
        validate_promotion_non_blank("summary draft promotion draft text", &draft_text)?;

        if selected_evidence.is_empty() {
            return Err(SummaryDraftPromotionError::EmptySelectedEvidence);
        }

        Ok(Self {
            summary_id,
            draft_text,
            selected_evidence,
            acceptance,
            source_record_id,
        })
    }

    #[must_use]
    pub(crate) fn summary_id(&self) -> &str {
        &self.summary_id
    }

    #[must_use]
    pub(crate) fn draft_text(&self) -> &str {
        &self.draft_text
    }

    #[must_use]
    pub(crate) fn selected_evidence(&self) -> &[JudgmentEvidence] {
        &self.selected_evidence
    }

    #[must_use]
    pub(crate) fn acceptance(&self) -> &SummaryDraftAcceptance {
        &self.acceptance
    }

    #[must_use]
    pub(crate) fn source_record_id(&self) -> Option<&JudgmentRecordId> {
        self.source_record_id.as_ref()
    }
}

/// Errors raised while explicitly promoting an accepted summary draft to context.
#[derive(Debug, PartialEq, Eq, Error)]
pub(crate) enum SummaryDraftPromotionError {
    /// A required promotion text field was blank.
    #[error("{field} must not be blank")]
    BlankField {
        /// Name of the invalid field.
        field: &'static str,
    },

    /// Promotion was attempted for a non-summary-draft request or outcome.
    #[error("{field} requires summary draft judgment purpose, got {actual_purpose}")]
    SummaryDraftPurposeRequired {
        /// Name of the rejected input field.
        field: &'static str,
        /// Rejected judgment purpose.
        actual_purpose: JudgmentPurpose,
    },

    /// A summary draft outcome produced no promotable recommendation.
    #[error(
        "summary draft promotion requires a summary draft recommendation, got no recommendation"
    )]
    NoRecommendation,

    /// A summary draft outcome carried an unsupported recommendation variant.
    #[error(
        "summary draft promotion requires a summary draft recommendation, got {recommendation}"
    )]
    SummaryDraftRecommendationRequired {
        /// Rejected recommendation kind.
        recommendation: &'static str,
    },

    /// Accepted text did not exactly match the draft recommended by the judgment.
    #[error("accepted summary draft text does not exactly match judgment recommendation")]
    DraftMismatch {
        /// Draft recommended by the judgment outcome.
        recommended: String,
        /// Draft supplied for promotion.
        accepted: String,
    },

    /// Promotion selected no exact evidence references.
    #[error("summary draft promotion requires at least one selected exact evidence reference")]
    EmptySelectedEvidence,

    /// Selected evidence did not come from the request/outcome evidence union.
    #[error("summary draft selected evidence was not present in request or outcome evidence")]
    SelectedEvidenceNotInJudgment {
        /// Artifact identifier from the rejected evidence reference.
        artifact_id: ArtifactId,
        /// Locator from the rejected evidence reference.
        locator: EvidenceLocator,
    },

    /// A context summary id already exists in the owning session.
    #[error("context summary id {summary_id} already exists")]
    DuplicateSummaryId {
        /// Duplicate context summary identifier.
        summary_id: String,
    },

    /// A promotion for this summary id was already recorded with different input.
    #[error(
        "summary draft promotion for context summary id {summary_id} conflicts with an existing promotion payload"
    )]
    PromotionPayloadConflict {
        /// Context summary identifier whose promotion payload conflicted.
        summary_id: String,
    },

    /// A promotion for this summary id was already rejected and cannot be retried.
    #[error("summary draft promotion for context summary id {summary_id} was already rejected")]
    PromotionAlreadyRejected {
        /// Context summary identifier whose prior exact promotion was rejected.
        summary_id: String,
    },

    /// Context construction or compilation rejected the promoted summary.
    #[error("summary draft promotion failed context validation: {source}")]
    Context {
        /// Source context validation error.
        #[from]
        source: ContextError,
    },
}

pub(crate) fn context_summary_from_accepted_summary_draft(
    request: &JudgmentRequest,
    outcome: &JudgmentOutcome,
    input: &SummaryDraftPromotionInput,
) -> Result<ContextSummary, SummaryDraftPromotionError> {
    if request.purpose() != JudgmentPurpose::SummaryDraft {
        return Err(SummaryDraftPromotionError::SummaryDraftPurposeRequired {
            field: "judgment request",
            actual_purpose: request.purpose(),
        });
    }

    if outcome.purpose() != JudgmentPurpose::SummaryDraft {
        return Err(SummaryDraftPromotionError::SummaryDraftPurposeRequired {
            field: "judgment outcome",
            actual_purpose: outcome.purpose(),
        });
    }

    let recommended_draft = match outcome.recommendation() {
        JudgmentRecommendation::SummaryDraft { draft } => draft.as_str(),
        JudgmentRecommendation::NoRecommendation => {
            return Err(SummaryDraftPromotionError::NoRecommendation);
        }
        recommendation => {
            return Err(
                SummaryDraftPromotionError::SummaryDraftRecommendationRequired {
                    recommendation: recommendation.kind(),
                },
            );
        }
    };

    if input.draft_text() != recommended_draft {
        return Err(SummaryDraftPromotionError::DraftMismatch {
            recommended: recommended_draft.to_owned(),
            accepted: input.draft_text().to_owned(),
        });
    }

    if input.selected_evidence().is_empty() {
        return Err(SummaryDraftPromotionError::EmptySelectedEvidence);
    }

    let evidence = input
        .selected_evidence()
        .iter()
        .map(|selected| {
            let evidence =
                find_matching_judgment_evidence(request, outcome, selected).ok_or_else(|| {
                    SummaryDraftPromotionError::SelectedEvidenceNotInJudgment {
                        artifact_id: selected.reference().artifact_id.clone(),
                        locator: selected.reference().locator.clone(),
                    }
                })?;

            Ok(ContextEvidence::new(
                evidence.label(),
                evidence.reference().clone(),
            )?)
        })
        .collect::<Result<Vec<_>, SummaryDraftPromotionError>>()?;

    Ok(ContextSummary::new(
        input.summary_id(),
        recommended_draft,
        evidence,
    )?)
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

    /// Completed record request and outcome purposes did not match.
    #[error(
        "judgment outcome purpose {outcome_purpose} does not match request purpose {request_purpose}"
    )]
    RecordPurposeMismatch {
        /// Request purpose.
        request_purpose: JudgmentPurpose,
        /// Outcome purpose.
        outcome_purpose: JudgmentPurpose,
    },

    /// A narrow internal recording helper received an unsupported judgment purpose.
    #[error("{field} requires summary draft judgment purpose, got {actual_purpose}")]
    SummaryDraftPurposeRequired {
        /// Name of the rejected input field.
        field: &'static str,
        /// Rejected judgment purpose.
        actual_purpose: JudgmentPurpose,
    },

    /// The strict model judgment parser received an unsupported request purpose.
    #[error(
        "model judgment output parser requires tool risk review judgment purpose, got {actual_purpose}"
    )]
    ModelJudgmentPurposeRequired {
        /// Rejected judgment purpose.
        actual_purpose: JudgmentPurpose,
    },

    /// Model judgment output was not one strict JSON object in the expected schema.
    #[error("model judgment output must be one strict JSON object matching the expected schema")]
    InvalidModelJudgmentOutput,

    /// Model judgment output used an unsupported literal field value.
    #[error("model judgment output field {field} must be {expected}, got {actual:?}")]
    InvalidModelJudgmentLiteral {
        /// Name of the invalid model output field.
        field: &'static str,
        /// Expected literal value.
        expected: &'static str,
        /// Rejected value.
        actual: String,
    },

    /// Model judgment output cited evidence that was not supplied by the request.
    #[error("model judgment output evidence index {index} is outside request evidence")]
    ModelJudgmentEvidenceIndexOutOfRange {
        /// Rejected evidence citation index.
        index: usize,
    },

    /// Model judgment output cited the same request evidence more than once.
    #[error("model judgment output evidence index {index} is cited more than once")]
    DuplicateModelJudgmentEvidenceCitation {
        /// Duplicate evidence citation index.
        index: usize,
    },

    /// Model judgment output cited request evidence with the wrong label.
    #[error(
        "model judgment output evidence index {index} label must exactly match request evidence label {expected:?}, got {actual:?}"
    )]
    ModelJudgmentEvidenceLabelMismatch {
        /// Evidence citation index whose label did not match.
        index: usize,
        /// Request-owned evidence label.
        expected: String,
        /// Model-supplied label.
        actual: String,
    },

    /// Model-backed judgment request compilation failed before provider setup.
    #[error("model judgment request could not be compiled ({kind:?}): {message}")]
    ModelJudgmentRequest {
        /// Provider-neutral error category.
        kind: ProviderErrorKind,
        /// Actionable provider-neutral error message.
        message: String,
    },

    /// Model-backed judgment provider setup failed.
    #[error("model judgment provider setup failed ({kind:?}): {message}")]
    ModelJudgmentProviderSetup {
        /// Provider-neutral error category.
        kind: ProviderErrorKind,
        /// Actionable provider-neutral error message.
        message: String,
    },

    /// Model-backed judgment provider stream failed.
    #[error("model judgment provider stream failed ({kind:?}): {message}")]
    ModelJudgmentProviderStream {
        /// Provider-neutral error category.
        kind: ProviderErrorKind,
        /// Actionable provider-neutral error message.
        message: String,
    },

    /// Model-backed judgment response did not match the accepted stream shape.
    #[error("model judgment response shape is unsupported: {reason}")]
    InvalidModelJudgmentResponseShape {
        /// Stable reason for the rejected provider-neutral response shape.
        reason: &'static str,
    },

    /// Internal judgment record id was invalid.
    #[error("judgment record id {value:?} is invalid: {reason}")]
    InvalidRecordId {
        /// Rejected identifier text.
        value: String,
        /// Actionable reason.
        reason: &'static str,
    },

    /// Internal judgment record id already exists.
    #[error("judgment record id {id} is already recorded")]
    DuplicateRecordId {
        /// Duplicate record identifier.
        id: JudgmentRecordId,
    },

    /// Judgment evidence could not be read from the session artifact registry.
    #[error("judgment evidence artifact {artifact_id} is unreadable: {source}")]
    UnreadableEvidence {
        /// Artifact identifier referenced by unreadable evidence.
        artifact_id: ArtifactId,
        /// Artifact registry read/locator failure.
        source: ArtifactError,
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

fn validate_record_purpose(
    request: &JudgmentRequest,
    outcome: &JudgmentOutcome,
) -> Result<(), JudgmentError> {
    if request.purpose() != outcome.purpose() {
        return Err(JudgmentError::RecordPurposeMismatch {
            request_purpose: request.purpose(),
            outcome_purpose: outcome.purpose(),
        });
    }

    Ok(())
}

pub(crate) fn validate_summary_draft_record_purpose(
    request: &JudgmentRequest,
    outcome: &JudgmentOutcome,
) -> Result<(), JudgmentError> {
    if request.purpose() != JudgmentPurpose::SummaryDraft {
        return Err(JudgmentError::SummaryDraftPurposeRequired {
            field: "judgment request",
            actual_purpose: request.purpose(),
        });
    }

    if outcome.purpose() != JudgmentPurpose::SummaryDraft {
        return Err(JudgmentError::SummaryDraftPurposeRequired {
            field: "judgment outcome",
            actual_purpose: outcome.purpose(),
        });
    }

    Ok(())
}

fn find_matching_judgment_evidence<'a>(
    request: &'a JudgmentRequest,
    outcome: &'a JudgmentOutcome,
    selected: &JudgmentEvidence,
) -> Option<&'a JudgmentEvidence> {
    request
        .evidence()
        .iter()
        .chain(outcome.evidence())
        .find(|evidence| *evidence == selected)
}

fn validate_promotion_non_blank(
    field: &'static str,
    value: &str,
) -> Result<(), SummaryDraftPromotionError> {
    if value.trim().is_empty() {
        return Err(SummaryDraftPromotionError::BlankField { field });
    }

    Ok(())
}

fn validate_record_id(value: &str) -> Result<(), JudgmentError> {
    if value.is_empty() {
        return Err(invalid_record_id(value, "must not be empty"));
    }

    if value.trim().is_empty() {
        return Err(invalid_record_id(value, "must not be whitespace only"));
    }

    if value.trim() != value {
        return Err(invalid_record_id(
            value,
            "must not have leading or trailing whitespace",
        ));
    }

    if value.chars().count() > 128 {
        return Err(invalid_record_id(
            value,
            "is longer than the allowed maximum length",
        ));
    }

    if value.chars().any(char::is_control) {
        return Err(invalid_record_id(
            value,
            "must not contain control characters",
        ));
    }

    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(invalid_record_id(
            value,
            "must contain only ASCII letters, digits, '-', '_' or '.'",
        ));
    }

    Ok(())
}

fn invalid_record_id(value: &str, reason: &'static str) -> JudgmentError {
    JudgmentError::InvalidRecordId {
        value: value.to_owned(),
        reason,
    }
}

fn render_request_payload(
    record_id: &JudgmentRecordId,
    order: u64,
    request: &JudgmentRequest,
) -> String {
    let mut payload = String::new();
    push_field(
        &mut payload,
        "schema_version",
        JUDGMENT_PAYLOAD_SCHEMA_VERSION,
    );
    push_field(&mut payload, "artifact", "request");
    push_field(&mut payload, "record_id", record_id.as_str());
    push_field(&mut payload, "commit_order", &order.to_string());
    push_field(&mut payload, "purpose", request.purpose().as_str());
    push_field(&mut payload, "subject", request.subject());
    push_field(&mut payload, "input", request.input());
    push_field(&mut payload, "source_label", request.source_label());
    push_list(&mut payload, "constraints", request.constraints());
    push_evidence(&mut payload, "evidence", request.evidence());
    payload
}

fn render_outcome_payload(
    record_id: &JudgmentRecordId,
    order: u64,
    outcome: &JudgmentOutcome,
) -> String {
    let mut payload = String::new();
    push_field(
        &mut payload,
        "schema_version",
        JUDGMENT_PAYLOAD_SCHEMA_VERSION,
    );
    push_field(&mut payload, "artifact", "outcome");
    push_field(&mut payload, "record_id", record_id.as_str());
    push_field(&mut payload, "commit_order", &order.to_string());
    push_field(&mut payload, "purpose", outcome.purpose().as_str());
    push_recommendation(&mut payload, outcome.recommendation());
    push_field(
        &mut payload,
        "confidence",
        &format!("{:.6}", outcome.confidence().as_f32()),
    );
    push_evidence(&mut payload, "evidence", outcome.evidence());
    push_field(&mut payload, "rationale", outcome.rationale());
    push_field(&mut payload, "uncertainty", outcome.uncertainty());
    push_field(
        &mut payload,
        "provenance.kind",
        outcome.provenance().source_kind().as_str(),
    );
    push_field(
        &mut payload,
        "provenance.label",
        outcome.provenance().source_label(),
    );
    payload
}

fn push_recommendation(payload: &mut String, recommendation: &JudgmentRecommendation) {
    push_field(payload, "recommendation.kind", recommendation.as_str());

    match recommendation {
        JudgmentRecommendation::SummaryDraft { draft } => {
            push_field(payload, "recommendation.draft", draft);
        }
        JudgmentRecommendation::ToolRiskReview { risk, concerns } => {
            push_field(payload, "recommendation.risk", risk.as_str());
            push_list(payload, "recommendation.concerns", concerns);
        }
        JudgmentRecommendation::MemoryRelevant
        | JudgmentRecommendation::MemoryNotRelevant
        | JudgmentRecommendation::NoRecommendation => {}
    }
}

fn push_list(payload: &mut String, name: &str, values: &[String]) {
    push_field(payload, &format!("{name}.count"), &values.len().to_string());
    for (index, value) in values.iter().enumerate() {
        push_field(payload, &format!("{name}.{index}"), value);
    }
}

fn push_evidence(payload: &mut String, name: &str, evidence: &[JudgmentEvidence]) {
    push_field(
        payload,
        &format!("{name}.count"),
        &evidence.len().to_string(),
    );
    for (index, item) in evidence.iter().enumerate() {
        push_field(payload, &format!("{name}.{index}.label"), item.label());
        push_field(
            payload,
            &format!("{name}.{index}.artifact_id"),
            item.reference().artifact_id.as_str(),
        );
        push_field(
            payload,
            &format!("{name}.{index}.locator"),
            &format_locator(&item.reference().locator),
        );
    }
}

fn push_field(payload: &mut String, key: &str, value: &str) {
    writeln!(payload, "{key}={}", escape_payload_value(value))
        .expect("writing to a String cannot fail");
}

fn escape_payload_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());

    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            other => escaped.push(other),
        }
    }

    escaped
}

fn format_locator(locator: &EvidenceLocator) -> String {
    if locator.is_whole_artifact() {
        return "whole".to_owned();
    }

    if let Some((start, end)) = locator.as_line_range() {
        return format!("line:{start}-{end}");
    }

    if let Some((start, end)) = locator.as_byte_range() {
        return format!("byte:{start}-{end}");
    }

    if let Some(pointer) = locator.as_json_pointer() {
        return format!("json:{pointer}");
    }

    if let Some(name) = locator.as_named_section() {
        return format!("section:{name}");
    }

    unreachable!("all evidence locator variants are covered by public accessors")
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
    use merry_core::{ArtifactId, EvidenceLocator, ProviderName, ToolName};
    use merry_llm::{
        FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream,
        ModelMessageRole, ModelName, ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest,
        ModelResponse, ModelStreamContext, ModelToolCall, ModelToolCallId, ProviderErrorKind,
        ToolArguments, testing::FakeModelProvider,
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

        let unknown_recommendation_field =
            model_tool_risk_output_with_recommendation_extra(json!({
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
            parse_tool_risk_review_model_judgment_output(
                &unknown_risk,
                &request,
                "test llm source",
            )
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
            parse_tool_risk_review_model_judgment_output(
                &blank_concern,
                &request,
                "test llm source",
            )
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
            parse_tool_risk_review_model_judgment_output(
                &out_of_range,
                &request,
                "test llm source",
            )
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
            parse_tool_risk_review_model_judgment_output(
                &label_mismatch,
                &request,
                "test llm source",
            )
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
        assert!(
            outcome_payload.contains("recommendation.draft=Summary draft from exact evidence.\n")
        );
        assert!(outcome_payload.contains("confidence=0.750000\n"));
        assert!(
            outcome_payload.contains("rationale=The draft uses the supplied artifact evidence.\n")
        );
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
        assert!(
            user.contains("input=Review whether the pending tool request has semantic risk.\n")
        );
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
        let input = SummaryDraftPromotionInput {
            summary_id: "accepted-summary".to_owned(),
            draft_text: "Summary draft from exact evidence.".to_owned(),
            selected_evidence: Vec::new(),
            acceptance: acceptance(),
            source_record_id: None,
        };

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

    fn completed_outputs_event(
        outputs: Vec<ModelOutput>,
        finish_reason: FinishReason,
    ) -> ModelEvent {
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
}
