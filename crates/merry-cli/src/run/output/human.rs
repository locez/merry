//! Human-readable progress and final-result formatting without lifecycle policy.

use crate::{
    cli_error::{CliError, stdout_error},
    tool_display::format_tool_call_progress,
};
use merry_core::{ErrorInfo, RuntimeEvent, ToolCallResultStatus};
use merry_runtime::{AgentLoopBlockedReason, AgentLoopResult, AgentLoopStatus};
use tokio::io::{AsyncWrite, AsyncWriteExt};

pub(super) async fn write_agent_loop_summary_to<W>(
    result: &AgentLoopResult,
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    if let Some(output) = result.final_output() {
        writer
            .write_all(output.as_bytes())
            .await
            .map_err(stdout_error)?;
        if !output.ends_with('\n') {
            writer.write_all(b"\n").await.map_err(stdout_error)?;
        }
    } else if let Some(output) = result.final_output_json() {
        writer
            .write_all(output.json().as_bytes())
            .await
            .map_err(stdout_error)?;
        writer.write_all(b"\n").await.map_err(stdout_error)?;
    } else {
        write_agent_loop_status_summary_to(result, writer).await?;
    }
    Ok(())
}

async fn write_agent_loop_status_summary_to<W>(
    result: &AgentLoopResult,
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let summary = match result.status() {
        AgentLoopStatus::Completed => "status: completed\n".to_owned(),
        AgentLoopStatus::Failed { diagnostic } => format_diagnostic_status("failed", diagnostic),
        AgentLoopStatus::Cancelled { diagnostic } => {
            format_diagnostic_status("cancelled", diagnostic)
        }
        AgentLoopStatus::Blocked { reason } => {
            format!(
                "status: blocked\nreason: {}\n",
                format_blocked_reason(reason)
            )
        }
        _ => format!("status: {:?}\n", result.status()),
    };
    writer
        .write_all(summary.as_bytes())
        .await
        .map_err(stdout_error)
}

fn format_diagnostic_status(status: &str, diagnostic: &ErrorInfo) -> String {
    format!(
        "status: {status}\nerror: {}: {}\n",
        diagnostic.code(),
        diagnostic.message()
    )
}

fn format_blocked_reason(reason: &AgentLoopBlockedReason) -> String {
    match reason {
        AgentLoopBlockedReason::MaxModelTurnsReached { max_model_turns } => {
            format!("max model turns reached ({max_model_turns})")
        }
        AgentLoopBlockedReason::MultiplePendingToolCalls { pending_count } => {
            format!("multiple pending tool calls ({pending_count})")
        }
        AgentLoopBlockedReason::StepCompletedWithPendingToolCall { pending_count } => {
            format!("step completed with pending tool calls ({pending_count})")
        }
        AgentLoopBlockedReason::StepEndedWithoutTerminalEvent => {
            "step ended without a terminal event".to_owned()
        }
        AgentLoopBlockedReason::FinalOutputToolNotCalled => {
            "final output tool was not called".to_owned()
        }
        AgentLoopBlockedReason::BridgeToolCallRequested { call_id, tool_name } => {
            format!(
                "bridge tool call requested: {} ({})",
                tool_name.as_str(),
                call_id.as_str()
            )
        }
        _ => format!("{reason:?}"),
    }
}

pub(super) async fn write_human_progress_event<W>(
    event: &RuntimeEvent,
    pending_commentary: &mut Option<String>,
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    match event {
        RuntimeEvent::AssistantMessage { text, .. } => {
            *pending_commentary = Some(text.clone());
        }
        RuntimeEvent::ToolCallStarted { call, .. } => {
            if let Some(commentary) = pending_commentary.take() {
                write_progress_commentary(&commentary, writer).await?;
            }
            write_human_progress_line(writer, format_tool_call_progress("tool", call)).await?;
        }
        RuntimeEvent::ToolCallBatchStarted { batch, .. } => {
            if let Some(commentary) = pending_commentary.take() {
                write_progress_commentary(&commentary, writer).await?;
            }
            for call in batch.calls() {
                write_human_progress_line(writer, format_tool_call_progress("tool", call)).await?;
            }
        }
        RuntimeEvent::ToolCallFinished { result, .. }
            if result.status() == ToolCallResultStatus::Failed =>
        {
            let line = result.diagnostic().map_or_else(
                || "tool failed".to_owned(),
                |diagnostic| {
                    format!(
                        "tool failed: {}: {}",
                        diagnostic.code(),
                        diagnostic.message()
                    )
                },
            );
            write_human_progress_line(writer, line).await?;
        }
        RuntimeEvent::ModelRetryScheduled {
            attempt,
            next_attempt,
            max_attempts,
            delay_ms,
            error_kind,
            ..
        } => {
            let line = format!(
                "model retry: attempt {attempt}/{max_attempts} failed with {error_kind}; retrying attempt {next_attempt}/{max_attempts} in {}",
                format_delay_ms(*delay_ms)
            );
            write_human_progress_line(writer, line).await?;
        }
        RuntimeEvent::ModelRetryExhausted {
            attempts_run,
            max_attempts,
            error_kind,
            ..
        } => {
            let line = format!(
                "model retry exhausted: {attempts_run}/{max_attempts} attempts failed with {error_kind}"
            );
            write_human_progress_line(writer, line).await?;
        }
        RuntimeEvent::StepCompleted { .. }
        | RuntimeEvent::RunFailed { .. }
        | RuntimeEvent::RunCancelled { .. }
        | RuntimeEvent::FinalOutputRecorded { .. } => {
            *pending_commentary = None;
        }
        _ => {}
    }

    Ok(())
}

async fn write_progress_commentary<W>(commentary: &str, writer: &mut W) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let commentary = commentary.trim();
    if commentary.is_empty() {
        return Ok(());
    }

    writer
        .write_all(commentary.as_bytes())
        .await
        .map_err(stdout_error)?;
    writer.write_all(b"\n\n").await.map_err(stdout_error)?;
    writer.flush().await.map_err(stdout_error)
}

async fn write_human_progress_line<W>(writer: &mut W, line: String) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let line = line.trim();
    if line.is_empty() {
        return Ok(());
    }

    writer
        .write_all(line.as_bytes())
        .await
        .map_err(stdout_error)?;
    writer.write_all(b"\n\n").await.map_err(stdout_error)?;
    writer.flush().await.map_err(stdout_error)
}

fn format_delay_ms(delay_ms: u64) -> String {
    if delay_ms >= 1000 && delay_ms.is_multiple_of(1000) {
        format!("{}s", delay_ms / 1000)
    } else {
        format!("{delay_ms}ms")
    }
}
