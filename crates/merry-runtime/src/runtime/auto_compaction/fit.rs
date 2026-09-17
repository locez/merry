//! Sizing and compiling one compaction request against the compaction window.
//!
//! This module owns the arithmetic that decides whether a request may be sent:
//! how much input the window can host for a reserve, how much covered history to
//! give up when it cannot, and how to compile the request with the resulting
//! output ceiling.

use super::super::RuntimeInner;
use super::CompactionRequestBudget;
use crate::{
    CitationCompactionInput, RuntimeError,
    compaction::{
        CompactionReasoningReserve, compaction_request_required_tokens,
        compaction_window_safety_tokens, compile_citation_compaction_model_request,
    },
};
use merry_llm::{ModelInputItem, ReasoningEffort};
pub(crate) enum CompactionRequestFit {
    /// The request fits the window under this attempt's reserve.
    Request {
        request: Box<merry_llm::ModelRequest>,
    },
    /// The window cannot host the checkpoint text budget plus the reasoning reserve.
    WindowTooSmall {
        estimated_input_tokens: u64,
        max_output_tokens: u64,
    },
}

/// How strictly one attempt has to afford its reasoning reserve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReservePolicy {
    /// Grant the room the window has, as long as the checkpoint text budget fits.
    ///
    /// Used for the first attempt: a small window can still compact by granting
    /// less reasoning room, and refusing outright would stall the session.
    BestEffort,
    /// Only accept a window that can host the checkpoint text budget and the whole reserve.
    ///
    /// Used after the provider truncated an attempt, because best effort is what
    /// produced the truncation. Covering less history is how the retry makes room.
    Required,
}

/// Compiles one compaction request sized for the window and this attempt's reserve.
///
/// The requested output is the checkpoint text budget plus the reasoning reserve.
/// Input size does not depend on that ceiling, so the input is measured first and
/// the ceiling is sized from it. `policy` decides whether the window has to afford
/// the whole reserve or only the text budget; a window that affords neither is
/// reported as `WindowTooSmall` so the caller covers less history instead.
pub(crate) fn compile_fitted_compaction_request(
    input: &CitationCompactionInput,
    model: &merry_llm::ModelName,
    stable_prefix: &[ModelInputItem],
    reasoning_effort: Option<&ReasoningEffort>,
    compactor_window_tokens: u64,
    reserve: CompactionReasoningReserve,
    policy: ReservePolicy,
) -> Result<CompactionRequestFit, RuntimeError> {
    let compile = |output_ceiling_tokens: u64| {
        compile_citation_compaction_model_request(
            input,
            model,
            stable_prefix,
            reasoning_effort,
            output_ceiling_tokens,
        )
        .map_err(|error| RuntimeError::CompactionModelRequest {
            message: error.to_string(),
        })
    };
    let text_budget_tokens = input.resolved_budget().output_token_limit();
    let measured = compile(text_budget_tokens)?;
    let estimated_input_tokens = compaction_request_required_tokens(&measured).0;
    let reserved_output_tokens =
        reserve.output_ceiling(input.resolved_budget(), estimated_input_tokens);
    let available_output_tokens = compactor_window_tokens.saturating_sub(estimated_input_tokens);
    let affordable_output_tokens = available_output_tokens
        .saturating_sub(compaction_window_safety_tokens(available_output_tokens));
    let output_ceiling_tokens = match policy {
        ReservePolicy::BestEffort => affordable_output_tokens.min(reserved_output_tokens),
        ReservePolicy::Required => reserved_output_tokens,
    };
    let affordable_budget = match policy {
        ReservePolicy::BestEffort => text_budget_tokens,
        ReservePolicy::Required => output_ceiling_tokens,
    };
    if affordable_output_tokens < affordable_budget {
        return Ok(CompactionRequestFit::WindowTooSmall {
            estimated_input_tokens,
            max_output_tokens: reserved_output_tokens,
        });
    }
    let request = if output_ceiling_tokens == text_budget_tokens {
        measured
    } else {
        compile(output_ceiling_tokens)?
    };
    Ok(CompactionRequestFit::Request {
        request: Box::new(request),
    })
}

pub(crate) fn trace_compaction_request(
    inner: &RuntimeInner,
    provider: &dyn merry_llm::ModelProvider,
    request: &merry_llm::ModelRequest,
    budget: &CompactionRequestBudget,
    attempt: usize,
) {
    let response_format_name = match request.response_format() {
        Some(merry_llm::ModelResponseFormat::StructuredOutput(format)) => format.name(),
        None => "none",
    };
    tracing::debug!(
        event = "runtime.compaction.request",
        session_id = inner.session_id.as_str(),
        provider_name = provider.name().as_str(),
        model = request.model().as_str(),
        attempt,
        message_count = request.messages().len(),
        stable_prefix_message_count = request.stable_prefix_message_count(),
        reasoning_effort = request
            .generation()
            .reasoning_effort()
            .map(merry_llm::ReasoningEffort::as_str),
        estimated_input_tokens = crate::token_estimate::estimate_model_input_tokens(request.input()),
        max_output_tokens = request.generation().max_output_tokens(),
        response_format = response_format_name,
        primary_window_tokens = budget.primary_window_tokens,
        compactor_window_tokens = ?provider.capabilities().max_input_tokens(),
        "compaction model request prepared"
    );
}
