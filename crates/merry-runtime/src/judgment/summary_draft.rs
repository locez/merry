#![cfg_attr(not(test), allow(dead_code))]

use super::{
    audit::JudgmentRecordId,
    core::{
        JudgmentEvidence, JudgmentOutcome, JudgmentPurpose, JudgmentRecommendation, JudgmentRequest,
    },
    error::JudgmentError,
    payload::canonicalize_label_text,
};
use crate::context::{ContextError, ContextEvidence, ContextSummary};
use merry_core::{ArtifactId, EvidenceLocator};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Authority allowed to explicitly accept a summary draft for context promotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SummaryDraftAcceptanceAuthority {
    /// Runtime hard policy accepted the promotion.
    HardPolicy,
    /// A human explicitly accepted the promotion.
    Human,
    /// A deterministic, non-LLM review accepted the promotion.
    DeterministicReview,
}

/// Explicit acceptance required before a summary draft can become context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

    #[cfg(test)]
    pub(super) fn new_unchecked_for_test(
        summary_id: impl Into<String>,
        draft_text: impl Into<String>,
        selected_evidence: Vec<JudgmentEvidence>,
        acceptance: SummaryDraftAcceptance,
        source_record_id: Option<JudgmentRecordId>,
    ) -> Self {
        Self {
            summary_id: summary_id.into(),
            draft_text: draft_text.into(),
            selected_evidence,
            acceptance,
            source_record_id,
        }
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
