mod schema;

pub use schema::subagent_tool_specs;
use schema::subagent_tool_specs_with_bounds;

use super::{
    CANCEL_SUBAGENTS_TOOL_NAME, SPAWN_SUBAGENTS_TOOL_NAME, SubagentError, SubagentManager,
    SubagentTaskSpec, WAIT_SUBAGENTS_TOOL_NAME, WaitMode,
    protocol::{
        CancelSubagentsInput, SpawnSubagentTaskInput, SpawnSubagentsInput, WaitSubagentsInput,
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

/// Returns provider-visible subagent tool specs with runtime-owned executors.
pub fn subagent_registered_tools(
    manager: SubagentManager,
) -> Result<[RegisteredTool; 3], merry_core::CoreError> {
    let [spawn_spec, wait_spec, cancel_spec] =
        subagent_tool_specs_with_bounds(manager.min_model_turns(), manager.max_model_turns())?;
    Ok([
        RegisteredTool::new(
            spawn_spec,
            Arc::new(SpawnSubagentsExecutor::new(manager.clone())),
            ToolActionKind::RuntimeControl,
        ),
        RegisteredTool::read_only(
            wait_spec,
            Arc::new(WaitSubagentsExecutor::new(manager.clone())),
        ),
        RegisteredTool::new(
            cancel_spec,
            Arc::new(CancelSubagentsExecutor::new(manager)),
            ToolActionKind::RuntimeControl,
        ),
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
            let output = self
                .manager
                .spawn(
                    tasks,
                    input.max_concurrency,
                    context.cancellation_token().clone(),
                )
                .await
                .map_err(infrastructure_error)?;

            spawn_json_output(&output)
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
        max_model_turns,
        allowed_tools,
        read_scope,
        write_scope,
        forbidden_paths,
        expected_output,
        reasoning_effort,
        plan_client_key,
    } = input;
    let reasoning_effort = reasoning_effort
        .map(|value| merry_llm::ReasoningEffort::new(&value))
        .transpose()
        .map_err(|error| InvalidSubagentToolArguments::new(error.to_string()))?;

    let max_model_turns = max_model_turns.unwrap_or(default_max_model_turns);
    validate_task_max_model_turns(max_model_turns, min_model_turns, default_max_model_turns)
        .map_err(InvalidSubagentToolArguments::from)?;
    let mut task = SubagentTaskSpec::new(task, max_model_turns)
        .map_err(InvalidSubagentToolArguments::from)?
        .with_display_name(display_name);
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
    Ok(task
        .with_expected_output(expected_output)
        .with_reasoning_effort(reasoning_effort)
        .with_plan_client_key(plan_client_key))
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
