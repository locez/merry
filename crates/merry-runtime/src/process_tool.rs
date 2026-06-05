//! Runtime-owned process command tool registration helper.
//!
//! This module exposes a small provider-neutral tool that turns model supplied
//! `argv` into [`crate::ProcessActionIntent`] proposal evidence. It never
//! spawns a process itself; execution remains owned by the runtime process
//! policy and injected [`crate::ProcessRunner`] lanes.

use crate::{
    ActionProposal, ActionProposalEvidence, ProcessActionIntent, ProcessEnvPolicy, RegisteredTool,
    ToolActionKind, ToolActionPreflight, ToolActionProposalFuture, ToolExecutionContext,
    ToolExecutionError, ToolExecutionOutcome, ToolExecutor, ToolExecutorFuture,
};
use merry_core::{CoreError, ErrorInfo, PendingToolCall, ToolInputSchema, ToolName, ToolSpec};
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
                "minLength": 1,
                "description": "Optional workspace-relative working directory. For the workspace root, omit cwd or use \".\"; never pass an empty string."
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
            let intent = match process_intent_from_call(&call) {
                Ok(intent) => intent,
                Err(error) => {
                    tracing::debug!(
                        tool_call_id = call.id().as_str(),
                        tool_name = call.name().as_str(),
                        diagnostic_code = PROCESS_COMMAND_INVALID_ARGUMENTS_CODE,
                        "process command tool proposal rejected invalid arguments"
                    );
                    return Ok(ToolActionPreflight::Outcome(invalid_arguments_outcome(
                        call.name().as_str(),
                        error,
                    )));
                }
            };
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

            Ok(ToolActionPreflight::Proposal(proposal))
        })
    }

    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            if let Err(error) = process_intent_from_call(&call) {
                return Ok(invalid_arguments_outcome(call.name().as_str(), error));
            }
            Err(ToolExecutionError::infrastructure(
                "process command tool must be executed through runtime process policy",
            ))
        })
    }
}

fn process_intent_from_call(
    call: &PendingToolCall,
) -> Result<ProcessActionIntent, InvalidProcessCommandArguments> {
    let arguments = call.arguments().as_object();
    for key in arguments.keys() {
        if key != "argv" && key != "cwd" {
            return Err(InvalidProcessCommandArguments::new(format!(
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
    .map_err(|error| InvalidProcessCommandArguments::new(error.to_string()))
}

fn argv_from_arguments(
    value: Option<&Value>,
) -> Result<Vec<String>, InvalidProcessCommandArguments> {
    let Some(Value::Array(values)) = value else {
        return Err(InvalidProcessCommandArguments::new(
            "argv must be an array of strings",
        ));
    };

    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value.as_str().map(str::to_owned).ok_or_else(|| {
                InvalidProcessCommandArguments::new(format!("argv[{index}] must be a string"))
            })
        })
        .collect()
}

fn cwd_from_arguments(
    value: Option<&Value>,
) -> Result<Option<String>, InvalidProcessCommandArguments> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(cwd)) if cwd.is_empty() => Ok(None),
        Some(Value::String(cwd)) => Ok(Some(cwd.clone())),
        Some(_) => Err(InvalidProcessCommandArguments::new(
            "cwd must be a string when provided",
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InvalidProcessCommandArguments {
    message: String,
}

impl InvalidProcessCommandArguments {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn message(&self) -> &str {
        &self.message
    }
}

const PROCESS_COMMAND_INVALID_ARGUMENTS_CODE: &str = "process_command_invalid_arguments";

fn invalid_arguments_outcome(
    tool_name: &str,
    error: InvalidProcessCommandArguments,
) -> ToolExecutionOutcome {
    let payload = json!({
        "ok": false,
        "tool": tool_name,
        "error": {
            "code": PROCESS_COMMAND_INVALID_ARGUMENTS_CODE,
            "message": error.message(),
        },
        "recovery": {
            "argv_contract": "Provide argv as a JSON array of non-empty strings. Newline and tab are allowed inside argv items; other control characters are rejected.",
            "cwd_contract": "cwd, when provided, must be non-empty, workspace-relative, and must not contain control characters. For the workspace root, omit cwd or use \".\".",
        },
        "guidance": {
            "kind": "invalid_process_arguments",
            "message": "Fix the process tool arguments before retrying. Use argv as an exact JSON string array, omit cwd or use a workspace-relative cwd such as \".\", and do not pass an empty cwd string.",
        }
    });
    ToolExecutionOutcome::failed_json(
        payload.to_string(),
        ErrorInfo::new(PROCESS_COMMAND_INVALID_ARGUMENTS_CODE, error.message())
            .expect("static process command diagnostic code is valid"),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        PROCESS_COMMAND_INVALID_ARGUMENTS_CODE, ProcessCommandToolExecutor,
        process_intent_from_call,
    };
    use crate::{ActionProposalEvidence, ToolActionPreflight, ToolExecutionContext, ToolExecutor};
    use merry_core::{PendingToolCall, ToolCallArguments, ToolCallId, ToolName};
    use serde_json::{Value, json};

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

    #[test]
    fn process_command_tool_schema_rejects_empty_cwd() {
        let tool = super::process_command_tool(
            ToolName::new("run_process").expect("valid tool name"),
            "Run a process.",
        )
        .expect("process command tool should build");
        let schema = serde_json::to_value(tool.spec().input_schema().as_schema())
            .expect("schema should serialize");

        assert_eq!(schema["properties"]["cwd"]["minLength"], 1);
        assert!(
            schema["properties"]["cwd"]["description"]
                .as_str()
                .expect("cwd description should be text")
                .contains("never pass an empty string")
        );
    }

    #[test]
    fn process_intent_from_call_treats_empty_cwd_as_workspace_root() {
        let call = pending_call(json!({
            "argv": ["ping", "-c", "1", "baidu.com"],
            "cwd": ""
        }));

        let intent = process_intent_from_call(&call).expect("process intent should parse");

        assert_eq!(intent.argv(), ["ping", "-c", "1", "baidu.com"]);
        assert_eq!(intent.cwd(), None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn malformed_arguments_fail_before_execution() {
        let call = pending_call(json!({ "argv": "rustc --version" }));
        let proposal = ProcessCommandToolExecutor
            .propose(call.clone(), ToolExecutionContext::default())
            .await
            .expect("malformed argv should not be infrastructure failure");

        assert!(matches!(proposal, ToolActionPreflight::Outcome(_)));

        let outcome = ProcessCommandToolExecutor
            .execute(call, ToolExecutionContext::default())
            .await
            .expect("malformed argv should resolve as failed tool outcome");

        assert_eq!(outcome.status(), merry_core::ToolCallResultStatus::Failed);
        let diagnostic = outcome
            .diagnostic()
            .expect("failed process argument outcome should include diagnostic");
        assert_eq!(diagnostic.code(), PROCESS_COMMAND_INVALID_ARGUMENTS_CODE);
        let payload: Value = serde_json::from_str(
            outcome
                .content()
                .as_text()
                .expect("failed process argument outcome should be JSON"),
        )
        .expect("failed process argument outcome should parse as JSON");
        assert_eq!(
            payload["error"]["message"],
            "argv must be an array of strings"
        );
        assert_eq!(payload["guidance"]["kind"], "invalid_process_arguments");
        assert!(
            payload["guidance"]["message"]
                .as_str()
                .expect("guidance should be text")
                .contains("omit cwd or use a workspace-relative cwd")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn proposal_carries_process_action_evidence() {
        let call = pending_call(json!({ "argv": ["rustc", "--version"] }));
        let preflight = ProcessCommandToolExecutor
            .propose(call, ToolExecutionContext::default())
            .await
            .expect("proposal should succeed");
        let ToolActionPreflight::Proposal(proposal) = preflight else {
            panic!("proposal should be present");
        };

        let ActionProposalEvidence::ProcessAction(intent) = proposal.evidence() else {
            panic!("proposal should carry process action evidence");
        };
        assert_eq!(intent.argv(), ["rustc", "--version"]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn proposal_accepts_multiline_shell_argv_for_policy_classification() {
        let call = pending_call(json!({
            "argv": ["bash", "-lc", "cargo check -p merry-runtime\ncargo test -p merry-runtime"],
            "cwd": "."
        }));
        let preflight = ProcessCommandToolExecutor
            .propose(call, ToolExecutionContext::default())
            .await
            .expect("multiline shell argv should not be infrastructure failure");
        let ToolActionPreflight::Proposal(proposal) = preflight else {
            panic!("multiline shell argv should produce process proposal");
        };

        let ActionProposalEvidence::ProcessAction(intent) = proposal.evidence() else {
            panic!("proposal should carry process action evidence");
        };
        assert_eq!(
            intent.argv()[2],
            "cargo check -p merry-runtime\ncargo test -p merry-runtime"
        );
    }
}
