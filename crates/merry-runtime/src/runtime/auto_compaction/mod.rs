//! Model-backed checkpoint compaction for the runtime.
//!
//! Compaction is a summarization turn outside the agent loop: the runtime reads
//! the session's own history, asks the compaction model for one structured
//! checkpoint, and installs it transactionally. The work is split by
//! responsibility:
//!
//! - this module owns the shared request types and the session preparation that
//!   turns runtime state into a [`CompactionPreparation`];
//! - [`source`] reuses primary request compilation for manual compaction;
//!   automatic compaction carries the actual step request unchanged;
//! - [`fit`] sizes and compiles one request against the compaction model window;
//! - [`plan`] picks a covered window the window can host;
//! - [`generate`] generates a candidate and installs it;
//! - [`install`] owns the installation transaction;
//! - [`manual`] serves an explicit caller request;
//! - [`progress`] decides when rolling has reached the destination body target;
//! - [`phase`] drives the automatic hard-watermark path for one provider step.

use super::{
    RuntimeInner,
    provider_request::{CompactionFixedDynamicTokens, RequestContextBudget},
};
use crate::{
    CitationCompactionInput, CitationCompactionPolicy, CompactionError,
    ResolvedCitationCompactionBudget, RuntimeError,
    compaction::{
        CompactionCoverageBudget, CompactionPreparation, CompactionShape, CompactionWindowBudget,
    },
    context::compacted_checkpoint_wrapper_token_ceiling,
    session::SessionState,
};

mod fit;
mod generate;
mod install;
mod manual;
mod phase;
mod plan;
mod progress;
mod source;

pub(super) use progress::CompactionProgress;

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
use source::manual_compaction_budget;

pub(super) async fn compaction_preparation_for_budget(
    inner: &RuntimeInner,
    budget: &CompactionRequestBudget,
) -> Result<Option<CompactionPreparation>, RuntimeError> {
    let session = inner.session.lock().await;
    build_preparation_for_shape(
        &session,
        budget.policy,
        budget.resolved_budget,
        budget.window_budget,
        budget.shape,
        CompactionCoverageBudget::unbounded(),
    )
}

/// Builds the preparation one shape asks for.
///
/// Every shape covers the whole history before the retained tail except rolling,
/// which starts unbounded too and lets the fit loop lower the coverage when the
/// request cannot host it.
pub(super) fn build_preparation_for_shape(
    session: &SessionState,
    policy: CitationCompactionPolicy,
    resolved_budget: ResolvedCitationCompactionBudget,
    window_budget: CompactionWindowBudget,
    shape: CompactionShape,
    coverage: CompactionCoverageBudget,
) -> Result<Option<CompactionPreparation>, RuntimeError> {
    match shape {
        CompactionShape::OneShot {
            retained_tool_exchanges,
        } => session.build_one_shot_compaction_preparation(
            policy,
            resolved_budget,
            window_budget,
            coverage,
            retained_tool_exchanges,
        ),
        CompactionShape::RollingText => session.build_compaction_preparation(
            policy,
            resolved_budget,
            window_budget,
            coverage,
            shape,
        ),
        CompactionShape::Rolling => session.build_rolling_compaction_preparation(
            policy,
            resolved_budget,
            window_budget,
            coverage,
        ),
        CompactionShape::SinglePass => session.build_compaction_preparation_with_window_budget(
            policy,
            resolved_budget,
            window_budget,
            coverage,
        ),
    }
}

pub(super) async fn compaction_input_for_policy(
    inner: &RuntimeInner,
    policy: CitationCompactionPolicy,
) -> Result<Option<CitationCompactionInput>, RuntimeError> {
    let budget = manual_compaction_budget(inner, policy).await?;
    let session = inner.session.lock().await;
    session.build_citation_compaction_input_with_window_budget(
        policy,
        budget.resolved_budget,
        budget.window_budget,
        CompactionCoverageBudget::unbounded(),
    )
}

/// Parameters the runtime keeps so it can rebuild a compaction request under a budget.
pub(super) struct CompactionRequestBudget {
    pub(super) source: crate::compaction::CompactionRequestSource,
    pub(super) policy: CitationCompactionPolicy,
    pub(super) resolved_budget: ResolvedCitationCompactionBudget,
    pub(super) window_budget: CompactionWindowBudget,
    pub(super) primary_window_tokens: u64,
    pub(super) dynamic_body_estimated_tokens: u64,
    /// Initial coverage, before the measured request chooses a fallback.
    pub(super) shape: CompactionShape,
}

impl CompactionRequestBudget {
    /// Reserves the hard accepted summary and a bounded raw-tail target.
    /// The install-time body is that sum plus fixed context, not half the watermark.
    /// Both manual and automatic planning account for tools and output.
    pub(super) fn new(
        source: crate::compaction::CompactionRequestSource,
        policy: CitationCompactionPolicy,
        request_budget: &RequestContextBudget,
        fixed_dynamic_body_tokens: CompactionFixedDynamicTokens,
    ) -> Result<Self, CompactionError> {
        let primary_window_tokens = request_budget.window.tokens();
        let resolved_budget = policy.resolve(primary_window_tokens)?;
        let checkpoint_output_ceiling_tokens = resolved_budget
            .output_token_limit()
            .checked_add(compacted_checkpoint_wrapper_token_ceiling())
            .ok_or(CompactionError::BudgetOverflow)?;
        let window_budget = CompactionWindowBudget::new(
            primary_window_tokens,
            request_budget.budget.hard_water_tokens(),
            fixed_dynamic_body_tokens.replacement,
            fixed_dynamic_body_tokens.archive_only,
            checkpoint_output_ceiling_tokens,
        )?
        .with_retained_history_target(resolved_budget.retained_history_token_target())?;
        Ok(Self {
            source,
            policy,
            resolved_budget,
            window_budget,
            primary_window_tokens,
            dynamic_body_estimated_tokens: request_budget.dynamic_body_estimated_tokens,
            shape: CompactionShape::SinglePass,
        })
    }

    /// Returns the destination body budget the installed checkpoint should reach.
    pub(super) const fn target_dynamic_body_tokens(&self) -> u64 {
        self.window_budget.target_dynamic_body_tokens()
    }
}

/// A compaction request that already fits the compaction model window.
pub(super) struct CompactionPlan {
    pub(super) compactor_window_tokens: u64,
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
