//! Runtime-owned process command tool registration helper.
//!
//! This module exposes a small provider-neutral tool that turns model supplied
//! `argv` into [`crate::ProcessActionIntent`] proposal evidence. It never
//! spawns a process itself; execution remains owned by the runtime process
//! policy and injected [`crate::ProcessRunner`] lanes.

use crate::{
    ActionProposal, ActionProposalEvidence, ProcessActionIntent, ProcessEnvPolicy, RegisteredTool,
    ToolActionKind, ToolActionProposalFuture, ToolExecutionContext, ToolExecutionError,
    ToolExecutor, ToolExecutorFuture,
};
use merry_core::{CoreError, PendingToolCall, ToolInputSchema, ToolName, ToolSpec};
use serde_json::{Value, json};
use std::sync::Arc;
use thiserror::Error;

const DEFAULT_PROCESS_TOOL_STDOUT_LIMIT_BYTES: usize = 64 * 1024;
const DEFAULT_PROCESS_TOOL_STDERR_LIMIT_BYTES: usize = 64 * 1024;

/// Errors raised while constructing the runtime-owned process command tool.
#[derive(Debug, Error)]
pub enum ProcessCommandToolError {
    /// The static provider-visible input schema could not be decoded.
    #[error("process command tool input schema could not be built: {source}")]
    InputSchema {
        /// Source schema decoding error.
        #[source]
        source: serde_json::Error,
    },

    /// A Merry core protocol value rejected the tool definition.
    #[error(transparent)]
    Core {
        /// Source core validation error.
        #[from]
        source: CoreError,
    },
}

/// Creates a registered process command tool for runtime agent loops.
///
/// The provider-visible tool accepts a JSON object with an `argv` string array
/// and optional workspace-relative `cwd`. The returned registered tool is a
/// `CommandExec` action with proposal evidence enabled, so admitted execution
/// goes through runtime process policy and an injected [`crate::ProcessRunner`].
pub fn process_command_tool(
    name: ToolName,
    description: &str,
) -> Result<RegisteredTool, ProcessCommandToolError> {
    let input_schema = process_command_tool_input_schema()?;
    let spec = ToolSpec::new(name, description, input_schema)?;
    Ok(RegisteredTool::new(
        spec,
        Arc::new(ProcessCommandToolExecutor),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal())
}

fn process_command_tool_input_schema() -> Result<ToolInputSchema, ProcessCommandToolError> {
    serde_json::from_value(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "argv": {
                "type": "array",
                "items": { "type": "string" },
                "minItems": 1
            },
            "cwd": {
                "type": "string",
                "description": "Optional workspace-relative working directory"
            }
        },
        "required": ["argv"]
    }))
    .map_err(|source| ProcessCommandToolError::InputSchema { source })
}

#[derive(Debug)]
struct ProcessCommandToolExecutor;

impl ToolExecutor for ProcessCommandToolExecutor {
    fn propose<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolActionProposalFuture<'a> {
        Box::pin(async move {
            let intent = process_intent_from_call(&call)?;
            let cwd_label = if intent.cwd().is_some() {
                "workspace-relative process"
            } else {
                "workspace-root process"
            };
            let proposal = ActionProposal::new(
                &call,
                ToolActionKind::CommandExec,
                "process command",
                cwd_label,
                format!(
                    "Run process with {} argv item(s) using an empty environment",
                    intent.argv().len()
                ),
                ActionProposalEvidence::ProcessAction(intent),
            )
            .map_err(|error| ToolExecutionError::infrastructure(error.to_string()))?;

            Ok(Some(proposal))
        })
    }

    fn execute<'a>(
        &'a self,
        _call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async {
            Err(ToolExecutionError::infrastructure(
                "process command tool must be executed through runtime process policy",
            ))
        })
    }
}

fn process_intent_from_call(
    call: &PendingToolCall,
) -> Result<ProcessActionIntent, ToolExecutionError> {
    let arguments = call.arguments().as_object();
    for key in arguments.keys() {
        if key != "argv" && key != "cwd" {
            return Err(invalid_arguments(format!(
                "unsupported argument field {key:?}"
            )));
        }
    }

    let argv = argv_from_arguments(arguments.get("argv"))?;
    let cwd = cwd_from_arguments(arguments.get("cwd"))?;

    ProcessActionIntent::new(
        argv,
        cwd,
        ProcessEnvPolicy::empty(),
        None,
        DEFAULT_PROCESS_TOOL_STDOUT_LIMIT_BYTES,
        DEFAULT_PROCESS_TOOL_STDERR_LIMIT_BYTES,
    )
    .map_err(|error| invalid_arguments(error.to_string()))
}

fn argv_from_arguments(value: Option<&Value>) -> Result<Vec<String>, ToolExecutionError> {
    let Some(Value::Array(values)) = value else {
        return Err(invalid_arguments("argv must be an array of strings"));
    };

    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid_arguments(format!("argv[{index}] must be a string")))
        })
        .collect()
}

fn cwd_from_arguments(value: Option<&Value>) -> Result<Option<String>, ToolExecutionError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(cwd)) => Ok(Some(cwd.clone())),
        Some(_) => Err(invalid_arguments("cwd must be a string when provided")),
    }
}

fn invalid_arguments(message: impl Into<String>) -> ToolExecutionError {
    ToolExecutionError::infrastructure(format!(
        "invalid process command tool arguments: {}",
        message.into()
    ))
}

#[cfg(test)]
mod tests {
    use super::{ProcessCommandToolExecutor, process_intent_from_call};
    use crate::{ActionProposalEvidence, ToolExecutionContext, ToolExecutor};
    use merry_core::{PendingToolCall, ToolCallArguments, ToolCallId, ToolName};
    use serde_json::json;

    fn pending_call(arguments: serde_json::Value) -> PendingToolCall {
        PendingToolCall::new(
            ToolCallId::new("call-process").expect("valid tool call id"),
            ToolName::new("run_process").expect("valid tool name"),
            ToolCallArguments::try_from(arguments).expect("valid tool call arguments"),
        )
    }

    #[test]
    fn process_intent_from_call_parses_argv_and_cwd() {
        let call = pending_call(json!({
            "argv": ["rustc", "--version"],
            "cwd": "crates/merry-runtime"
        }));

        let intent = process_intent_from_call(&call).expect("process intent should parse");

        assert_eq!(intent.argv(), ["rustc", "--version"]);
        assert_eq!(intent.cwd(), Some("crates/merry-runtime"));
        assert!(intent.stdin_text().is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn malformed_arguments_fail_before_execution() {
        let call = pending_call(json!({ "argv": "rustc --version" }));
        let error = ProcessCommandToolExecutor
            .propose(call, ToolExecutionContext::default())
            .await
            .expect_err("malformed argv should fail proposal");

        assert!(error.to_string().contains("argv must be an array"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn proposal_carries_process_action_evidence() {
        let call = pending_call(json!({ "argv": ["rustc", "--version"] }));
        let proposal = ProcessCommandToolExecutor
            .propose(call, ToolExecutionContext::default())
            .await
            .expect("proposal should succeed")
            .expect("proposal should be present");

        let ActionProposalEvidence::ProcessAction(intent) = proposal.evidence() else {
            panic!("proposal should carry process action evidence");
        };
        assert_eq!(intent.argv(), ["rustc", "--version"]);
    }
}
