//! Model-backed checkpoint compaction for the runtime.
//!
//! Compaction is a summarization turn outside the agent loop: the runtime reads
//! the session's own history, asks the compaction model for one structured
//! checkpoint, and installs it transactionally. The work is split by
//! responsibility:
//!
//! - this module owns the shared request types and the session preparation that
//!   turns runtime state into a [`CompactionPreparation`];
//! - [`prefix`] compiles the stable prefix a compaction request shares with the
//!   agent loop;
//! - [`fit`] sizes and compiles one request against the compaction model window;
//! - [`plan`] picks a covered window the window can host;
//! - [`generate`] generates a candidate and installs it;
//! - [`install`] owns the installation transaction;
//! - [`manual`] serves an explicit caller request;
//! - [`phase`] drives the automatic hard-watermark path for one provider step.

use super::{RuntimeInner, provider_request::resolve_request_context_window};
use crate::{
    CitationCompactionInput, CitationCompactionPolicy, CompactionError,
    ResolvedCitationCompactionBudget, ResolvedContextWindow, RuntimeError, RuntimeModelRole,
    compaction::{CompactionPreparation, CompactionWindowBudget},
};

mod fit;
mod generate;
mod install;
mod manual;
mod phase;
mod plan;
mod prefix;

pub(super) use phase::{
    HardWatermarkCompaction, HardWatermarkOutcome, reduce_context_at_hard_watermark,
};

pub(super) use generate::generate_and_install_compaction;
pub(super) use install::{
    install_archive_only_compaction_transactionally,
    install_citation_compaction_candidate_transactionally,
};
pub(super) use manual::compact_context_once_inner;
pub(super) use plan::plan_compaction_attempt;
pub(super) use prefix::compaction_stable_prefix;

pub(super) async fn compaction_preparation_for_hard_watermark(
    inner: &RuntimeInner,
    policy: CitationCompactionPolicy,
    resolved_budget: ResolvedCitationCompactionBudget,
    window_budget: CompactionWindowBudget,
    primary_window_tokens: u64,
) -> Result<Option<(CompactionPreparation, CompactionRequestBudget)>, RuntimeError> {
    let session = inner.session.lock().await;
    let preparation = session.build_compaction_preparation_with_window_budget(
        policy,
        resolved_budget,
        window_budget,
        crate::compaction::CompactionCoverageBudget::unbounded(),
    )?;
    Ok(preparation.map(|preparation| {
        (
            preparation,
            CompactionRequestBudget {
                policy,
                resolved_budget,
                window_budget,
                primary_window_tokens,
            },
        )
    }))
}

pub(super) async fn compaction_input_for_policy(
    inner: &RuntimeInner,
    policy: CitationCompactionPolicy,
) -> Result<Option<CitationCompactionInput>, RuntimeError> {
    let primary_window = resolved_primary_context_window(inner).await?;
    build_compaction_input(inner, policy, primary_window).await
}

async fn build_compaction_input(
    inner: &RuntimeInner,
    policy: CitationCompactionPolicy,
    primary_window: ResolvedContextWindow,
) -> Result<Option<CitationCompactionInput>, RuntimeError> {
    let resolved_budget = policy.resolve(primary_window.tokens())?;
    let session = inner.session.lock().await;
    session.build_citation_compaction_input(policy, resolved_budget)
}

async fn resolved_primary_context_window(
    inner: &RuntimeInner,
) -> Result<ResolvedContextWindow, RuntimeError> {
    let provider_config = inner.model_config(RuntimeModelRole::Primary).await.ok_or(
        RuntimeError::MissingModelProvider {
            role: RuntimeModelRole::Primary.as_str(),
        },
    )?;
    let context_window_override = inner
        .context_window_tokens
        .read()
        .await
        .map(std::num::NonZeroU64::get);
    resolve_request_context_window(
        provider_config.provider().capabilities(),
        context_window_override,
    )
    .map_err(RuntimeError::from)
}

/// Parameters the runtime keeps so it can rebuild a compaction request under a budget.
pub(super) struct CompactionRequestBudget {
    pub(super) policy: CitationCompactionPolicy,
    pub(super) resolved_budget: ResolvedCitationCompactionBudget,
    pub(super) window_budget: CompactionWindowBudget,
    pub(super) primary_window_tokens: u64,
}

/// A compaction request that already fits the compaction model window.
pub(super) struct CompactionPlan {
    pub(super) input: Box<CitationCompactionInput>,
    pub(super) request: Box<merry_llm::ModelRequest>,
    /// Reasoning allowance this request was sized with.
    pub(super) reserve: crate::compaction::CompactionReasoningReserve,
}

/// Why one prepared compaction will not replace the checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ArchiveOnlyReason {
    /// The planner itself found no covered window to replace.
    PlanChoseArchiveOnly,
    /// No covered window fit the compaction request budget.
    BudgetExhausted {
        /// Measured input of the smallest request the runtime could build.
        estimated_input_tokens: u64,
        /// Output the window could not afford on top of that input.
        max_output_tokens: u64,
        /// Compaction model window that was too small.
        compactor_window_tokens: u64,
    },
}

impl ArchiveOnlyReason {
    /// Returns the budget failure this degradation ran into, when there was one.
    pub(super) fn budget_failure(self) -> Option<RuntimeError> {
        match self {
            Self::PlanChoseArchiveOnly => None,
            Self::BudgetExhausted {
                estimated_input_tokens,
                max_output_tokens,
                compactor_window_tokens,
            } => Some(RuntimeError::CompactionModelRequestTooLarge {
                estimated_input_tokens,
                max_output_tokens,
                compactor_window_tokens,
            }),
        }
    }
}

/// What the runtime should do for one prepared compaction.
pub(super) enum CompactionAttempt {
    /// The fitted request fits the compaction model window and its reserve.
    Generate(CompactionPlan),
    /// No checkpoint replacement fits; archive tool results without a model call.
    ArchiveOnly {
        input: crate::compaction::ArchiveOnlyCompactionInput,
        reason: ArchiveOnlyReason,
    },
}

/// Builds the error for work that stopped because its caller cancelled.
fn compaction_cancelled_before_request() -> RuntimeError {
    RuntimeError::Compaction {
        source: CompactionError::InvalidModelResponseShape {
            reason: "compaction cancelled before model request",
        },
    }
}
