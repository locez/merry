//! Reuses the primary request compiler for manually requested compaction.

use super::{CompactionRequestBudget, RuntimeInner};
use crate::{
    CitationCompactionPolicy, RuntimeError, RuntimeModelRole, StepInput,
    compaction::CompactionRequestSource,
    runtime::provider_request::{
        compile_step_request_from_inputs, estimate_compaction_fixed_dynamic_tokens,
        request_context_budget, step_request_inputs_from_session,
    },
};
use merry_llm::GenerationConfig;

/// Compiles the session and budgets the destination request for manual compaction.
pub(super) async fn manual_compaction_budget(
    inner: &RuntimeInner,
    policy: CitationCompactionPolicy,
) -> Result<CompactionRequestBudget, RuntimeError> {
    let config = inner.model_config(RuntimeModelRole::Primary).await.ok_or(
        RuntimeError::MissingModelProvider {
            role: RuntimeModelRole::Primary.as_str(),
        },
    )?;
    let (inputs, history_ids) = {
        let session = inner.session.lock().await;
        (
            step_request_inputs_from_session(&session, None, inner.coordinator_plan_tools)?,
            session.provider_transcript_history_ids(),
        )
    };
    let input = StepInput::no_new_user_input();
    let tools = inner.visible_tool_specs();
    let generation = GenerationConfig::default();
    let request = compile_step_request_from_inputs(
        &input,
        config.model(),
        &inputs,
        tools.clone(),
        generation.clone(),
        &inner.prompt_profile,
        inner.progress_commentary,
    )
    .map_err(|error| RuntimeError::CompactionModelRequest {
        message: error.to_string(),
    })?;
    let fixed_tokens = estimate_compaction_fixed_dynamic_tokens(
        &input,
        config.model(),
        &inputs,
        tools,
        generation,
        &inner.prompt_profile,
        inner.progress_commentary,
    )
    .map_err(|error| RuntimeError::CompactionModelRequest {
        message: error.to_string(),
    })?;
    let context_window_override = inner
        .context_window_tokens
        .read()
        .await
        .map(std::num::NonZeroU64::get);
    let request_budget = request_context_budget(
        config.provider().capabilities(),
        &request,
        context_window_override,
    )?;
    let source = CompactionRequestSource::new(request, &history_ids, 0).map_err(|error| {
        RuntimeError::CompactionModelRequest {
            message: error.to_string(),
        }
    })?;
    CompactionRequestBudget::new(source, policy, &request_budget, fixed_tokens)
        .map_err(RuntimeError::from)
}
