use crate::{
    CheckpointDecision, ContextBudget, ContextBudgetPolicy, ContextCompiler,
    DEFAULT_CONTEXT_WINDOW_FALLBACK_TOKENS, ProjectRules, ResolvedContextWindow, RuntimeError,
    SessionContextSnapshot, SkillCatalog, TaskAnchor, decide_checkpoint, resolve_context_window,
    session::{SessionState, TranscriptItemSnapshot},
    step::{StepInput, StepModelRequestParts, compile_step_model_request},
    token_estimate::estimate_model_input_tokens,
};
use merry_core::{CompactionUsageWindow, ErrorInfo, UsageContextWindow};
use merry_llm::{GenerationConfig, ModelError, ModelName};

use super::diagnostic_from_text;

const DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT: u8 = 95;
const DEFAULT_OUTPUT_RESERVE_WINDOW_DIVISOR: u64 = 20;
const DEFAULT_OUTPUT_RESERVE_MIN_TOKENS: u64 = 3_200;
const DEFAULT_OUTPUT_RESERVE_MAX_TOKENS: u64 = 8_192;

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
    pub(super) transcript: Vec<TranscriptItemSnapshot>,
}

impl StepRequestInputs {
    pub(super) fn from_session(
        session: &SessionState,
        transcript: Vec<TranscriptItemSnapshot>,
    ) -> Self {
        Self {
            snapshot: session.context_snapshot(),
            skill_catalog: session.skill_catalog(),
            project_rules: session.project_rules(),
            task_anchor: session.task_anchor(),
            transcript,
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
    let transcript = session.provider_transcript_snapshot()?;
    Ok(StepRequestInputs::from_session(session, transcript))
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
        transcript: &inputs.transcript,
        tool_specs,
        generation_config,
        progress_commentary,
    })
    .map_err(StepRequestCompileError::from)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CompactionFixedDynamicTokens {
    pub(super) replacement: u64,
    pub(super) archive_only: u64,
}

pub(super) fn estimate_compaction_fixed_dynamic_tokens(
    input: &StepInput,
    model: &ModelName,
    inputs: &StepRequestInputs,
    tool_specs: Vec<merry_core::ToolSpec>,
    generation_config: GenerationConfig,
    progress_commentary: bool,
) -> Result<CompactionFixedDynamicTokens, StepRequestCompileError> {
    let replacement_context =
        ContextCompiler::new().compile_without_compacted_checkpoint(&inputs.snapshot)?;
    let replacement_request = compile_step_model_request(StepModelRequestParts {
        input,
        model,
        skill_catalog: inputs.skill_catalog.as_ref(),
        project_rules: inputs.project_rules.as_ref(),
        task_anchor: inputs.task_anchor.as_ref(),
        context: &replacement_context,
        transcript: &[],
        tool_specs: tool_specs.clone(),
        generation_config: generation_config.clone(),
        progress_commentary,
    })?;
    let archive_only_context = ContextCompiler::new().compile(&inputs.snapshot)?;
    let archive_only_request = compile_step_model_request(StepModelRequestParts {
        input,
        model,
        skill_catalog: inputs.skill_catalog.as_ref(),
        project_rules: inputs.project_rules.as_ref(),
        task_anchor: inputs.task_anchor.as_ref(),
        context: &archive_only_context,
        transcript: &[],
        tool_specs,
        generation_config,
        progress_commentary,
    })?;

    Ok(CompactionFixedDynamicTokens {
        replacement: estimate_model_input_tokens(replacement_request.dynamic_input()),
        archive_only: estimate_model_input_tokens(archive_only_request.dynamic_input()),
    })
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StepUsageContextSnapshot {
    pub(super) context: Option<UsageContextWindow>,
    pub(super) compaction: Option<CompactionUsageWindow>,
}

impl StepUsageContextSnapshot {
    pub(super) const fn unavailable() -> Self {
        Self {
            context: None,
            compaction: None,
        }
    }
}

pub(super) fn step_usage_context_snapshot(
    request_budget: Option<&RequestContextBudget>,
    auto_compaction_enabled: bool,
) -> StepUsageContextSnapshot {
    let Some(request_budget) = request_budget else {
        return StepUsageContextSnapshot::unavailable();
    };

    StepUsageContextSnapshot {
        context: Some(UsageContextWindow {
            resolved_model_window_tokens: request_budget.window.tokens(),
            effective_window_tokens: request_budget.budget.effective_window_tokens(),
            source: request_budget.window.source(),
        }),
        compaction: Some(CompactionUsageWindow {
            auto_compaction_enabled,
            dynamic_body_estimated_tokens: Some(request_budget.dynamic_body_estimated_tokens),
            body_budget_tokens: request_budget.budget.body_budget_tokens(),
            soft_water_tokens: request_budget.budget.soft_water_tokens(),
            hard_water_tokens: request_budget.budget.hard_water_tokens(),
        }),
    }
}

pub(super) fn request_context_budget(
    capabilities: &merry_llm::ModelCapabilities,
    request: &merry_llm::ModelRequest,
    context_window_override: Option<u64>,
) -> Result<RequestContextBudget, crate::ContextError> {
    let window = resolve_request_context_window(capabilities, context_window_override)?;
    let output_reserve_tokens = request
        .generation()
        .max_output_tokens()
        .or_else(|| capabilities.max_output_tokens())
        .unwrap_or_else(|| default_output_reserve_tokens(window.tokens()));
    let policy = ContextBudgetPolicy::Balanced;
    let stable_prefix_estimated_tokens = estimate_model_input_tokens(request.stable_prefix_input());
    let budget = ContextBudget::from_window(
        window.tokens(),
        DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT,
        stable_prefix_estimated_tokens,
        output_reserve_tokens,
        policy,
    )?;
    let dynamic_body_estimated_tokens = estimate_model_input_tokens(request.dynamic_input());
    let decision = decide_checkpoint(dynamic_body_estimated_tokens, budget);

    Ok(RequestContextBudget {
        window,
        policy,
        budget,
        dynamic_body_estimated_tokens,
        decision,
    })
}

pub(super) fn resolve_request_context_window(
    capabilities: &merry_llm::ModelCapabilities,
    context_window_override: Option<u64>,
) -> Result<ResolvedContextWindow, crate::ContextError> {
    resolve_context_window(
        context_window_override,
        capabilities.max_input_tokens(),
        None,
        DEFAULT_CONTEXT_WINDOW_FALLBACK_TOKENS,
    )
}

fn default_output_reserve_tokens(window_tokens: u64) -> u64 {
    (window_tokens / DEFAULT_OUTPUT_RESERVE_WINDOW_DIVISOR).clamp(
        DEFAULT_OUTPUT_RESERVE_MIN_TOKENS,
        DEFAULT_OUTPUT_RESERVE_MAX_TOKENS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CompactedCheckpoint, session::SessionState};
    use merry_core::SessionId;

    #[test]
    fn compaction_fixed_estimates_keep_checkpoint_only_for_archive_only() {
        let mut session = SessionState::new(
            SessionId::new("fixed-compaction-estimates").expect("valid session id"),
        );
        let input = StepInput::user_text("current input").expect("valid step input");
        let model = ModelName::new("test/model").expect("valid model name");

        let without_checkpoint = StepRequestInputs::from_session(&session, Vec::new());
        let baseline = estimate_compaction_fixed_dynamic_tokens(
            &input,
            &model,
            &without_checkpoint,
            Vec::new(),
            GenerationConfig::default(),
            false,
        )
        .expect("baseline estimate builds");
        assert_eq!(baseline.replacement, baseline.archive_only);

        session.set_compacted_checkpoint(
            CompactedCheckpoint::new("checkpoint body ".repeat(200)).expect("valid checkpoint"),
        );
        let with_checkpoint = StepRequestInputs::from_session(&session, Vec::new());
        let checkpoint_estimates = estimate_compaction_fixed_dynamic_tokens(
            &input,
            &model,
            &with_checkpoint,
            Vec::new(),
            GenerationConfig::default(),
            false,
        )
        .expect("checkpoint estimates build");
        assert_eq!(checkpoint_estimates.replacement, baseline.replacement);
        assert!(checkpoint_estimates.archive_only > baseline.archive_only);

        let turn_id = session.begin_model_turn().expect("history turn begins");
        session
            .record_user_message_body(turn_id, &"history ballast ".repeat(1_000))
            .expect("history records");
        session
            .close_model_response(turn_id, false)
            .expect("history turn completes");
        let with_history = StepRequestInputs::from_session(
            &session,
            session
                .provider_transcript_snapshot()
                .expect("provider transcript builds"),
        );
        let history_estimates = estimate_compaction_fixed_dynamic_tokens(
            &input,
            &model,
            &with_history,
            Vec::new(),
            GenerationConfig::default(),
            false,
        )
        .expect("history estimates build");
        assert_eq!(history_estimates, checkpoint_estimates);
    }
}
