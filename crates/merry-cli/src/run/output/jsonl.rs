//! Public event and result serialization for headless JSONL output.

use crate::cli_error::{CliError, stdout_error, unexpected};
use merry_core::RuntimeEvent;
use merry_runtime::{AgentLoopResult, AgentLoopStatus};
use tokio::io::{AsyncWrite, AsyncWriteExt};

pub(super) async fn write_public_runtime_event<W>(
    event: &RuntimeEvent,
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let line = serde_json::to_string(event).map_err(unexpected)?;
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(stdout_error)?;
    writer.write_all(b"\n").await.map_err(stdout_error)
}

pub(super) async fn write_agent_loop_result<W>(
    result: &AgentLoopResult,
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let line = match result.status() {
        AgentLoopStatus::Completed => serde_json::json!({
            "type": "agent_loop_result",
            "status": "completed",
            "model_turns_run": result.model_turns_run(),
            "final_output": result.final_output(),
            "final_output_json": result.final_output_json().map(merry_runtime::FinalOutput::json),
        }),
        AgentLoopStatus::Failed { diagnostic } => serde_json::json!({
            "type": "agent_loop_result",
            "status": "failed",
            "model_turns_run": result.model_turns_run(),
            "final_output": result.final_output(),
            "final_output_json": result.final_output_json().map(merry_runtime::FinalOutput::json),
            "diagnostic": {
                "code": diagnostic.code(),
                "message": diagnostic.message(),
            },
        }),
        AgentLoopStatus::Cancelled { diagnostic } => serde_json::json!({
            "type": "agent_loop_result",
            "status": "cancelled",
            "model_turns_run": result.model_turns_run(),
            "final_output": result.final_output(),
            "final_output_json": result.final_output_json().map(merry_runtime::FinalOutput::json),
            "diagnostic": {
                "code": diagnostic.code(),
                "message": diagnostic.message(),
            },
        }),
        AgentLoopStatus::Blocked { reason } => serde_json::json!({
            "type": "agent_loop_result",
            "status": "blocked",
            "model_turns_run": result.model_turns_run(),
            "final_output": result.final_output(),
            "final_output_json": result.final_output_json().map(merry_runtime::FinalOutput::json),
            "blocked_reason": format!("{reason:?}"),
        }),
        _ => serde_json::json!({
            "type": "agent_loop_result",
            "status": "unknown",
            "model_turns_run": result.model_turns_run(),
            "final_output": result.final_output(),
            "final_output_json": result.final_output_json().map(merry_runtime::FinalOutput::json),
        }),
    };
    let line = serde_json::to_string(&line).map_err(unexpected)?;
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(stdout_error)?;
    writer.write_all(b"\n").await.map_err(stdout_error)
}
