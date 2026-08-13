use crate::cli_error::{CliError, unexpected};
use merry_core::{RuntimeJournalEvent, RuntimeJournalPayload, ToolCallResultStatus, ToolName};
use merry_runtime::{AgentLoopStatus, Runtime};
use std::{collections::BTreeMap, fs, path::Path};

use super::{
    CODING_LOOP_LIVE_SMOKE_TARGET_VALUE, CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE,
    CODING_LOOP_SUBAGENT_LIVE_SMOKE_TARGET, CODING_LOOP_TASK_SMOKE_MAX_PATCH_BYTES,
    CodingLoopTaskSmokeFixture, coding_loop_smoke_patched_source,
};
use merry_tool_workspace::{
    CODING_LOOP_PROCESS_TOOL, WORKSPACE_PATCH_TOOL, WORKSPACE_READ_FILE_TOOL,
};

pub(crate) async fn assert_coding_loop_smoke_result(
    runtime: &Runtime,
    result: &merry_runtime::AgentLoopResult,
    smoke_root: &Path,
) -> Result<(), CliError> {
    if result.status() != &AgentLoopStatus::Completed {
        return Err(CliError::Unexpected(format!(
            "coding-loop-smoke did not complete: {:?}",
            result.status()
        )));
    }
    if !runtime.pending_tool_calls().await.is_empty() {
        return Err(CliError::Unexpected(
            "coding-loop-smoke left pending tool calls".to_owned(),
        ));
    }
    assert_coding_loop_smoke_tool_results(result.events())?;

    let patched = fs::read_to_string(smoke_root.join("src/lib.rs")).map_err(unexpected)?;
    if patched != coding_loop_smoke_patched_source() {
        return Err(CliError::Unexpected(
            "coding-loop-smoke fixture was not patched as expected".to_owned(),
        ));
    }
    Ok(())
}

pub(crate) async fn assert_permission_network_smoke_result(
    runtime: &Runtime,
    result: &merry_runtime::AgentLoopResult,
) -> Result<(), CliError> {
    if result.status() != &AgentLoopStatus::Completed {
        return Err(CliError::Unexpected(format!(
            "permission-network-smoke did not complete: {:?}",
            result.status()
        )));
    }
    if !runtime.pending_tool_calls().await.is_empty() {
        return Err(CliError::Unexpected(
            "permission-network-smoke left pending tool calls".to_owned(),
        ));
    }

    let mut pending_by_call_id = BTreeMap::new();
    let mut saw_initial_failed_network_attempt = false;
    let mut saw_approved_successful_network_attempt = false;
    for event in result.events() {
        match &event.payload {
            RuntimeJournalPayload::ToolCallPending { call } => {
                pending_by_call_id.insert(call.id().clone(), call.clone());
            }
            RuntimeJournalPayload::ToolCallResolved { result } => {
                let call = pending_by_call_id.get(result.call_id()).ok_or_else(|| {
                    CliError::Unexpected(format!(
                        "permission-network-smoke resolved unknown tool call {}",
                        result.call_id()
                    ))
                })?;
                let content = runtime
                    .read_artifact_content(result.artifact().id())
                    .await
                    .map_err(unexpected)?;
                let Some(text) = content.as_text() else {
                    continue;
                };
                if !text.contains("\"kind\":\"process_action\"")
                    || !process_artifact_has_command(text, super::PERMISSION_NETWORK_SMOKE_ARGV)
                {
                    continue;
                }

                match (call.name().as_str(), result.status()) {
                    (CODING_LOOP_PROCESS_TOOL, ToolCallResultStatus::Failed) => {
                        saw_initial_failed_network_attempt = true;
                    }
                    ("request_permissions", ToolCallResultStatus::Succeeded) => {
                        saw_approved_successful_network_attempt = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    if !saw_initial_failed_network_attempt {
        return Err(CliError::Unexpected(
            "permission-network-smoke did not observe the default sandbox blocking the initial network lookup".to_owned(),
        ));
    }
    if !saw_approved_successful_network_attempt {
        return Err(CliError::Unexpected(format!(
            "permission-network-smoke did not observe the approved network lookup succeeding{}",
            failed_tool_result_summary(result.events())
                .map(|summary| format!("; {summary}"))
                .unwrap_or_default()
        )));
    }

    Ok(())
}

pub(crate) async fn assert_coding_loop_task_smoke_result(
    runtime: &Runtime,
    result: &merry_runtime::AgentLoopResult,
    smoke_root: &Path,
    fixture: CodingLoopTaskSmokeFixture,
) -> Result<(), CliError> {
    if result.status() != &AgentLoopStatus::Completed {
        return Err(CliError::Unexpected(format!(
            "coding-loop-task-smoke did not complete: {:?}",
            result.status()
        )));
    }
    if !runtime.pending_tool_calls().await.is_empty() {
        return Err(CliError::Unexpected(
            "coding-loop-task-smoke left pending tool calls".to_owned(),
        ));
    }

    let statuses = result
        .events()
        .iter()
        .filter_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => Some(result.status()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if statuses.len() < 5 {
        return Err(CliError::Unexpected(format!(
            "coding-loop-task-smoke expected at least 5 resolved tool calls, saw {}",
            statuses.len()
        )));
    }
    if !statuses.contains(&ToolCallResultStatus::Failed) {
        return Err(CliError::Unexpected(
            "coding-loop-task-smoke did not observe the initial failing verification".to_owned(),
        ));
    }
    if statuses.last().copied() != Some(ToolCallResultStatus::Succeeded) {
        return Err(CliError::Unexpected(format!(
            "coding-loop-task-smoke final verification did not succeed{}",
            failed_tool_result_summary(result.events())
                .map(|summary| format!("; {summary}"))
                .unwrap_or_default()
        )));
    }

    let patched = fs::read_to_string(smoke_root.join("src/lib.rs")).map_err(unexpected)?;
    if patched != fixture.patched_source() {
        return Err(CliError::Unexpected(
            "coding-loop-task-smoke fixture was not patched as expected".to_owned(),
        ));
    }
    assert_coding_loop_task_smoke_uses_small_patch(result.events(), fixture)?;
    Ok(())
}

pub(crate) async fn assert_coding_loop_task_live_smoke_result(
    runtime: &Runtime,
    result: &merry_runtime::AgentLoopResult,
    smoke_root: &Path,
    fixture: CodingLoopTaskSmokeFixture,
) -> Result<(), CliError> {
    if result.status() != &AgentLoopStatus::Completed {
        return Err(CliError::Unexpected(format!(
            "coding-loop-task-live-smoke did not complete: {:?}",
            result.status()
        )));
    }
    if !runtime.pending_tool_calls().await.is_empty() {
        return Err(CliError::Unexpected(
            "coding-loop-task-live-smoke left pending tool calls".to_owned(),
        ));
    }

    let patched = fs::read_to_string(smoke_root.join("src/lib.rs")).map_err(unexpected)?;
    if !fixture.source_satisfies_task(&patched) {
        return Err(CliError::Unexpected(
            "coding-loop-task-live-smoke fixture source does not satisfy the status-text task"
                .to_owned(),
        ));
    }
    assert_coding_loop_task_smoke_uses_small_patch(result.events(), fixture)?;
    Ok(())
}

pub(crate) async fn assert_coding_loop_subagent_live_smoke_result(
    runtime: &Runtime,
    result: &merry_runtime::AgentLoopResult,
    smoke_root: &Path,
) -> Result<(), CliError> {
    if result.status() != &AgentLoopStatus::Completed {
        return Err(CliError::Unexpected(format!(
            "coding-loop-subagent-live-smoke did not complete: {:?}",
            result.status()
        )));
    }
    if !runtime.pending_tool_calls().await.is_empty() {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke left pending tool calls".to_owned(),
        ));
    }

    let patched = fs::read_to_string(smoke_root.join(CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE))
        .map_err(unexpected)?;
    if patched != CODING_LOOP_SUBAGENT_LIVE_SMOKE_TARGET {
        return Err(CliError::Unexpected(format!(
            "coding-loop-subagent-live-smoke fixture `{CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE}` was not patched by the child"
        )));
    }

    assert_coding_loop_subagent_live_smoke_tool_sequence(runtime, result.events()).await
}

pub(crate) fn assert_coding_loop_task_smoke_uses_small_patch(
    events: &[RuntimeJournalEvent],
    fixture: CodingLoopTaskSmokeFixture,
) -> Result<(), CliError> {
    let mut pending_patch_args = BTreeMap::new();
    for event in events {
        if let RuntimeJournalPayload::ToolCallPending { call } = &event.payload
            && call.name().as_str() == WORKSPACE_PATCH_TOOL
        {
            let arguments = call.arguments().as_object();
            let Some(patch) = arguments.get("patch").and_then(serde_json::Value::as_str) else {
                continue;
            };
            pending_patch_args.insert(call.id().clone(), patch);
        }
    }

    let Some(patch) = events.iter().find_map(|event| {
        let RuntimeJournalPayload::ToolCallResolved { result } = &event.payload else {
            return None;
        };
        if result.status() != ToolCallResultStatus::Succeeded {
            return None;
        }
        pending_patch_args.get(result.call_id()).copied()
    }) else {
        return Err(CliError::Unexpected(
            "coding-loop-task-smoke did not observe successful workspace patch arguments"
                .to_owned(),
        ));
    };

    let initial_source = fixture.initial_source();
    let patched_source = fixture.patched_source();
    if patch.contains(&initial_source) || patch.contains(&patched_source) {
        return Err(CliError::Unexpected(
            "coding-loop-task-smoke used whole-file workspace patch text".to_owned(),
        ));
    }

    if !workspace_patch_envelope_is_accepted(patch)
        || !patch.contains("*** Update File: src/lib.rs\n")
    {
        return Err(CliError::Unexpected(
            "coding-loop-task-smoke did not use a workspace patch envelope".to_owned(),
        ));
    }

    if patch.len() > CODING_LOOP_TASK_SMOKE_MAX_PATCH_BYTES {
        return Err(CliError::Unexpected(format!(
            "coding-loop-task-smoke patch payload was too large: {} bytes",
            patch.len()
        )));
    }

    Ok(())
}

fn workspace_patch_envelope_is_accepted(patch: &str) -> bool {
    (patch.starts_with("*** Begin Workspace Patch\n") && patch.ends_with("*** End Workspace Patch"))
        || (patch.starts_with("*** Begin Patch\n") && patch.ends_with("*** End Patch"))
}

pub(crate) async fn assert_coding_loop_subagent_live_smoke_tool_sequence(
    runtime: &Runtime,
    events: &[RuntimeJournalEvent],
) -> Result<(), CliError> {
    let mut pending_by_call_id = BTreeMap::new();
    let mut resolved_tool_names = Vec::new();
    let mut resolved_read_paths = Vec::new();
    let mut spawn_call_id = None;
    let mut wait_call_id = None;
    let mut parent_patch_call_seen = false;
    for event in events {
        match &event.payload {
            RuntimeJournalPayload::ToolCallPending { call } => {
                pending_by_call_id.insert(call.id().clone(), call.clone());
                match call.name().as_str() {
                    "spawn_subagents" => spawn_call_id = Some(call.id().clone()),
                    "wait_subagents" => wait_call_id = Some(call.id().clone()),
                    WORKSPACE_PATCH_TOOL => parent_patch_call_seen = true,
                    _ => {}
                }
            }
            RuntimeJournalPayload::ToolCallResolved { result } => {
                if result.status() != ToolCallResultStatus::Succeeded {
                    return Err(CliError::Unexpected(format!(
                        "coding-loop-subagent-live-smoke tool call {} did not succeed",
                        result.call_id()
                    )));
                }
                let call = pending_by_call_id.get(result.call_id()).ok_or_else(|| {
                    CliError::Unexpected(format!(
                        "coding-loop-subagent-live-smoke resolved unknown tool call {}",
                        result.call_id()
                    ))
                })?;
                resolved_tool_names.push(call.name().as_str().to_owned());
                if call.name().as_str() == WORKSPACE_READ_FILE_TOOL
                    && result.status() == ToolCallResultStatus::Succeeded
                    && let Some(path) = call
                        .arguments()
                        .as_object()
                        .get("path")
                        .and_then(serde_json::Value::as_str)
                {
                    resolved_read_paths.push(path.to_owned());
                }
            }
            _ => {}
        }
    }

    require_live_smoke_tool_name(&resolved_tool_names, "spawn_subagents")?;
    require_live_smoke_tool_name(&resolved_tool_names, "wait_subagents")?;
    require_live_smoke_tool_name(&resolved_tool_names, WORKSPACE_READ_FILE_TOOL)?;
    if !resolved_read_paths
        .iter()
        .any(|path| path == CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE)
    {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke did not read back the child-edited fixture".to_owned(),
        ));
    }
    if parent_patch_call_seen {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke parent should not patch the fixture".to_owned(),
        ));
    }

    let Some(spawn_call_id) = spawn_call_id else {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke did not call spawn_subagents".to_owned(),
        ));
    };
    let Some(wait_call_id) = wait_call_id else {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke did not call wait_subagents".to_owned(),
        ));
    };

    let spawn_call = pending_by_call_id
        .get(&spawn_call_id)
        .ok_or_else(|| CliError::Unexpected("spawn_subagents call was not recorded".to_owned()))?;
    let spawn_args = spawn_call.arguments().as_object();
    let Some(tasks) = spawn_args
        .get("tasks")
        .and_then(serde_json::Value::as_array)
    else {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke spawn args did not include tasks".to_owned(),
        ));
    };
    if tasks.len() != 1 {
        return Err(CliError::Unexpected(format!(
            "coding-loop-subagent-live-smoke expected exactly one child task, saw {}",
            tasks.len()
        )));
    }
    let task = tasks[0].as_object().ok_or_else(|| {
        CliError::Unexpected(
            "coding-loop-subagent-live-smoke child task payload was not an object".to_owned(),
        )
    })?;
    let read_scope = task
        .get("read_scope")
        .and_then(serde_json::Value::as_array)
        .map(|paths| {
            paths
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !read_scope.contains(&CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE) {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke child task should be scoped to read the fixture file"
                .to_owned(),
        ));
    }
    let write_scope = task
        .get("write_scope")
        .and_then(serde_json::Value::as_array)
        .map(|paths| {
            paths
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !write_scope.contains(&CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE) {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke child task should be scoped to the fixture file"
                .to_owned(),
        ));
    }

    let wait_call = pending_by_call_id
        .get(&wait_call_id)
        .ok_or_else(|| CliError::Unexpected("wait_subagents call was not recorded".to_owned()))?;
    let wait_args = wait_call.arguments().as_object();
    let Some(agent_ids) = wait_args
        .get("agent_ids")
        .and_then(serde_json::Value::as_array)
    else {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke wait args did not include agent_ids".to_owned(),
        ));
    };
    if agent_ids.len() != 1 {
        return Err(CliError::Unexpected(format!(
            "coding-loop-subagent-live-smoke expected exactly one waited child, saw {}",
            agent_ids.len()
        )));
    }
    let wait_mode = wait_args.get("mode").and_then(serde_json::Value::as_str);
    if wait_mode != Some("all") {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke wait mode was not `all`".to_owned(),
        ));
    }
    let Some(first_agent_id) = agent_ids.first().and_then(serde_json::Value::as_str) else {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke wait args did not contain a child agent id".to_owned(),
        ));
    };
    if !first_agent_id.starts_with("agent-") {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke child agent id was not runtime-generated".to_owned(),
        ));
    }

    let Some(snapshot) = runtime.subagent_snapshot().await else {
        return Err(CliError::Unexpected(
            "coding-loop-subagent-live-smoke runtime did not expose a subagent snapshot".to_owned(),
        ));
    };
    if snapshot.len() != 1 {
        return Err(CliError::Unexpected(format!(
            "coding-loop-subagent-live-smoke expected one subagent in the snapshot, saw {}",
            snapshot.len()
        )));
    }
    let child = &snapshot[0];
    if child.status.as_str() != "completed" {
        return Err(CliError::Unexpected(format!(
            "coding-loop-subagent-live-smoke child status was not completed: {:?}",
            child.status
        )));
    }
    if child.agent_id.as_str() != first_agent_id {
        return Err(CliError::Unexpected(format!(
            "coding-loop-subagent-live-smoke snapshot child id {} did not match wait target {}",
            child.agent_id.as_str(),
            first_agent_id
        )));
    }

    Ok(())
}

fn assert_coding_loop_smoke_tool_results(events: &[RuntimeJournalEvent]) -> Result<(), CliError> {
    let statuses = events
        .iter()
        .filter_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => Some(result.status()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if statuses.len() != 4 {
        return Err(CliError::Unexpected(format!(
            "coding-loop-smoke expected 4 resolved tool calls, saw {}",
            statuses.len()
        )));
    }
    if statuses
        .iter()
        .any(|status| *status != ToolCallResultStatus::Succeeded)
    {
        return Err(CliError::Unexpected(format!(
            "coding-loop-smoke had a failed tool result{}",
            failed_tool_result_summary(events)
                .map(|summary| format!("; {summary}"))
                .unwrap_or_default()
        )));
    }
    Ok(())
}

fn failed_tool_result_summary(events: &[RuntimeJournalEvent]) -> Option<String> {
    let mut pending_by_call_id = BTreeMap::new();
    for event in events {
        if let RuntimeJournalPayload::ToolCallPending { call } = &event.payload {
            pending_by_call_id.insert(call.id().clone(), call.name().clone());
        }
    }

    events.iter().find_map(|event| {
        let RuntimeJournalPayload::ToolCallResolved { result } = &event.payload else {
            return None;
        };
        if result.status() != ToolCallResultStatus::Failed {
            return None;
        }
        let tool_name = pending_by_call_id
            .get(result.call_id())
            .map_or("<unknown>", ToolName::as_str);
        let diagnostic = result
            .diagnostic()
            .map(|diagnostic| {
                format!(
                    "diagnostic={} message={}",
                    diagnostic.code(),
                    diagnostic.message()
                )
            })
            .unwrap_or_else(|| "diagnostic=<none>".to_owned());
        Some(format!(
            "tool={tool_name} call_id={} {diagnostic}",
            result.call_id()
        ))
    })
}

pub(crate) async fn assert_coding_loop_live_smoke_tool_sequence(
    runtime: &Runtime,
    events: &[RuntimeJournalEvent],
) -> Result<(), CliError> {
    let mut pending_by_call_id = BTreeMap::new();
    let mut resolved_tool_names = Vec::new();
    let mut resolved_artifacts = Vec::new();
    for event in events {
        match &event.payload {
            RuntimeJournalPayload::ToolCallPending { call } => {
                pending_by_call_id.insert(call.id().clone(), call.clone());
            }
            RuntimeJournalPayload::ToolCallResolved { result } => {
                if result.status() != ToolCallResultStatus::Succeeded {
                    return Err(CliError::Unexpected(format!(
                        "live smoke tool call {} did not succeed",
                        result.call_id()
                    )));
                }
                let call = pending_by_call_id.get(result.call_id()).ok_or_else(|| {
                    CliError::Unexpected(format!(
                        "live smoke resolved unknown tool call {}",
                        result.call_id()
                    ))
                })?;
                resolved_tool_names.push(call.name().as_str().to_owned());
                resolved_artifacts.push(result.artifact().id().clone());
            }
            _ => {}
        }
    }

    require_live_smoke_tool_name(&resolved_tool_names, CODING_LOOP_PROCESS_TOOL)?;
    require_live_smoke_tool_name(&resolved_tool_names, WORKSPACE_READ_FILE_TOOL)?;
    require_live_smoke_tool_name(&resolved_tool_names, WORKSPACE_PATCH_TOOL)?;

    let mut process_artifact_texts = Vec::new();
    for artifact_id in &resolved_artifacts {
        let Ok(content) = runtime.read_artifact_content(artifact_id).await else {
            continue;
        };
        let Some(text) = content.as_text() else {
            continue;
        };
        if text.contains("\"kind\":\"process_action\"") {
            process_artifact_texts.push(text.to_owned());
        }
    }
    let inspected = process_artifact_texts.iter().any(|text| {
        process_artifact_has_command(text, ["rg", "--files"]) && text.contains("src/lib.rs")
    });
    let verified = process_artifact_texts.iter().any(|text| {
        process_artifact_has_command(text, ["rg", CODING_LOOP_LIVE_SMOKE_TARGET_VALUE])
            && text.contains(CODING_LOOP_LIVE_SMOKE_TARGET_VALUE)
    });
    if !inspected {
        return Err(CliError::Unexpected(
            "live smoke did not resolve a real rg --files process call".to_owned(),
        ));
    }
    if !verified {
        return Err(CliError::Unexpected(format!(
            "live smoke did not resolve a real rg {CODING_LOOP_LIVE_SMOKE_TARGET_VALUE} verification call"
        )));
    }

    Ok(())
}

pub(crate) async fn assert_coding_loop_task_live_smoke_tool_sequence(
    runtime: &Runtime,
    events: &[RuntimeJournalEvent],
    fixture: CodingLoopTaskSmokeFixture,
) -> Result<(), CliError> {
    let mut pending_by_call_id = BTreeMap::new();
    let mut resolved_tool_names = Vec::new();
    let mut resolved_artifacts = Vec::new();
    let mut resolved_read_paths = Vec::new();
    for event in events {
        match &event.payload {
            RuntimeJournalPayload::ToolCallPending { call } => {
                pending_by_call_id.insert(call.id().clone(), call.clone());
            }
            RuntimeJournalPayload::ToolCallResolved { result } => {
                let call = pending_by_call_id.get(result.call_id()).ok_or_else(|| {
                    CliError::Unexpected(format!(
                        "task live smoke resolved unknown tool call {}",
                        result.call_id()
                    ))
                })?;
                resolved_tool_names.push(call.name().as_str().to_owned());
                if result.status() == ToolCallResultStatus::Succeeded
                    && call.name().as_str() == WORKSPACE_READ_FILE_TOOL
                    && let Some(path) = call
                        .arguments()
                        .as_object()
                        .get("path")
                        .and_then(serde_json::Value::as_str)
                {
                    resolved_read_paths.push(path.to_owned());
                }
                resolved_artifacts.push(result.artifact().id().clone());
            }
            _ => {}
        }
    }

    require_live_smoke_tool_name(&resolved_tool_names, CODING_LOOP_PROCESS_TOOL)?;
    require_live_smoke_tool_name(&resolved_tool_names, WORKSPACE_READ_FILE_TOOL)?;
    require_live_smoke_tool_name(&resolved_tool_names, WORKSPACE_PATCH_TOOL)?;

    if !resolved_read_paths.iter().any(|path| path == "AGENTS.md") {
        return Err(CliError::Unexpected(
            "task live smoke did not read AGENTS.md before completing".to_owned(),
        ));
    }
    if !resolved_read_paths.iter().any(|path| path == "src/lib.rs") {
        return Err(CliError::Unexpected(
            "task live smoke did not read src/lib.rs before patching".to_owned(),
        ));
    }

    let mut saw_cargo_check = false;
    let mut saw_cargo_test = false;
    for artifact_id in &resolved_artifacts {
        let Ok(content) = runtime.read_artifact_content(artifact_id).await else {
            continue;
        };
        let Some(text) = content.as_text() else {
            continue;
        };
        if !text.contains("\"kind\":\"process_action\"") || !text.contains("\"ok\":true") {
            continue;
        }
        saw_cargo_check |=
            process_artifact_has_cargo_package_command(text, "check", fixture.package_name());
        saw_cargo_test |=
            process_artifact_has_cargo_package_command(text, "test", fixture.package_name());
    }
    if !saw_cargo_check {
        return Err(CliError::Unexpected(
            "task live smoke did not observe a successful cargo check for the fixture package"
                .to_owned(),
        ));
    }
    if !saw_cargo_test {
        return Err(CliError::Unexpected(
            "task live smoke did not observe a successful cargo test for the fixture package"
                .to_owned(),
        ));
    }

    Ok(())
}

fn require_live_smoke_tool_name(names: &[String], required: &str) -> Result<(), CliError> {
    if names.iter().any(|name| name == required) {
        Ok(())
    } else {
        Err(CliError::Unexpected(format!(
            "live smoke did not resolve required tool `{required}`"
        )))
    }
}

fn process_artifact_has_command<const N: usize>(text: &str, expected: [&str; N]) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    let argv = value
        .get("intent")
        .and_then(|intent| intent.get("argv"))
        .and_then(serde_json::Value::as_array);
    let expected_command = expected.join(" ");
    if let Some(argv) = argv {
        return matches!(
            argv.as_slice(),
            [shell, flag, command]
                if shell.as_str() == Some("bash")
                    && flag.as_str() == Some("-lc")
                    && command.as_str() == Some(expected_command.as_str())
        );
    }
    value
        .get("intent")
        .and_then(|intent| intent.get("command"))
        .and_then(serde_json::Value::as_str)
        == Some(expected_command.as_str())
}

fn process_artifact_has_cargo_package_command(text: &str, command: &str, package: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    let argv = value
        .get("intent")
        .and_then(|intent| intent.get("argv"))
        .and_then(serde_json::Value::as_array);
    let expected_command = format!("cargo {command} -p {package}");
    if let Some(argv) = argv {
        return matches!(
            argv.as_slice(),
            [shell, flag, actual_command]
                if shell.as_str() == Some("bash")
                    && flag.as_str() == Some("-lc")
                    && actual_command.as_str() == Some(expected_command.as_str())
        );
    }
    value
        .get("intent")
        .and_then(|intent| intent.get("command"))
        .and_then(serde_json::Value::as_str)
        == Some(expected_command.as_str())
}
