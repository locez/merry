//! Internal advisory judgment boundary.
//!
//! Judgment is a semantic signal source for runtime-owned decisions, not a
//! runtime policy authority. Hard runtime policy still decides whether tools,
//! actions, or context mutations are allowed. Provider wire formats must not
//! enter this module; provider crates adapt external APIs into Merry-owned
//! traits and values before runtime sees them.
//!
//! Summaries remain navigation only. Any exact evidence used to draft or assess
//! a summary must remain available through artifact-backed evidence references,
//! and a judgment outcome never replaces those artifacts.
//!
//! Completed judgments are recorded in a crate-internal audit registry. That
//! registry uses internal artifacts for exact request/outcome payloads; it does
//! not claim public runtime artifacts, emit events, or append ledger facts.

mod audit;
mod core;
mod error;
mod payload;
mod source;
mod summary_draft;
mod tool_risk_review;

pub(crate) use self::{
    audit::{JudgmentRecord, JudgmentRecordId, JudgmentRegistry},
    core::{JudgmentEvidence, JudgmentOutcome, JudgmentRequest},
    error::JudgmentError,
    source::{JudgmentContext, JudgmentSource},
    summary_draft::{
        SummaryDraftAcceptance, SummaryDraftAcceptanceAuthority, SummaryDraftPromotionError,
        SummaryDraftPromotionInput, context_summary_from_accepted_summary_draft,
        validate_summary_draft_record_purpose,
    },
};

#[cfg(test)]
pub(crate) use self::{
    core::{
        JudgmentConfidence, JudgmentProvenance, JudgmentPurpose, JudgmentRecommendation,
        JudgmentRiskLevel, JudgmentSourceKind,
    },
    source::{JudgmentFuture, NoopJudgmentSource},
    tool_risk_review::{
        MODEL_BACKED_JUDGMENT_MAX_OUTPUT_TOKENS, MODEL_JUDGMENT_OUTPUT_SCHEMA_VERSION,
        MODEL_JUDGMENT_TOOL_RISK_EXPECTED_RISK, ModelBackedJudgmentSource,
        parse_tool_risk_review_model_judgment_output,
    },
};

#[cfg(test)]
mod tests;
