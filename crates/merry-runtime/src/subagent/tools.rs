mod schema;

pub use schema::subagent_tool_specs;
use schema::subagent_tool_specs_with_bounds_and_roles;

use super::{
    BackgroundHandoffNotice, CANCEL_SUBAGENTS_TOOL_NAME, SEND_SUBAGENT_MESSAGE_TOOL_NAME,
    SPAWN_SUBAGENTS_TOOL_NAME, SendSubagentMessageOptions, SubagentError, SubagentManager,
    SubagentTaskSpec, WAIT_SUBAGENTS_TOOL_NAME, WaitMode,
    protocol::{
        CancelSubagentsInput, SendSubagentMessageInput, SendSubagentMessageOutcome,
        SpawnSubagentTaskInput, SpawnSubagentsInput, SpawnWaitMode, SubagentMessageDelivery,
        WaitSubagentsInput, WaitSubagentsOutput,
    },
    sanitize_diagnostic_message, validate_task_max_model_turns,
};
use crate::{
    RegisteredTool, ToolActionKind, ToolExecutionContext, ToolExecutionError, ToolExecutionOutcome,
    ToolExecutionResult, ToolExecutor, ToolExecutorFuture,
};
use merry_core::{ErrorInfo, PendingToolCall};
use serde::{Serialize, de::DeserializeOwned};
use std::{sync::Arc, time::Duration};

const BACKGROUND_HANDOFF_MESSAGE: &str = "Foreground wait ended before all subagents finished. They were NOT cancelled and are still running in the background — do not spawn them again. You will be notified automatically when they complete.";

/// Returns provider-visible subagent tool specs with runtime-owned executors.
pub fn subagent_registered_tools(
    manager: SubagentManager,
) -> Result<[RegisteredTool; 4], merry_core::CoreError> {
    subagent_registered_tools_with_roles(manager, &[])
}

/// Returns provider-visible subagent tool specs advertising the enabled role
/// catalog in `spawn_subagents`'s description.
pub fn subagent_registered_tools_with_roles(
    manager: SubagentManager,
    role_summaries: &[(String, String)],
) -> Result<[RegisteredTool; 4], merry_core::CoreError> {
    let [spawn_spec, send_spec, wait_spec, cancel_spec] =
        subagent_tool_specs_with_bounds_and_roles(
            manager.min_model_turns(),
            manager.max_model_turns(),
            role_summaries,
        )?;
    Ok([
        RegisteredTool::new(
            spawn_spec,
            Arc::new(SpawnSubagentsExecutor::new(manager.clone())),
            ToolActionKind::RuntimeControl,
        )
        .with_parallel_safe_execution(),
        RegisteredTool::new(
            send_spec,
            Arc::new(SendSubagentMessageExecutor::new(manager.clone())),
            ToolActionKind::RuntimeControl,
        )
        .with_parallel_safe_execution(),
        RegisteredTool::read_only(
            wait_spec,
            Arc::new(WaitSubagentsExecutor::new(manager.clone())),
        )
        .with_parallel_safe_execution(),
        RegisteredTool::new(
            cancel_spec,
            Arc::new(CancelSubagentsExecutor::new(manager)),
            ToolActionKind::RuntimeControl,
        )
        .with_parallel_safe_execution(),
    ])
}

/// Runtime-owned executor for the provider-visible `spawn_subagents` tool.
#[derive(Clone)]
struct SpawnSubagentsExecutor {
    manager: SubagentManager,
}

impl SpawnSubagentsExecutor {
    /// Creates a spawn executor backed by the shared subagent manager.
    #[must_use]
    fn new(manager: SubagentManager) -> Self {
        Self { manager }
    }
}

impl ToolExecutor for SpawnSubagentsExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let input = match input_from_call::<SpawnSubagentsInput>(&call) {
                Ok(input) => input,
                Err(error) => {
                    return Ok(invalid_subagent_arguments_outcome(
                        call.name().as_str(),
                        error,
                    ));
                }
            };
            let wait_mode = input.wait.unwrap_or(SpawnWaitMode::None);
            let foreground_budget = match input.foreground_budget_secs {
                Some(0) => {
                    return Ok(invalid_subagent_arguments_outcome(
                        call.name().as_str(),
                        InvalidSubagentToolArguments::new(
                            "foreground_budget_secs must be greater than zero",
                        ),
                    ));
                }
                Some(secs) => Duration::from_secs(secs),
                None => self.manager.foreground_wait_budget(),
            };
            let tasks = match input
                .tasks
                .into_iter()
                .map(|task| {
                    task_spec_from_input(
                        task,
                        self.manager.min_model_turns(),
                        self.manager.max_model_turns(),
                    )
                })
                .collect::<Result<Vec<_>, _>>()
            {
                Ok(tasks) => tasks,
                Err(error) => {
                    return Ok(invalid_subagent_arguments_outcome(
                        call.name().as_str(),
                        error,
                    ));
                }
            };
            if context.cancellation_token().is_cancelled() {
                return Err(ToolExecutionError::Cancelled);
            }
            let mut output = self
                .manager
                .spawn(
                    tasks,
                    input.max_concurrency,
                    self.manager.session_token().clone(),
                )
                .await
                .map_err(infrastructure_error)?;

            let wait_target_mode = match wait_mode {
                SpawnWaitMode::None => None,
                SpawnWaitMode::All => Some(WaitMode::All),
                SpawnWaitMode::Any => Some(WaitMode::Any),
            };
            if let Some(mode) = wait_target_mode
                && !output.spawned.is_empty()
            {
                let spawned_ids = output
                    .spawned
                    .iter()
                    .map(|view| view.agent_id.clone())
                    .collect::<Vec<_>>();
                let wait = self
                    .manager
                    .wait(&spawned_ids, mode, Some(foreground_budget));
                let (wait_result, handoff_reason) = tokio::select! {
                    biased;
                    () = context.cancellation_token().cancelled() => {
                        let snapshot = self.manager.status_for(&spawned_ids).await;
                        let wait_snapshot = WaitSubagentsOutput::with_wait_state(
                            snapshot,
                            false,
                            true,
                        );
                        (wait_snapshot, Some("parent_turn_interrupted"))
                    }
                    result = wait => {
                        let wait_output = result.map_err(infrastructure_error)?;
                        let reason = (wait_output.timed_out && !wait_output.terminal)
                            .then_some("foreground_budget_elapsed");
                        (wait_output, reason)
                    }
                };
                if let Some(reason) = handoff_reason
                    && !wait_result.pending_agent_ids.is_empty()
                {
                    output.background_handoff = Some(BackgroundHandoffNotice {
                        handed_off_agent_ids: wait_result.pending_agent_ids.clone(),
                        reason: reason.to_owned(),
                        message: BACKGROUND_HANDOFF_MESSAGE.to_owned(),
                    });
                }
                output.wait_result = Some(wait_result);
            }

            spawn_json_output(&output)
        })
    }
}

/// Runtime-owned executor for the provider-visible `send_subagent_message` tool.
#[derive(Clone)]
struct SendSubagentMessageExecutor {
    manager: SubagentManager,
}

impl SendSubagentMessageExecutor {
    #[must_use]
    fn new(manager: SubagentManager) -> Self {
        Self { manager }
    }
}

impl ToolExecutor for SendSubagentMessageExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let input = match input_from_call::<SendSubagentMessageInput>(&call) {
                Ok(input) => input,
                Err(error) => {
                    return Ok(invalid_subagent_arguments_outcome(
                        call.name().as_str(),
                        error,
                    ));
                }
            };
            if input.message.trim().is_empty() {
                return Ok(invalid_subagent_arguments_outcome(
                    call.name().as_str(),
                    InvalidSubagentToolArguments::new("message must not be blank"),
                ));
            }
            if let Some(extra_turns) = input.extra_model_turns
                && let Err(error) =
                    validate_task_max_model_turns(extra_turns, 1, self.manager.max_model_turns())
            {
                return Ok(invalid_subagent_arguments_outcome(
                    call.name().as_str(),
                    InvalidSubagentToolArguments::from(error),
                ));
            }
            if context.cancellation_token().is_cancelled() {
                return Err(ToolExecutionError::Cancelled);
            }

            let options = SendSubagentMessageOptions {
                delivery: input.delivery.unwrap_or(SubagentMessageDelivery::Steer),
                extra_model_turns: input.extra_model_turns,
            };
            let mut output = self
                .manager
                .send_message_with_options(&input.agent_id, input.message, options)
                .await
                .map_err(infrastructure_error)?;

            if input.wait.unwrap_or(false)
                && matches!(
                    output.outcome,
                    SendSubagentMessageOutcome::Accepted | SendSubagentMessageOutcome::Woken
                )
            {
                let ids = [input.agent_id.clone()];
                let timeout = input
                    .timeout_ms
                    .map(Duration::from_millis)
                    .unwrap_or_else(|| self.manager.foreground_wait_budget());
                let wait = self.manager.wait(&ids, WaitMode::All, Some(timeout));
                let wait_result = tokio::select! {
                    biased;
                    () = context.cancellation_token().cancelled() => {
                        let snapshot = self.manager.status_for(&ids).await;
                        WaitSubagentsOutput::with_wait_state(snapshot, false, true)
                    }
                    result = wait => result.map_err(infrastructure_error)?,
                };
                if let Some(first) = wait_result.agents.first() {
                    output.status = Some(first.status.clone());
                }
                output.wait_result = Some(wait_result);
            }

            succeeded_json_output(SEND_SUBAGENT_MESSAGE_TOOL_NAME, &output)
        })
    }
}

/// Runtime-owned executor for the provider-visible `wait_subagents` tool.
#[derive(Clone)]
struct WaitSubagentsExecutor {
    manager: SubagentManager,
}

impl WaitSubagentsExecutor {
    /// Creates a wait executor backed by the shared subagent manager.
    #[must_use]
    fn new(manager: SubagentManager) -> Self {
        Self { manager }
    }
}

impl ToolExecutor for WaitSubagentsExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let input = match input_from_call::<WaitSubagentsInput>(&call) {
                Ok(input) => input,
                Err(error) => {
                    return Ok(invalid_subagent_arguments_outcome(
                        call.name().as_str(),
                        error,
                    ));
                }
            };
            if input.agent_ids.is_empty() {
                return Ok(invalid_subagent_arguments_outcome(
                    call.name().as_str(),
                    InvalidSubagentToolArguments::new(
                        "agent_ids must contain at least one child agent id",
                    ),
                ));
            }
            let timeout = input.timeout_ms.map(Duration::from_millis);
            let wait = self.manager.wait(
                &input.agent_ids,
                input.mode.unwrap_or(WaitMode::All),
                timeout,
            );
            let output = tokio::select! {
                biased;
                () = context.cancellation_token().cancelled() => {
                    return Err(ToolExecutionError::Cancelled);
                }
                output = wait => output.map_err(infrastructure_error)?,
            };

            succeeded_json_output(WAIT_SUBAGENTS_TOOL_NAME, &output)
        })
    }
}

/// Runtime-owned executor for the provider-visible `cancel_subagents` tool.
#[derive(Clone)]
struct CancelSubagentsExecutor {
    manager: SubagentManager,
}

impl CancelSubagentsExecutor {
    /// Creates a cancel executor backed by the shared subagent manager.
    #[must_use]
    fn new(manager: SubagentManager) -> Self {
        Self { manager }
    }
}

impl ToolExecutor for CancelSubagentsExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let input = match input_from_call::<CancelSubagentsInput>(&call) {
                Ok(input) => input,
                Err(error) => {
                    return Ok(invalid_subagent_arguments_outcome(
                        call.name().as_str(),
                        error,
                    ));
                }
            };
            if input.agent_ids.is_empty() {
                return Ok(invalid_subagent_arguments_outcome(
                    call.name().as_str(),
                    InvalidSubagentToolArguments::new(
                        "agent_ids must contain at least one child agent id",
                    ),
                ));
            }
            let output = self
                .manager
                .cancel(&input.agent_ids)
                .await
                .map_err(infrastructure_error)?;

            succeeded_json_output(CANCEL_SUBAGENTS_TOOL_NAME, &output)
        })
    }
}

fn input_from_call<T>(call: &PendingToolCall) -> Result<T, InvalidSubagentToolArguments>
where
    T: DeserializeOwned,
{
    call.arguments()
        .deserialize_as()
        .map_err(|error| InvalidSubagentToolArguments::new(format!("invalid tool input: {error}")))
}

fn task_spec_from_input(
    input: SpawnSubagentTaskInput,
    min_model_turns: u32,
    default_max_model_turns: u32,
) -> Result<SubagentTaskSpec, InvalidSubagentToolArguments> {
    let SpawnSubagentTaskInput {
        task,
        display_name,
        role,
        max_model_turns,
        timeout_secs,
        allowed_tools,
        read_scope,
        write_scope,
        forbidden_paths,
        context_files,
        expected_output,
        reasoning_effort,
        workspace,
        worktree_base,
    } = input;
    let reasoning_effort = reasoning_effort
        .map(|value| merry_llm::ReasoningEffort::new(&value))
        .transpose()
        .map_err(|error| InvalidSubagentToolArguments::new(error.to_string()))?;

    let max_model_turns_explicit = max_model_turns.is_some();
    let max_model_turns = max_model_turns.unwrap_or(default_max_model_turns);
    validate_task_max_model_turns(max_model_turns, min_model_turns, default_max_model_turns)
        .map_err(InvalidSubagentToolArguments::from)?;
    let mut task = SubagentTaskSpec::new(task, max_model_turns)
        .map_err(InvalidSubagentToolArguments::from)?
        .with_max_model_turns_explicit(max_model_turns_explicit)
        .with_display_name(display_name)
        .with_role(role)
        .map_err(InvalidSubagentToolArguments::from)?
        .with_timeout_secs(timeout_secs)
        .map_err(InvalidSubagentToolArguments::from)?;
    if let Some(allowed_tools) = allowed_tools {
        task = task.with_allowed_tools(allowed_tools);
    }
    if let Some(read_scope) = read_scope {
        task = task
            .with_read_scope(read_scope)
            .map_err(InvalidSubagentToolArguments::from)?;
    }
    if let Some(write_scope) = write_scope {
        task = task
            .with_write_scope(write_scope)
            .map_err(InvalidSubagentToolArguments::from)?;
    }
    if let Some(forbidden_paths) = forbidden_paths {
        task = task
            .with_forbidden_paths(forbidden_paths)
            .map_err(InvalidSubagentToolArguments::from)?;
    }
    if let Some(context_files) = context_files {
        task = task
            .with_context_files(context_files)
            .map_err(InvalidSubagentToolArguments::from)?;
    }
    if let Some(workspace_mode) = workspace {
        task = task.with_workspace_mode(workspace_mode);
    }
    if let Some(worktree_base) = worktree_base {
        task = task.with_worktree_base(worktree_base);
    }
    Ok(task
        .with_expected_output(expected_output)
        .with_reasoning_effort(reasoning_effort))
}

fn succeeded_json_output<T>(tool_name: &str, output: &T) -> ToolExecutionResult
where
    T: Serialize,
{
    let content = serde_json::to_string(output).map_err(|error| {
        ToolExecutionError::infrastructure(format!(
            "failed to serialize {tool_name} output: {error}"
        ))
    })?;
    Ok(ToolExecutionOutcome::succeeded_json(content))
}

fn spawn_json_output(output: &super::super::SpawnSubagentsOutput) -> ToolExecutionResult {
    let content = serde_json::to_string(output).map_err(|error| {
        ToolExecutionError::infrastructure(format!(
            "failed to serialize {SPAWN_SUBAGENTS_TOOL_NAME} output: {error}"
        ))
    })?;

    if output.spawned.is_empty() && !output.rejected.is_empty() {
        return Ok(ToolExecutionOutcome::failed_json(
            content,
            ErrorInfo::new(
                SUBAGENT_SPAWN_REJECTED_CODE,
                "all requested child agents were rejected",
            )
            .expect("static subagent diagnostic code is valid"),
        ));
    }

    Ok(ToolExecutionOutcome::succeeded_json(content))
}

fn infrastructure_error(error: impl std::fmt::Display) -> ToolExecutionError {
    ToolExecutionError::infrastructure(error.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InvalidSubagentToolArguments {
    message: String,
}

impl InvalidSubagentToolArguments {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: sanitize_diagnostic_message(message.into()),
        }
    }

    fn message(&self) -> &str {
        &self.message
    }
}

impl From<SubagentError> for InvalidSubagentToolArguments {
    fn from(error: SubagentError) -> Self {
        Self::new(error.to_string())
    }
}

const SUBAGENT_INVALID_ARGUMENTS_CODE: &str = "subagent_invalid_arguments";
const SUBAGENT_SPAWN_REJECTED_CODE: &str = "subagent_spawn_rejected";

fn invalid_subagent_arguments_outcome(
    tool_name: &str,
    error: InvalidSubagentToolArguments,
) -> ToolExecutionOutcome {
    let payload = serde_json::json!({
        "ok": false,
        "tool": tool_name,
        "error": {
            "code": SUBAGENT_INVALID_ARGUMENTS_CODE,
            "message": error.message(),
        },
        "recovery": {
            "input_contract": "Provide arguments matching the subagent tool input schema.",
            "scope_contract": "Paths must be normalized workspace-relative paths.",
            "tool_name_contract": "allowed_tools entries must be exact registered Merry tool names copied from the current tool list. Use run_process, never functions.run_process.",
        }
    });

    ToolExecutionOutcome::failed_json(
        payload.to_string(),
        ErrorInfo::new(SUBAGENT_INVALID_ARGUMENTS_CODE, error.message())
            .expect("static subagent diagnostic code is valid"),
    )
}

#[cfg(test)]
mod tests;
