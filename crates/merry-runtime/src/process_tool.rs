//! Runtime-owned process command tool registration helper.
//!
//! This module exposes a small provider-neutral tool that turns a model
//! supplied shell command into [`crate::ProcessActionIntent`] proposal
//! evidence. It never spawns a process itself; execution remains owned by the
//! runtime process policy and injected [`crate::ProcessRunner`] lanes.

use crate::{
    ActionProposal, ActionProposalEvidence, MAX_PROCESS_ARG_BYTES, ProcessActionIntent,
    ProcessEnvPolicy, RegisteredTool, ToolActionKind, ToolActionPreflight,
    ToolActionProposalFuture, ToolExecutionContext, ToolExecutionError, ToolExecutionOutcome,
    ToolExecutor, ToolExecutorFuture,
    permission::{
        RequestedCapabilitiesInput, permission_reason_schema_for_schemars,
        process_cwd_schema_for_schemars, requested_capabilities_schema_for_schemars,
    },
    process::shell_command_argv,
    tool_input::deserialize_non_empty_process_command,
};
use merry_core::{CoreError, ErrorInfo, PendingToolCall, ToolName};
use merry_tools_macros::tool;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use thiserror::Error;

const DEFAULT_PROCESS_TOOL_STDOUT_LIMIT_BYTES: usize = 64 * 1024;
const DEFAULT_PROCESS_TOOL_STDERR_LIMIT_BYTES: usize = 64 * 1024;

#[tool(
    crate = "crate",
    name = "run_process",
    description = "Run a validated shell command through the runtime process policy."
)]
#[derive(Debug, JsonSchema)]
struct ProcessCommandInput {
    #[schemars(
        description = "Shell command to execute. Newline and tab are allowed; other control characters are rejected. JSON strings must escape embedded control characters.",
        length(min = 1, max = MAX_PROCESS_ARG_BYTES)
    )]
    command: String,
    #[schemars(
        schema_with = "process_cwd_schema_for_schemars",
        description = "Optional workspace-relative working directory. Omit it, use \".\", or use null for the current workspace directory; an empty string is accepted as the same root default for compatibility."
    )]
    #[serde(default)]
    cwd: String,
    #[schemars(
        schema_with = "permission_reason_schema_for_schemars",
        description = "Optional short explanation of why the current task needs the requested capability. Use it only together with permissions."
    )]
    #[serde(default)]
    reason: Option<String>,
    #[schemars(schema_with = "requested_capabilities_schema_for_schemars")]
    #[serde(default)]
    permissions: Option<RequestedCapabilitiesInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessCommandInputWire {
    #[serde(deserialize_with = "deserialize_non_empty_process_command")]
    command: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    permissions: Option<RequestedCapabilitiesInput>,
}

impl<'de> Deserialize<'de> for ProcessCommandInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let input = ProcessCommandInputWire::deserialize(deserializer)?;
        Ok(Self {
            command: input.command,
            cwd: input.cwd.unwrap_or_default(),
            reason: input.reason,
            permissions: input.permissions,
        })
    }
}

/// Errors raised while constructing the runtime-owned process command tool.
#[derive(Debug, Error)]
pub enum ProcessCommandToolError {
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
/// The provider-visible tool accepts a JSON object with a shell `command`,
/// nullable workspace-relative `cwd`, and optional capabilities to request
/// before execution. The returned registered tool is a `CommandExec` action
/// with proposal evidence enabled, so admitted execution goes through runtime
/// process policy and an injected [`crate::ProcessRunner`].
pub fn process_command_tool(
    name: ToolName,
    description: &str,
) -> Result<RegisteredTool, ProcessCommandToolError> {
    let spec =
        ProcessCommandInput::tool_spec_with(name.as_str(), description).map_err(|error| {
            ProcessCommandToolError::Core {
                source: error.into(),
            }
        })?;
    Ok(RegisteredTool::new(
        spec,
        Arc::new(ProcessCommandToolExecutor),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal())
}

pub(crate) fn process_call_requests_permission(call: &PendingToolCall) -> bool {
    let arguments = call.arguments().as_object();
    arguments
        .get("permissions")
        .is_some_and(|permissions| !permissions.is_null())
        || arguments
            .get("reason")
            .is_some_and(|reason| !reason.is_null())
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
                "Run the validated shell command through the platform process wrapper using an empty environment".to_owned(),
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
    let input = call
        .arguments()
        .deserialize_as::<ProcessCommandInput>()
        .map_err(|error| InvalidProcessCommandArguments::new(error.to_string()))?;
    let _ = (&input.reason, &input.permissions);
    let argv = shell_command_argv(&input.command);
    let cwd = (!input.cwd.is_empty()).then_some(input.cwd);

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
            "command_contract": "Provide command as one non-empty shell command string. Newline and tab are allowed; other control characters are rejected. Escape control characters for the surrounding JSON string.",
            "cwd_contract": "cwd, when provided, must be non-empty, workspace-relative, and must not contain control characters. For the workspace root, omit cwd or use \".\".",
        },
        "guidance": {
            "kind": "invalid_process_arguments",
            "message": "Fix the process tool arguments before retrying. Use command as one JSON string, and set cwd to null or a workspace-relative directory such as \".\".",
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
        process_call_requests_permission, process_intent_from_call,
    };
    use crate::{
        ActionProposalEvidence, ToolActionPreflight, ToolExecutionContext, ToolExecutor,
        process::shell_command_argv,
    };
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
    fn process_intent_from_call_parses_command_and_cwd() {
        let call = pending_call(json!({
            "command": "rustc --version",
            "cwd": "crates/merry-runtime"
        }));

        let intent = process_intent_from_call(&call).expect("process intent should parse");

        assert_eq!(intent.argv(), shell_command_argv("rustc --version"));
        assert_eq!(intent.cwd(), Some("crates/merry-runtime"));
        assert!(intent.stdin_text().is_none());
    }

    #[test]
    fn process_command_tool_schema_is_provider_compatible() {
        let tool = super::process_command_tool(
            ToolName::new("run_process").expect("valid tool name"),
            "Run a process.",
        )
        .expect("process command tool should build");
        let schema = serde_json::to_value(tool.spec().input_schema().as_schema())
            .expect("schema should serialize");
        crate::schema_contract::assert_provider_input_schema_fields_have_descriptions(tool.spec());

        assert!(
            schema["properties"]["cwd"]["description"]
                .as_str()
                .expect("cwd description should be text")
                .contains("Omit it")
        );
        assert!(
            schema["properties"]["cwd"]["description"]
                .as_str()
                .expect("cwd description should be text")
                .contains("current workspace directory")
        );
        let cwd_string_schema = schema["properties"]["cwd"]["anyOf"]
            .as_array()
            .expect("cwd should have nullable branches")
            .iter()
            .find(|branch| branch["type"] == "string")
            .expect("cwd should have a string branch");
        assert_eq!(cwd_string_schema["minLength"], 1);
        assert_eq!(cwd_string_schema["maxLength"], crate::MAX_PROCESS_CWD_BYTES);
        assert!(
            !schema["properties"]["command"]["description"]
                .as_str()
                .unwrap_or_default()
                .is_empty()
        );
        assert_eq!(schema["properties"]["command"]["minLength"], 1);
        assert_eq!(
            schema["properties"]["command"]["maxLength"],
            crate::MAX_PROCESS_ARG_BYTES
        );
        assert_eq!(schema["required"], json!(["command"]));
        assert!(
            schema["properties"]["permissions"]["description"]
                .as_str()
                .is_some_and(|description| description.contains("exact action"))
        );
        assert!(schema["properties"]["reason"]["description"].is_string());
    }

    #[test]
    fn process_intent_from_call_treats_empty_cwd_as_workspace_root() {
        let call = pending_call(json!({
            "command": "ping -c 1 baidu.com",
            "cwd": ""
        }));

        let intent = process_intent_from_call(&call).expect("process intent should parse");

        assert_eq!(intent.argv(), shell_command_argv("ping -c 1 baidu.com"));
        assert_eq!(intent.cwd(), None);
    }

    #[test]
    fn process_intent_from_call_defaults_omitted_cwd_to_workspace_root() {
        let call = pending_call(json!({
            "command": "pwd"
        }));

        let intent = process_intent_from_call(&call).expect("process intent should parse");

        assert_eq!(intent.cwd(), None);
    }

    #[test]
    fn nullable_permission_reason_is_treated_as_omitted() {
        let call = pending_call(json!({
            "command": "rg --files",
            "cwd": null,
            "reason": null,
        }));

        assert!(!process_call_requests_permission(&call));
        process_intent_from_call(&call).expect("nullable reason should remain valid");
    }

    #[test]
    fn nullable_permissions_are_treated_as_omitted() {
        let call = pending_call(json!({
            "command": "rg --files",
            "permissions": null,
        }));

        assert!(!process_call_requests_permission(&call));
        process_intent_from_call(&call).expect("nullable permissions should remain valid");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn malformed_arguments_fail_before_execution() {
        let call = pending_call(json!({ "command": 42, "cwd": null }));
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
            "command must be a non-empty string"
        );
        assert_eq!(payload["guidance"]["kind"], "invalid_process_arguments");
        assert!(
            payload["guidance"]["message"]
                .as_str()
                .expect("guidance should be text")
                .contains("set cwd to null or a workspace-relative directory")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn proposal_carries_process_action_evidence() {
        let call = pending_call(json!({ "command": "rustc --version", "cwd": null }));
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
        assert_eq!(intent.argv(), shell_command_argv("rustc --version"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn proposal_accepts_multiline_command_for_policy_classification() {
        let call = pending_call(json!({
            "command": "cargo check -p merry-runtime\ncargo test -p merry-runtime",
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
            intent.argv(),
            shell_command_argv("cargo check -p merry-runtime\ncargo test -p merry-runtime")
        );
    }
}
