//! The automatic hard-watermark compaction phase of one provider step.
//!
//! This module owns the whole decision for one step: estimate the fixed dynamic
//! body, build the window budget, prepare a compaction, fit a request the
//! compaction model window can host, and install either a checkpoint replacement
//! or an archive-only reduction. It emits the compaction lifecycle events, so the
//! caller only has to recompile its request and report a completed checkpoint.

use super::super::journal_emission::{
    send_cancelled_event, send_compaction_started_event, send_failed_event,
    trace_provider_step_cancelled, trace_provider_step_failed,
};
use super::super::memory_activation::clear_current_activated_memories;
use super::super::provider_request::{
    RequestContextBudget, StepRequestInputs, estimate_compaction_fixed_dynamic_tokens,
    step_request_compile_diagnostic,
};
use super::super::{RuntimeInner, diagnostic_from_text, runtime_error_message};
use super::{
    ArchiveOnlyReason, CompactionAttempt, CompactionRequestBudget,
    compaction_preparation_for_budget, generate_and_install_compaction,
    install_archive_only_compaction_transactionally, plan_compaction_attempt,
};
use crate::{
    CitationCompactionPolicy, CompactionError, CompactionOutcome,
    compaction::{ArchiveOnlyCompactionInput, CompactionPreparation},
    events::{ActiveStepPermit, RuntimeJournalEventBatch},
    step::StepInput,
};
use merry_core::{ErrorInfo, ToolSpec};
use merry_llm::{GenerationConfig, ModelName, ReasoningEffort};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Everything the compaction phase needs from the step that triggered it.
pub(in crate::runtime) struct HardWatermarkCompaction<'a> {
    /// Compaction policy for this step.
    pub(crate) policy: CitationCompactionPolicy,
    /// Reasoning level compaction requests use, resolved from the runtime config.
    pub(crate) reasoning_effort: Option<&'a ReasoningEffort>,
    /// Resolved budget of the request that crossed the hard watermark.
    pub(crate) request_budget: &'a RequestContextBudget,
    /// Current step input.
    pub(crate) input: &'a StepInput,
    /// Compiled request inputs of the current step.
    pub(crate) request_inputs: &'a StepRequestInputs,
    /// Tool specs of the current step.
    pub(crate) tool_specs: Vec<ToolSpec>,
    /// Generation controls of the current step.
    pub(crate) generation_config: GenerationConfig,
    /// Primary model, used to estimate the replacement request.
    pub(crate) primary_model: &'a ModelName,
    pub(crate) request: &'a merry_llm::ModelRequest,
}

/// Result of the hard-watermark compaction phase for one step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) enum HardWatermarkOutcome {
    /// The step continues, carrying the checkpoint replacement to report when there is one.
    Continue {
        /// Installed replacement, when compaction replaced the checkpoint.
        replacement: Option<CompactionOutcome>,
        /// Destination dynamic-body target for this compaction pass.
        target_dynamic_body_tokens: u64,
    },
    /// The phase already emitted the step's terminal event; the caller returns.
    Aborted,
}

/// Reduces context for one step that crossed the hard watermark.
pub(in crate::runtime) async fn reduce_context_at_hard_watermark(
    inner: &Arc<RuntimeInner>,
    sender: &mpsc::Sender<RuntimeJournalEventBatch>,
    token: &CancellationToken,
    active_permit: &ActiveStepPermit,
    parts: HardWatermarkCompaction<'_>,
) -> HardWatermarkOutcome {
    let HardWatermarkCompaction {
        policy,
        reasoning_effort,
        request_budget,
        input,
        request_inputs,
        tool_specs,
        generation_config,
        primary_model,
        request,
    } = parts;

    let fixed_dynamic_body_tokens = match estimate_compaction_fixed_dynamic_tokens(
        input,
        primary_model,
        request_inputs,
        tool_specs,
        generation_config,
        &inner.prompt_profile,
        inner.progress_commentary,
    ) {
        Ok(tokens) => tokens,
        Err(error) => {
            return abort_with_diagnostic(
                inner,
                sender,
                token,
                step_request_compile_diagnostic(&error),
            )
            .await;
        }
    };
    let history_ids = inner.session.lock().await.provider_transcript_history_ids();
    let source = match crate::compaction::CompactionRequestSource::new(
        request.clone(),
        &history_ids,
        input.user_messages_for_request().len(),
    ) {
        Ok(source) => source,
        Err(error) => {
            return abort_with_error(
                inner,
                sender,
                token,
                crate::RuntimeError::CompactionModelRequest {
                    message: error.to_string(),
                },
            )
            .await;
        }
    };
    let compaction_budget = match CompactionRequestBudget::new(
        source,
        policy,
        request_budget,
        fixed_dynamic_body_tokens,
    ) {
        Ok(budget) => budget,
        Err(error) => return abort_with_error(inner, sender, token, error.into()).await,
    };
    let preparation = compaction_preparation_for_budget(inner, &compaction_budget).await;
    let preparation = match preparation {
        Ok(Some(preparation)) => preparation,
        Ok(None)
            if request_budget.dynamic_body_estimated_tokens
                < compaction_budget.window_budget.max_dynamic_body_tokens() =>
        {
            return HardWatermarkOutcome::Continue {
                replacement: None,
                target_dynamic_body_tokens: compaction_budget.target_dynamic_body_tokens(),
            };
        }
        Ok(None) => {
            return abort_with_diagnostic(
                inner,
                sender,
                token,
                diagnostic_from_text(
                    "auto_compaction",
                    CompactionError::NoCompressibleWindow.to_string(),
                ),
            )
            .await;
        }
        Err(error) => {
            return abort_with_error(inner, sender, token, error).await;
        }
    };

    match preparation {
        CompactionPreparation::ReplaceCheckpoint(compaction_input) => {
            let attempt = match plan_compaction_attempt(
                inner,
                CompactionPreparation::ReplaceCheckpoint(compaction_input),
                &compaction_budget,
                reasoning_effort,
                token,
            )
            .await
            {
                Ok(attempt) => attempt,
                Err(error) => return abort_with_error(inner, sender, token, error).await,
            };
            match attempt {
                CompactionAttempt::ArchiveOnly { input, reason } => {
                    log_archive_only(inner, reason);
                    if !install_archive_only_reduction(inner, sender, input, token, active_permit)
                        .await
                    {
                        return HardWatermarkOutcome::Aborted;
                    }
                    HardWatermarkOutcome::Continue {
                        replacement: None,
                        target_dynamic_body_tokens: compaction_budget.target_dynamic_body_tokens(),
                    }
                }
                CompactionAttempt::Generate(plan) => {
                    if !send_compaction_started_event(inner, sender, token).await {
                        return HardWatermarkOutcome::Aborted;
                    }
                    match generate_and_install_compaction(
                        inner,
                        plan,
                        &compaction_budget,
                        reasoning_effort,
                        token.clone(),
                        active_permit,
                    )
                    .await
                    {
                        Ok(replacement) => HardWatermarkOutcome::Continue {
                            replacement: Some(replacement),
                            target_dynamic_body_tokens: compaction_budget
                                .target_dynamic_body_tokens(),
                        },
                        Err(error) => abort_with_error(inner, sender, token, error).await,
                    }
                }
            }
        }
        CompactionPreparation::ArchiveToolResults(archive_input) => {
            if !install_archive_only_reduction(inner, sender, archive_input, token, active_permit)
                .await
            {
                return HardWatermarkOutcome::Aborted;
            }
            HardWatermarkOutcome::Continue {
                replacement: None,
                target_dynamic_body_tokens: compaction_budget.target_dynamic_body_tokens(),
            }
        }
    }
}

/// Installs one archive-only reduction and reports whether the step may continue.
///
/// Returns `false` when this call already emitted the terminal event for the
/// step, which happens on cancellation or install failure.
async fn install_archive_only_reduction(
    inner: &Arc<RuntimeInner>,
    sender: &mpsc::Sender<RuntimeJournalEventBatch>,
    archive_input: ArchiveOnlyCompactionInput,
    token: &CancellationToken,
    active_permit: &ActiveStepPermit,
) -> bool {
    if token.is_cancelled() {
        let _ = abort_cancelled(inner, sender).await;
        return false;
    }
    if let Err(error) = install_archive_only_compaction_transactionally(
        Arc::clone(inner),
        archive_input,
        token.clone(),
        active_permit.clone(),
    )
    .await
    {
        if token.is_cancelled() {
            let _ = abort_cancelled(inner, sender).await;
            return false;
        }
        let _ = abort_with_diagnostic(
            inner,
            sender,
            token,
            diagnostic_from_text("auto_compaction", error.to_string()),
        )
        .await;
        return false;
    }
    tracing::debug!(
        event = "runtime.compaction.archive_only",
        session_id = inner.session_id.as_str(),
        "archived retained tool results without replacing the checkpoint"
    );
    true
}

/// Records why the runtime kept every turn raw.
fn log_archive_only(inner: &RuntimeInner, reason: ArchiveOnlyReason) {
    if let Some(error) = reason.budget_failure() {
        tracing::debug!(
            event = "runtime.compaction.archive_only_budget",
            session_id = inner.session_id.as_str(),
            error = runtime_error_message(&error),
            "compaction window cannot host a checkpoint replacement; archiving tool results instead"
        );
    }
}

/// Ends the step with the failure a compaction error produced.
async fn abort_with_error(
    inner: &Arc<RuntimeInner>,
    sender: &mpsc::Sender<RuntimeJournalEventBatch>,
    token: &CancellationToken,
    error: crate::RuntimeError,
) -> HardWatermarkOutcome {
    if token.is_cancelled() {
        return abort_cancelled(inner, sender).await;
    }
    abort_with_diagnostic(
        inner,
        sender,
        token,
        diagnostic_from_text("auto_compaction", runtime_error_message(&error)),
    )
    .await
}

/// Ends the step with one diagnostic.
async fn abort_with_diagnostic(
    inner: &Arc<RuntimeInner>,
    sender: &mpsc::Sender<RuntimeJournalEventBatch>,
    token: &CancellationToken,
    diagnostic: ErrorInfo,
) -> HardWatermarkOutcome {
    clear_current_activated_memories(inner).await;
    trace_provider_step_failed(&diagnostic);
    let _ = send_failed_event(inner, sender, token, diagnostic).await;
    HardWatermarkOutcome::Aborted
}

/// Ends the step as cancelled.
async fn abort_cancelled(
    inner: &Arc<RuntimeInner>,
    sender: &mpsc::Sender<RuntimeJournalEventBatch>,
) -> HardWatermarkOutcome {
    clear_current_activated_memories(inner).await;
    trace_provider_step_cancelled();
    let _ = send_cancelled_event(inner, sender).await;
    HardWatermarkOutcome::Aborted
}
