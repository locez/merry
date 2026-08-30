//! Strict provider-neutral JSON conversion at the Python boundary.

use merry::{
    AgentLoopBlockedReason, AgentLoopStatus, RunResult,
    binding::{
        OwnedAgentRunMessage, OwnedToolInvocationBatch, ToolInvocation, ToolInvocationContent,
        ToolInvocationResult, ToolInvocationSubmission,
    },
};
use merry_core::{
    ArtifactRef, ErrorInfo, RuntimeEvent, SessionUsage, ToolCallArguments, ToolCallBatchId,
    ToolCallId, ToolInputSchema, ToolName,
};
use schemars::Schema;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum NativeRunMessage<'a> {
    Event {
        event: &'a RuntimeEvent,
    },
    ToolInvocations {
        #[serde(flatten)]
        batch: NativeToolInvocationBatch<'a>,
    },
}

#[derive(Serialize)]
struct NativeToolInvocationBatch<'a> {
    id: &'a ToolCallBatchId,
    invocations: Vec<NativeToolInvocation<'a>>,
}

#[derive(Serialize)]
struct NativeToolInvocation<'a> {
    id: &'a ToolCallId,
    name: &'a ToolName,
    arguments: &'a ToolCallArguments,
}

pub(crate) fn message_to_json(message: &OwnedAgentRunMessage) -> Result<String, serde_json::Error> {
    let wire = match message {
        OwnedAgentRunMessage::Event(event) => NativeRunMessage::Event {
            event: event.as_ref(),
        },
        OwnedAgentRunMessage::ToolInvocations { batch } => NativeRunMessage::ToolInvocations {
            batch: tool_invocation_batch(batch),
        },
        _ => return Err(unsupported_protocol_variant("agent run message")),
    };
    serde_json::to_string(&wire)
}

fn tool_invocation_batch(batch: &OwnedToolInvocationBatch) -> NativeToolInvocationBatch<'_> {
    NativeToolInvocationBatch {
        id: batch.id(),
        invocations: batch.invocations().iter().map(tool_invocation).collect(),
    }
}

fn tool_invocation(invocation: &ToolInvocation) -> NativeToolInvocation<'_> {
    NativeToolInvocation {
        id: invocation.id(),
        name: invocation.name(),
        arguments: invocation.arguments(),
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum NativeRunStatus<'a> {
    Completed,
    Failed { diagnostic: &'a ErrorInfo },
    Cancelled { diagnostic: &'a ErrorInfo },
    Blocked { reason: NativeBlockedReason },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum NativeBlockedReason {
    MaxModelTurnsReached { max_model_turns: usize },
    MultiplePendingToolCalls { pending_count: usize },
    StepCompletedWithPendingToolCall { pending_count: usize },
    StepEndedWithoutTerminalEvent,
    FinalOutputToolNotCalled,
    BridgeToolCallRequested { call_id: String, tool_name: String },
}

#[derive(Serialize)]
struct NativeRunResult<'a> {
    status: NativeRunStatus<'a>,
    events: &'a [RuntimeEvent],
    model_turns_run: usize,
    final_output: Option<&'a str>,
    final_output_json: Option<NativeFinalOutput<'a>>,
    session_usage: Option<&'a SessionUsage>,
}

#[derive(Serialize)]
struct NativeFinalOutput<'a> {
    call_id: &'a ToolCallId,
    artifact: &'a ArtifactRef,
    json: &'a str,
}

pub(crate) fn run_result_to_json(result: &RunResult) -> Result<String, serde_json::Error> {
    let final_output_json = result.final_output_json().map(|output| NativeFinalOutput {
        call_id: output.call_id(),
        artifact: output.artifact(),
        json: output.json(),
    });
    let wire = NativeRunResult {
        status: status_to_wire(result.status())?,
        events: result.events(),
        model_turns_run: result.model_turns_run(),
        final_output: result.final_output(),
        final_output_json,
        session_usage: result.session_usage(),
    };
    serde_json::to_string(&wire)
}

fn status_to_wire(status: &AgentLoopStatus) -> Result<NativeRunStatus<'_>, serde_json::Error> {
    match status {
        AgentLoopStatus::Completed => Ok(NativeRunStatus::Completed),
        AgentLoopStatus::Failed { diagnostic } => Ok(NativeRunStatus::Failed { diagnostic }),
        AgentLoopStatus::Cancelled { diagnostic } => Ok(NativeRunStatus::Cancelled { diagnostic }),
        AgentLoopStatus::Blocked { reason } => Ok(NativeRunStatus::Blocked {
            reason: blocked_reason_to_wire(reason)?,
        }),
        _ => Err(unsupported_protocol_variant("agent loop status")),
    }
}

fn blocked_reason_to_wire(
    reason: &AgentLoopBlockedReason,
) -> Result<NativeBlockedReason, serde_json::Error> {
    match reason {
        AgentLoopBlockedReason::MaxModelTurnsReached { max_model_turns } => {
            Ok(NativeBlockedReason::MaxModelTurnsReached {
                max_model_turns: *max_model_turns,
            })
        }
        AgentLoopBlockedReason::MultiplePendingToolCalls { pending_count } => {
            Ok(NativeBlockedReason::MultiplePendingToolCalls {
                pending_count: *pending_count,
            })
        }
        AgentLoopBlockedReason::StepCompletedWithPendingToolCall { pending_count } => {
            Ok(NativeBlockedReason::StepCompletedWithPendingToolCall {
                pending_count: *pending_count,
            })
        }
        AgentLoopBlockedReason::StepEndedWithoutTerminalEvent => {
            Ok(NativeBlockedReason::StepEndedWithoutTerminalEvent)
        }
        AgentLoopBlockedReason::FinalOutputToolNotCalled => {
            Ok(NativeBlockedReason::FinalOutputToolNotCalled)
        }
        AgentLoopBlockedReason::BridgeToolCallRequested { call_id, tool_name } => {
            Ok(NativeBlockedReason::BridgeToolCallRequested {
                call_id: call_id.to_string(),
                tool_name: tool_name.to_string(),
            })
        }
        _ => Err(unsupported_protocol_variant("agent loop blocked reason")),
    }
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum NativeToolResult {
    Succeeded {
        call_id: String,
        content: NativeToolContent,
    },
    Failed {
        call_id: String,
        content: NativeToolContent,
        diagnostic: ErrorInfo,
    },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum NativeToolContent {
    Text { text: String },
    Json { json: String },
}

pub(crate) fn parse_tool_results(payload: &str) -> Result<Vec<ToolInvocationResult>, String> {
    let values = serde_json::from_str::<Vec<NativeToolResult>>(payload)
        .map_err(|error| format!("tool result payload is invalid: {error}"))?;
    values
        .into_iter()
        .map(tool_result)
        .collect::<Result<Vec<_>, _>>()
}

fn tool_result(value: NativeToolResult) -> Result<ToolInvocationResult, String> {
    match value {
        NativeToolResult::Succeeded { call_id, content } => Ok(ToolInvocationResult::succeeded(
            parse_call_id(&call_id)?,
            parse_content(content)?,
        )),
        NativeToolResult::Failed {
            call_id,
            content,
            diagnostic,
        } => Ok(ToolInvocationResult::failed(
            parse_call_id(&call_id)?,
            parse_content(content)?,
            diagnostic,
        )),
    }
}

fn parse_call_id(value: &str) -> Result<ToolCallId, String> {
    ToolCallId::new(value).map_err(|error| format!("tool call id is invalid: {error}"))
}

fn parse_content(content: NativeToolContent) -> Result<ToolInvocationContent, String> {
    match content {
        NativeToolContent::Text { text } => Ok(ToolInvocationContent::text(text)),
        NativeToolContent::Json { json } => {
            ToolInvocationContent::json(json).map_err(|error| error.to_string())
        }
    }
}

pub(crate) fn parse_batch_id(value: &str) -> Result<ToolCallBatchId, String> {
    ToolCallBatchId::new(value)
        .map_err(|error| format!("tool invocation batch id is invalid: {error}"))
}

pub(crate) fn parse_input_schema(value: &str) -> Result<ToolInputSchema, String> {
    let schema = serde_json::from_str::<Schema>(value)
        .map_err(|error| format!("tool input schema is invalid JSON: {error}"))?;
    ToolInputSchema::new(schema).map_err(|error| format!("tool input schema is invalid: {error}"))
}

#[derive(Serialize)]
struct NativeSubmission<'a> {
    status: &'a str,
}

pub(crate) fn submission_to_json(
    submission: ToolInvocationSubmission,
) -> Result<String, serde_json::Error> {
    let status = match submission {
        ToolInvocationSubmission::Accepted => "accepted",
        ToolInvocationSubmission::RejectedAndRecorded => "rejected_and_recorded",
        _ => return Err(unsupported_protocol_variant("tool invocation submission")),
    };
    serde_json::to_string(&NativeSubmission { status })
}

fn unsupported_protocol_variant(kind: &str) -> serde_json::Error {
    serde_json::Error::io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("unsupported provider-neutral {kind} variant"),
    ))
}
