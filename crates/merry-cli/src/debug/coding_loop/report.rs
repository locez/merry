use crate::{
    CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE, CODING_LOOP_SUBAGENT_LIVE_SMOKE_TARGET, CliError,
    stdout_error, unexpected, write_runtime_event_slice,
};
use merry_core::{RuntimeEvent, RuntimeEventKind};
use merry_runtime::{ArtifactContent, AutomaticCompactionConfig, Runtime};
use std::{fs, path::Path};
use tokio::io::{AsyncWrite, AsyncWriteExt};

async fn write_subagent_snapshot_summary<W>(
    runtime: &Runtime,
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let line = serde_json::json!({
        "type": "subagent_snapshot",
        "agents": runtime.subagent_snapshot().await,
    });
    let line = serde_json::to_string(&line).map_err(unexpected)?;
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(stdout_error)?;
    writer.write_all(b"\n").await.map_err(stdout_error)
}

async fn write_subagent_fixture_summary<W>(
    smoke_root: &Path,
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let content = fs::read_to_string(smoke_root.join(CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE))
        .unwrap_or_else(|error| format!("unreadable fixture file: {error}"));
    let line = serde_json::json!({
        "type": "subagent_live_smoke_fixture",
        "path": CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE,
        "content": content,
        "target_matched": content == CODING_LOOP_SUBAGENT_LIVE_SMOKE_TARGET,
    });
    let line = serde_json::to_string(&line).map_err(unexpected)?;
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(stdout_error)?;
    writer.write_all(b"\n").await.map_err(stdout_error)
}

async fn write_compaction_config_summary<W>(
    automatic_compaction: AutomaticCompactionConfig,
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let policy = automatic_compaction.policy();
    let line = serde_json::json!({
        "type": "runtime_compaction_config_summary",
        "auto_compaction_enabled": automatic_compaction.is_enabled(),
        "target_output_tokens": policy.target_output_tokens(),
        "model_output_token_limit": policy.model_output_token_limit(),
        "max_accepted_output_bytes": policy.max_accepted_output_bytes(),
        "retained_raw_tail_items": policy.retained_raw_tail_items(),
        "max_ref_excerpt_bytes": policy.max_ref_excerpt_bytes(),
        "max_carried_prior_refs": policy.max_carried_prior_refs(),
    });
    let line = serde_json::to_string(&line).map_err(unexpected)?;
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(stdout_error)?;
    writer.write_all(b"\n").await.map_err(stdout_error)?;
    Ok(())
}

async fn write_compaction_summary<W>(runtime: &Runtime, writer: &mut W) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let summary = runtime.compacted_checkpoint_summary().await;
    let line = match summary {
        Some(summary) => serde_json::json!({
            "type": "runtime_compaction_summary",
            "checkpoint_present": true,
            "citation_backed": summary.citation_backed(),
            "checkpoint_id": summary.checkpoint_id().map(merry_runtime::CheckpointId::as_str),
            "claim_count": summary.claim_count(),
            "ref_count": summary.ref_count(),
        }),
        None => serde_json::json!({
            "type": "runtime_compaction_summary",
            "checkpoint_present": false,
            "citation_backed": false,
            "checkpoint_id": null,
            "claim_count": 0,
            "ref_count": 0,
        }),
    };
    let line = serde_json::to_string(&line).map_err(unexpected)?;
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(stdout_error)?;
    writer.write_all(b"\n").await.map_err(stdout_error)?;
    Ok(())
}

async fn write_process_artifact_previews<W>(
    runtime: &Runtime,
    events: &[RuntimeEvent],
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    for event in events {
        let RuntimeEventKind::ToolCallResolved { result } = &event.kind else {
            continue;
        };
        let content = runtime
            .read_artifact_content(result.artifact().id())
            .await
            .map_err(unexpected)?;
        let ArtifactContent::Json(content) = content else {
            continue;
        };
        let value = serde_json::from_str::<serde_json::Value>(&content).map_err(unexpected)?;
        if value.pointer("/kind").and_then(serde_json::Value::as_str) != Some("process_action") {
            continue;
        }
        let preview = serde_json::json!({
            "type": "process_artifact_preview",
            "artifact_id": result.artifact().id().as_str(),
            "call_id": result.call_id().as_str(),
            "status": value.pointer("/status"),
            "stdout": value.pointer("/stdout/text").and_then(serde_json::Value::as_str).unwrap_or(""),
            "stderr": value.pointer("/stderr/text").and_then(serde_json::Value::as_str).unwrap_or(""),
            "stdout_truncated": value.pointer("/stdout/truncated").and_then(serde_json::Value::as_bool).unwrap_or(false),
            "stderr_truncated": value.pointer("/stderr/truncated").and_then(serde_json::Value::as_bool).unwrap_or(false),
        });
        let line = serde_json::to_string(&preview).map_err(unexpected)?;
        writer
            .write_all(line.as_bytes())
            .await
            .map_err(stdout_error)?;
        writer.write_all(b"\n").await.map_err(stdout_error)?;
    }
    Ok(())
}

pub(crate) async fn write_coding_loop_task_live_smoke_report<W>(
    runtime: &Runtime,
    automatic_compaction: AutomaticCompactionConfig,
    passed: bool,
    events: &[RuntimeEvent],
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let header = if passed {
        b"coding-loop-task-live-smoke: ok\n".as_slice()
    } else {
        b"coding-loop-task-live-smoke: failed\n".as_slice()
    };
    writer.write_all(header).await.map_err(stdout_error)?;
    write_runtime_event_slice(events, writer).await?;
    write_compaction_config_summary(automatic_compaction, writer).await?;
    write_compaction_summary(runtime, writer).await?;
    write_process_artifact_previews(runtime, events, writer).await
}

pub(crate) async fn write_permission_network_smoke_report<W>(
    runtime: &Runtime,
    events: &[RuntimeEvent],
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    writer
        .write_all(b"permission-network-smoke: ok\n")
        .await
        .map_err(stdout_error)?;
    write_runtime_event_slice(events, writer).await?;
    write_process_artifact_previews(runtime, events, writer).await
}

pub(crate) async fn write_coding_loop_subagent_live_smoke_report<W>(
    runtime: &Runtime,
    passed: bool,
    events: &[RuntimeEvent],
    smoke_root: &Path,
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let header = if passed {
        b"coding-loop-subagent-live-smoke: ok\n".as_slice()
    } else {
        b"coding-loop-subagent-live-smoke: failed\n".as_slice()
    };
    writer.write_all(header).await.map_err(stdout_error)?;
    write_runtime_event_slice(events, writer).await?;
    write_subagent_snapshot_summary(runtime, writer).await?;
    write_subagent_fixture_summary(smoke_root, writer).await
}
