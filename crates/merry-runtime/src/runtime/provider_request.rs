use crate::{
    CheckpointDecision, ContextBudget, ContextBudgetPolicy, ContextCompiler, ProjectRules,
    ResolvedContextWindow, RuntimeError, SessionContextSnapshot, SkillCatalog, TaskAnchor,
    decide_checkpoint, resolve_context_window,
    session::{ResolvedToolContinuationSnapshot, SessionState},
    step::{CompiledSessionMessage, StepInput, StepModelRequestParts, compile_step_model_request},
};
use merry_core::ErrorInfo;
use merry_llm::{GenerationConfig, ModelError, ModelName};

use super::diagnostic_from_text;

const DEFAULT_CONTEXT_WINDOW_FALLBACK_TOKENS: u64 = 64_000;
const DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT: u8 = 95;
const DEFAULT_OUTPUT_RESERVE_TOKENS: u64 = 32_000;

pub(super) fn trace_provider_request(
    session_id: &str,
    provider_name: &str,
    request: &merry_llm::ModelRequest,
    continuation_count: usize,
    request_budget: Option<&RequestContextBudget>,
) {
    if let Some(request_budget) = request_budget {
        tracing::debug!(
            event = "runtime.provider.request",
            session_id,
            provider_name,
            model = request.model().as_str(),
            message_count = request.messages().len(),
            tool_count = request.tools().len(),
            continuation_count,
            stable_prefix_message_count = request.stable_prefix_message_count(),
            tool_profile_hash = request.tool_profile_hash().as_str(),
            stable_prefix_hash = request.stable_prefix_hash().as_str(),
            dynamic_context_hash = request.dynamic_context_hash().as_str(),
            context_window_tokens = request_budget.window.tokens(),
            context_window_source = request_budget.window.source().as_str(),
            context_budget_policy = request_budget.policy.as_str(),
            dynamic_body_estimated_tokens = request_budget.dynamic_body_estimated_tokens,
            body_budget_tokens = request_budget.budget.body_budget_tokens(),
            soft_water_tokens = request_budget.budget.soft_water_tokens(),
            hard_water_tokens = request_budget.budget.hard_water_tokens(),
            checkpoint_decision = request_budget.decision.as_str(),
            max_output_tokens = request.generation().max_output_tokens(),
            allow_parallel_tool_calls = request.generation().allow_parallel_tool_calls(),
            "runtime provider request metadata"
        );
    } else {
        tracing::debug!(
            event = "runtime.provider.request",
            session_id,
            provider_name,
            model = request.model().as_str(),
            message_count = request.messages().len(),
            tool_count = request.tools().len(),
            continuation_count,
            stable_prefix_message_count = request.stable_prefix_message_count(),
            tool_profile_hash = request.tool_profile_hash().as_str(),
            stable_prefix_hash = request.stable_prefix_hash().as_str(),
            dynamic_context_hash = request.dynamic_context_hash().as_str(),
            max_output_tokens = request.generation().max_output_tokens(),
            allow_parallel_tool_calls = request.generation().allow_parallel_tool_calls(),
            "runtime provider request metadata"
        );
    }
}

pub(super) fn trace_provider_request_budget_unavailable(
    session_id: &str,
    provider_name: &str,
    request: &merry_llm::ModelRequest,
    error: &crate::ContextError,
) {
    tracing::debug!(
        event = "runtime.provider.request.context_budget_unavailable",
        session_id,
        provider_name,
        model = request.model().as_str(),
        diagnostic_code = "context_budget",
        diagnostic_message = error.to_string(),
        "runtime provider request context budget unavailable"
    );
}

#[derive(Debug)]
pub(super) struct StepRequestInputs {
    snapshot: SessionContextSnapshot,
    skill_catalog: Option<SkillCatalog>,
    project_rules: Option<ProjectRules>,
    task_anchor: Option<TaskAnchor>,
    append_only_body: Vec<CompiledSessionMessage>,
    pub(super) continuations: Vec<ResolvedToolContinuationSnapshot>,
}

impl StepRequestInputs {
    pub(super) fn from_session(
        session: &SessionState,
        append_only_body: Vec<CompiledSessionMessage>,
        continuations: Vec<ResolvedToolContinuationSnapshot>,
    ) -> Self {
        Self {
            snapshot: session.context_snapshot(),
            skill_catalog: session.skill_catalog(),
            project_rules: session.project_rules(),
            task_anchor: session.task_anchor(),
            append_only_body,
            continuations,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum StepRequestCompileError {
    #[error("context compile error: {source}")]
    Context {
        #[from]
        source: crate::ContextError,
    },

    #[error("model request error: {source}")]
    Model {
        #[from]
        source: ModelError,
    },
}

pub(super) fn step_request_inputs_from_session(
    session: &SessionState,
) -> Result<StepRequestInputs, RuntimeError> {
    let append_only_body = session.append_only_body_snapshot()?;
    let continuations = session.uncheckpointed_tool_continuation_snapshots()?;
    Ok(StepRequestInputs::from_session(
        session,
        append_only_body,
        continuations,
    ))
}

pub(super) fn compile_step_request_from_inputs(
    input: &StepInput,
    model: &ModelName,
    inputs: &StepRequestInputs,
    tool_specs: Vec<merry_core::ToolSpec>,
    generation_config: GenerationConfig,
    progress_commentary: bool,
) -> Result<merry_llm::ModelRequest, StepRequestCompileError> {
    let compiled_context = ContextCompiler::new().compile(&inputs.snapshot)?;
    compile_step_model_request(StepModelRequestParts {
        input,
        model,
        skill_catalog: inputs.skill_catalog.as_ref(),
        project_rules: inputs.project_rules.as_ref(),
        task_anchor: inputs.task_anchor.as_ref(),
        context: &compiled_context,
        append_only_body: &inputs.append_only_body,
        continuations: &inputs.continuations,
        tool_specs,
        generation_config,
        progress_commentary,
    })
    .map_err(StepRequestCompileError::from)
}

pub(super) fn step_request_compile_diagnostic(error: &StepRequestCompileError) -> ErrorInfo {
    match error {
        StepRequestCompileError::Context { .. } => {
            diagnostic_from_text("context_compile", error.to_string())
        }
        StepRequestCompileError::Model { .. } => {
            diagnostic_from_text("model_request", error.to_string())
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct RequestContextBudget {
    pub(super) window: ResolvedContextWindow,
    pub(super) policy: ContextBudgetPolicy,
    pub(super) budget: ContextBudget,
    pub(super) dynamic_body_estimated_tokens: u64,
    pub(super) decision: CheckpointDecision,
}

pub(super) fn request_context_budget(
    capabilities: &merry_llm::ModelCapabilities,
    request: &merry_llm::ModelRequest,
) -> Result<RequestContextBudget, crate::ContextError> {
    let window = resolve_context_window(
        None,
        capabilities.max_input_tokens(),
        None,
        DEFAULT_CONTEXT_WINDOW_FALLBACK_TOKENS,
    )?;
    let output_reserve_tokens = request
        .generation()
        .max_output_tokens()
        .or_else(|| capabilities.max_output_tokens())
        .unwrap_or(DEFAULT_OUTPUT_RESERVE_TOKENS);
    let policy = ContextBudgetPolicy::Balanced;
    let stable_prefix_estimated_tokens =
        estimate_model_message_tokens(request.stable_prefix_messages());
    let budget = ContextBudget::from_window(
        window.tokens(),
        DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        stable_prefix_estimated_tokens,
        output_reserve_tokens,
        policy,
    )?;
    let dynamic_body_estimated_tokens = estimate_model_message_tokens(request.dynamic_messages())
        + estimate_tool_continuation_tokens(request.continuations());
    let decision = decide_checkpoint(dynamic_body_estimated_tokens, budget);

    Ok(RequestContextBudget {
        window,
        policy,
        budget,
        dynamic_body_estimated_tokens,
        decision,
    })
}

fn estimate_model_message_tokens(messages: &[merry_llm::ModelMessage]) -> u64 {
    messages
        .iter()
        .map(|message| estimate_text_tokens(message.content().as_text()))
        .sum()
}

fn estimate_tool_continuation_tokens(continuations: &[merry_llm::ModelToolContinuation]) -> u64 {
    continuations
        .iter()
        .map(|continuation| {
            estimate_text_tokens(continuation.call().name().as_str())
                + estimate_text_tokens(
                    &serde_json::to_string(continuation.call().arguments().as_object())
                        .expect("tool arguments must serialize for budget estimation"),
                )
                + estimate_text_tokens(continuation.result().content().as_str())
        })
        .sum()
}

fn estimate_text_tokens(text: &str) -> u64 {
    u64::try_from(text.len().div_ceil(4)).expect("usize should fit in u64 on supported targets")
}
