use crate::cli_error::{CliError, debug_openai_usage_error, stdout_error, unexpected};
use crate::coding_runtime::{
    ActionProcessBackend, CodingLoopRuntimeOptions, action_process_runner,
    build_coding_loop_runtime, coding_agent_loop_config,
    coding_loop_smoke_admission_from_current_process, coding_loop_workspace_roots,
    with_workspace_coding_loop_profile, workspace_tools_config,
};
use crate::config::{self, MerryConfig};
use crate::debug::CodingLoopTaskSmokeTask;
use crate::provider_config::{
    OpenAiRuntimeConfig, openai_approval_review_provider, openai_context_compaction_provider,
};
use crate::runtime_config::{automatic_compaction_config, subagents_config};
use crate::sandbox::ChildHandoff as SandboxChildHandoff;
use crate::{
    CODING_LOOP_LIVE_SMOKE_INITIAL_VALUE, CODING_LOOP_LIVE_SMOKE_SESSION_ID,
    CODING_LOOP_LIVE_SMOKE_TARGET_VALUE, CODING_LOOP_PROCESS_TOOL, CODING_LOOP_SMOKE_SESSION_ID,
    CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE, CODING_LOOP_SUBAGENT_LIVE_SMOKE_INITIAL,
    CODING_LOOP_SUBAGENT_LIVE_SMOKE_SESSION_ID, CODING_LOOP_SUBAGENT_LIVE_SMOKE_TARGET,
    CODING_LOOP_TASK_LIVE_SMOKE_SESSION_ID, CODING_LOOP_TASK_SMOKE_MAX_PATCH_BYTES,
    CODING_LOOP_TASK_SMOKE_SESSION_ID, PERMISSION_NETWORK_SMOKE_ARGV,
    PERMISSION_NETWORK_SMOKE_SESSION_ID, WORKSPACE_PATCH_TOOL, WORKSPACE_READ_FILE_TOOL,
};
use merry_core::{RuntimeEvent, RuntimeEventKind, SessionId, ToolCallResultStatus, ToolName};
use merry_llm::{GenerationConfig, ModelName};
use merry_provider_openai::OpenAiProvider;
#[cfg(test)]
use merry_runtime::RuntimeModelRole;
use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, AgentLoopConfig, AgentLoopStatus,
    AutomaticCompactionConfig, BwrapPermissionedProcessRunnerFactory, BwrapProcessRunner,
    PermissionedProcessRunnerFactory, ProcessRunner, Runtime, StepContext, StepInput,
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::io::{AsyncWriteExt, BufWriter};

use merry_tool_workspace::WorkspaceCodingLoopProfile;

mod fixture;
mod provider;
mod report;

pub(crate) use fixture::{
    CodingLoopTaskSmokeFixture, coding_loop_smoke_patched_source,
    prepare_coding_loop_smoke_fixture, prepare_coding_loop_subagent_live_smoke_fixture,
    prepare_coding_loop_task_fixture,
};
#[cfg(test)]
pub(crate) use fixture::{coding_loop_smoke_initial_source, coding_loop_task_fixture_manifest};
pub(crate) use provider::{CodingLoopSmokeProvider, CodingLoopTaskSmokeProvider};
#[cfg(test)]
pub(crate) use provider::{
    PermissionNetworkSmokeProvider, PermissionNetworkSmokeReviewProvider, coding_loop_process_call,
    coding_loop_tool_call, coding_loop_workspace_call,
};
pub(crate) use report::{
    write_coding_loop_subagent_live_smoke_report, write_coding_loop_task_live_smoke_report,
    write_permission_network_smoke_report,
};

pub(crate) async fn run_smoke(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    merry_config: Option<&MerryConfig>,
) -> Result<(), CliError> {
    let Some(admission) =
        coding_loop_smoke_admission_from_current_process(sandbox_child_handoff).await
    else {
        return Err(coding_loop_smoke_requires_sandbox_error(
            "coding-loop-smoke",
        ));
    };

    let smoke_root = prepare_coding_loop_smoke_fixture("coding-loop-smoke")?;
    let backend = action_process_runner(&smoke_root, merry_config)?;
    let runtime = build_coding_loop_smoke_runtime(
        &smoke_root,
        None,
        admission,
        backend.runner(),
        Some(backend.permissioned_factory()),
        automatic_compaction_config(merry_config).map_err(unexpected)?,
    )?;

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Run the sandboxed coding-loop smoke.").map_err(unexpected)?,
            StepContext::default(),
            coding_agent_loop_config()?,
        )
        .await
        .map_err(unexpected)?;

    assert_coding_loop_smoke_result(&runtime, &result, &smoke_root).await?;

    let mut writer = BufWriter::new(tokio::io::stdout());
    writer
        .write_all(b"coding-loop-smoke: ok\n")
        .await
        .map_err(stdout_error)?;
    writer.flush().await.map_err(stdout_error)
}

pub(crate) async fn run_permission_network_smoke(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    model_flag: Option<&str>,
    max_output_tokens: u64,
    merry_config: Option<&MerryConfig>,
) -> Result<(), CliError> {
    let Some(admission) =
        coding_loop_smoke_admission_from_current_process(sandbox_child_handoff).await
    else {
        return Err(coding_loop_smoke_requires_sandbox_error(
            "permission-network-smoke",
        ));
    };

    let config = crate::debug::openai::debug_config(model_flag, merry_config)?;
    let smoke_root = prepare_coding_loop_smoke_fixture(PERMISSION_NETWORK_SMOKE_SESSION_ID)?;
    let backend = permission_network_smoke_process_runner(&smoke_root, merry_config)?;
    let runtime = build_permission_network_smoke_runtime(
        &smoke_root,
        admission,
        config,
        backend.runner(),
        backend.permissioned_factory(),
        automatic_compaction_config(merry_config).map_err(unexpected)?,
    )?;
    let generation_config =
        GenerationConfig::new(Some(max_output_tokens), false).map_err(debug_openai_usage_error)?;
    let context = StepContext::default().with_generation_config(generation_config);

    let result = runtime
        .run_agent_loop(
            StepInput::user_text(&permission_network_live_smoke_task()).map_err(unexpected)?,
            context,
            AgentLoopConfig::new(6).map_err(unexpected)?,
        )
        .await
        .map_err(unexpected)?;

    assert_permission_network_smoke_result(&runtime, &result).await?;

    let mut writer = BufWriter::new(tokio::io::stdout());
    write_permission_network_smoke_report(&runtime, result.events(), &mut writer).await?;
    writer.flush().await.map_err(stdout_error)
}

pub(crate) async fn run_live_smoke(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    model_flag: Option<&str>,
    max_output_tokens: u64,
    merry_config: Option<&MerryConfig>,
) -> Result<(), CliError> {
    let Some(admission) =
        coding_loop_smoke_admission_from_current_process(sandbox_child_handoff).await
    else {
        return Err(coding_loop_smoke_requires_sandbox_error(
            "coding-loop-live-smoke",
        ));
    };
    let config = crate::debug::openai::debug_config(model_flag, merry_config)?;
    let smoke_root = prepare_coding_loop_smoke_fixture("coding-loop-live-smoke")?;
    let skill_roots = merry_config
        .map(MerryConfig::skill_roots)
        .transpose()
        .map_err(unexpected)?
        .unwrap_or_default();
    let backend = action_process_runner(&smoke_root, merry_config)?;
    let runtime = build_coding_loop_live_smoke_runtime(
        &smoke_root,
        admission,
        config,
        backend.runner(),
        Some(backend.permissioned_factory()),
        CodingLoopLiveRuntimeOptions {
            automatic_compaction: automatic_compaction_config(merry_config).map_err(unexpected)?,
            skill_roots,
            subagents: subagents_config(merry_config).map_err(unexpected)?,
        },
    )?;
    let generation_config =
        GenerationConfig::new(Some(max_output_tokens), false).map_err(debug_openai_usage_error)?;
    let context = StepContext::default().with_generation_config(generation_config);

    let result = runtime
        .run_agent_loop(
            StepInput::user_text(&coding_loop_live_smoke_task(None)).map_err(unexpected)?,
            context,
            coding_agent_loop_config()?,
        )
        .await
        .map_err(unexpected)?;

    assert_coding_loop_smoke_result(&runtime, &result, &smoke_root).await?;
    assert_coding_loop_live_smoke_tool_sequence(&runtime, result.events()).await?;

    let mut writer = BufWriter::new(tokio::io::stdout());
    writer
        .write_all(b"coding-loop-live-smoke: ok\n")
        .await
        .map_err(stdout_error)?;
    writer.flush().await.map_err(stdout_error)
}

pub(crate) async fn run_task_smoke(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    task: CodingLoopTaskSmokeTask,
    merry_config: Option<&MerryConfig>,
) -> Result<(), CliError> {
    let Some(admission) =
        coding_loop_smoke_admission_from_current_process(sandbox_child_handoff).await
    else {
        return Err(coding_loop_smoke_requires_sandbox_error(
            "coding-loop-task-smoke",
        ));
    };

    let fixture = CodingLoopTaskSmokeFixture::for_task(task);
    let smoke_root = prepare_coding_loop_task_fixture("coding-loop-task-smoke", fixture)?;
    let backend = action_process_runner(&smoke_root, merry_config)?;
    let runtime = build_coding_loop_task_smoke_runtime(
        &smoke_root,
        None,
        admission,
        backend.runner(),
        Some(backend.permissioned_factory()),
        fixture,
        automatic_compaction_config(merry_config).map_err(unexpected)?,
    )?;

    let result = runtime
        .run_agent_loop(
            StepInput::user_text(fixture.task_prompt()).map_err(unexpected)?,
            StepContext::default(),
            coding_agent_loop_config()?,
        )
        .await
        .map_err(unexpected)?;

    assert_coding_loop_task_smoke_result(&runtime, &result, &smoke_root, fixture).await?;

    let mut writer = BufWriter::new(tokio::io::stdout());
    writer
        .write_all(b"coding-loop-task-smoke: ok\n")
        .await
        .map_err(stdout_error)?;
    writer.flush().await.map_err(stdout_error)
}

pub(crate) async fn run_task_live_smoke(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    task: CodingLoopTaskSmokeTask,
    model_flag: Option<&str>,
    max_output_tokens: u64,
    merry_config: Option<&MerryConfig>,
) -> Result<(), CliError> {
    let Some(admission) =
        coding_loop_smoke_admission_from_current_process(sandbox_child_handoff).await
    else {
        return Err(coding_loop_smoke_requires_sandbox_error(
            "coding-loop-task-live-smoke",
        ));
    };
    let config = crate::debug::openai::debug_config(model_flag, merry_config)?;

    let fixture = CodingLoopTaskSmokeFixture::for_task(task);
    let smoke_root = prepare_coding_loop_task_fixture("coding-loop-task-live-smoke", fixture)?;
    let automatic_compaction = automatic_compaction_config(merry_config).map_err(unexpected)?;
    let skill_roots = merry_config
        .map(MerryConfig::skill_roots)
        .transpose()
        .map_err(unexpected)?
        .unwrap_or_default();
    let backend = action_process_runner(&smoke_root, merry_config)?;
    let runtime = build_coding_loop_task_live_smoke_runtime(
        &smoke_root,
        admission,
        config,
        backend.runner(),
        Some(backend.permissioned_factory()),
        CodingLoopLiveRuntimeOptions {
            automatic_compaction,
            skill_roots,
            subagents: subagents_config(merry_config).map_err(unexpected)?,
        },
    )?;
    let generation_config =
        GenerationConfig::new(Some(max_output_tokens), false).map_err(debug_openai_usage_error)?;
    let context = StepContext::default().with_generation_config(generation_config);

    let result = runtime
        .run_agent_loop(
            StepInput::user_text(&fixture.live_task_prompt(None)).map_err(unexpected)?,
            context,
            coding_agent_loop_config()?,
        )
        .await
        .map_err(unexpected)?;

    let assertion = async {
        assert_coding_loop_task_live_smoke_result(&runtime, &result, &smoke_root, fixture).await?;
        assert_coding_loop_task_live_smoke_tool_sequence(&runtime, result.events(), fixture).await
    }
    .await;

    let mut writer = BufWriter::new(tokio::io::stdout());
    write_coding_loop_task_live_smoke_report(
        &runtime,
        automatic_compaction,
        assertion.is_ok(),
        result.events(),
        &mut writer,
    )
    .await?;
    writer.flush().await.map_err(stdout_error)?;

    assertion
}

pub(crate) async fn run_subagent_live_smoke(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    model_flag: Option<&str>,
    max_output_tokens: u64,
    merry_config: Option<&MerryConfig>,
) -> Result<(), CliError> {
    let Some(admission) =
        coding_loop_smoke_admission_from_current_process(sandbox_child_handoff).await
    else {
        return Err(coding_loop_smoke_requires_sandbox_error(
            "coding-loop-subagent-live-smoke",
        ));
    };
    let config = crate::debug::openai::debug_config(model_flag, merry_config)?;

    let smoke_root = prepare_coding_loop_subagent_live_smoke_fixture()?;
    let automatic_compaction = automatic_compaction_config(merry_config).map_err(unexpected)?;
    let skill_roots = merry_config
        .map(MerryConfig::skill_roots)
        .transpose()
        .map_err(unexpected)?
        .unwrap_or_default();
    let backend = action_process_runner(&smoke_root, merry_config)?;
    let runtime = build_coding_loop_subagent_live_smoke_runtime(
        &smoke_root,
        admission,
        config,
        backend.runner(),
        Some(backend.permissioned_factory()),
        CodingLoopLiveRuntimeOptions {
            automatic_compaction,
            skill_roots,
            subagents: coding_loop_subagent_live_smoke_config()?,
        },
    )?;
    let generation_config =
        GenerationConfig::new(Some(max_output_tokens), false).map_err(debug_openai_usage_error)?;
    let context = StepContext::default().with_generation_config(generation_config);

    let result = runtime
        .run_agent_loop(
            StepInput::user_text(&coding_loop_subagent_live_smoke_task()).map_err(unexpected)?,
            context,
            coding_agent_loop_config()?,
        )
        .await
        .map_err(unexpected)?;

    let assertion =
        assert_coding_loop_subagent_live_smoke_result(&runtime, &result, &smoke_root).await;

    let mut writer = BufWriter::new(tokio::io::stdout());
    write_coding_loop_subagent_live_smoke_report(
        &runtime,
        assertion.is_ok(),
        result.events(),
        &smoke_root,
        &mut writer,
    )
    .await?;
    writer.flush().await.map_err(stdout_error)?;

    assertion
}

fn coding_loop_smoke_requires_sandbox_error(command: &str) -> CliError {
    CliError::DebugUsage(format!(
        "{command} must run via `merry --with-sandbox debug {command}`"
    ))
}

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
        match &event.kind {
            RuntimeEventKind::ToolCallPending { call } => {
                pending_by_call_id.insert(call.id().clone(), call.clone());
            }
            RuntimeEventKind::ToolCallResolved { result } => {
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
                    || !process_artifact_has_argv(text, PERMISSION_NETWORK_SMOKE_ARGV)
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
        .filter_map(|event| match &event.kind {
            RuntimeEventKind::ToolCallResolved { result } => Some(result.status()),
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

async fn assert_coding_loop_task_live_smoke_result(
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

async fn assert_coding_loop_subagent_live_smoke_result(
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
    events: &[RuntimeEvent],
    fixture: CodingLoopTaskSmokeFixture,
) -> Result<(), CliError> {
    let mut pending_patch_args = BTreeMap::new();
    for event in events {
        if let RuntimeEventKind::ToolCallPending { call } = &event.kind
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
        let RuntimeEventKind::ToolCallResolved { result } = &event.kind else {
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

async fn assert_coding_loop_subagent_live_smoke_tool_sequence(
    runtime: &Runtime,
    events: &[RuntimeEvent],
) -> Result<(), CliError> {
    let mut pending_by_call_id = BTreeMap::new();
    let mut resolved_tool_names = Vec::new();
    let mut resolved_read_paths = Vec::new();
    let mut spawn_call_id = None;
    let mut wait_call_id = None;
    let mut parent_patch_call_seen = false;
    for event in events {
        match &event.kind {
            RuntimeEventKind::ToolCallPending { call } => {
                pending_by_call_id.insert(call.id().clone(), call.clone());
                match call.name().as_str() {
                    "spawn_subagents" => spawn_call_id = Some(call.id().clone()),
                    "wait_subagents" => wait_call_id = Some(call.id().clone()),
                    WORKSPACE_PATCH_TOOL => parent_patch_call_seen = true,
                    _ => {}
                }
            }
            RuntimeEventKind::ToolCallResolved { result } => {
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

fn assert_coding_loop_smoke_tool_results(events: &[RuntimeEvent]) -> Result<(), CliError> {
    let statuses = events
        .iter()
        .filter_map(|event| match &event.kind {
            RuntimeEventKind::ToolCallResolved { result } => Some(result.status()),
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

fn failed_tool_result_summary(events: &[RuntimeEvent]) -> Option<String> {
    let mut pending_by_call_id = BTreeMap::new();
    for event in events {
        if let RuntimeEventKind::ToolCallPending { call } = &event.kind {
            pending_by_call_id.insert(call.id().clone(), call.name().clone());
        }
    }

    events.iter().find_map(|event| {
        let RuntimeEventKind::ToolCallResolved { result } = &event.kind else {
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

pub(crate) fn build_coding_loop_smoke_runtime(
    root: &Path,
    relative_cwd: Option<&str>,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    runner: Arc<dyn ProcessRunner>,
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    automatic_compaction: AutomaticCompactionConfig,
) -> Result<Runtime, CliError> {
    let provider = CodingLoopSmokeProvider::new(relative_cwd)?;
    build_coding_loop_runtime(
        CODING_LOOP_SMOKE_SESSION_ID,
        root,
        admission,
        Arc::new(provider),
        ModelName::new("merry-coding-loop-smoke").map_err(unexpected)?,
        runner,
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: false,
            approval_review: None,
            automatic_compaction,
            retry_policy: None,
            context_compaction: None,
            permissioned_process_runner_factory,
            skill_roots: Vec::new(),
            subagents: config::SubagentsConfig::default(),
        },
    )
}

fn build_coding_loop_live_smoke_runtime(
    root: &Path,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    config: OpenAiRuntimeConfig,
    runner: Arc<dyn ProcessRunner>,
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    options: CodingLoopLiveRuntimeOptions,
) -> Result<Runtime, CliError> {
    let provider = OpenAiProvider::new(config.primary.provider);
    let context_compaction = config
        .context_compaction
        .map(openai_context_compaction_provider)
        .transpose()?;
    let approval_review = config
        .approval_review
        .map(openai_approval_review_provider)
        .transpose()?;
    build_coding_loop_runtime(
        CODING_LOOP_LIVE_SMOKE_SESSION_ID,
        root,
        admission,
        Arc::new(provider),
        ModelName::new(&config.primary.model).map_err(debug_openai_usage_error)?,
        runner,
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: true,
            approval_review,
            automatic_compaction: options.automatic_compaction,
            retry_policy: config.retry_policy,
            context_compaction,
            permissioned_process_runner_factory,
            skill_roots: options.skill_roots,
            subagents: options.subagents,
        },
    )
}

pub(crate) fn build_coding_loop_task_smoke_runtime(
    root: &Path,
    relative_cwd: Option<&str>,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    runner: Arc<dyn ProcessRunner>,
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    fixture: CodingLoopTaskSmokeFixture,
    automatic_compaction: AutomaticCompactionConfig,
) -> Result<Runtime, CliError> {
    let provider = CodingLoopTaskSmokeProvider::new(relative_cwd, fixture)?;
    build_coding_loop_runtime(
        CODING_LOOP_TASK_SMOKE_SESSION_ID,
        root,
        admission,
        Arc::new(provider),
        ModelName::new("merry-coding-loop-task-smoke").map_err(unexpected)?,
        runner,
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: false,
            approval_review: None,
            automatic_compaction,
            retry_policy: None,
            context_compaction: None,
            permissioned_process_runner_factory,
            skill_roots: Vec::new(),
            subagents: config::SubagentsConfig::default(),
        },
    )
}

fn build_coding_loop_task_live_smoke_runtime(
    root: &Path,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    config: OpenAiRuntimeConfig,
    runner: Arc<dyn ProcessRunner>,
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    options: CodingLoopLiveRuntimeOptions,
) -> Result<Runtime, CliError> {
    let provider = OpenAiProvider::new(config.primary.provider);
    let context_compaction = config
        .context_compaction
        .map(openai_context_compaction_provider)
        .transpose()?;
    let approval_review = config
        .approval_review
        .map(openai_approval_review_provider)
        .transpose()?;
    build_coding_loop_runtime(
        CODING_LOOP_TASK_LIVE_SMOKE_SESSION_ID,
        root,
        admission,
        Arc::new(provider),
        ModelName::new(&config.primary.model).map_err(debug_openai_usage_error)?,
        runner,
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: true,
            approval_review,
            automatic_compaction: options.automatic_compaction,
            retry_policy: config.retry_policy,
            context_compaction,
            permissioned_process_runner_factory,
            skill_roots: options.skill_roots,
            subagents: options.subagents,
        },
    )
}

fn build_coding_loop_subagent_live_smoke_runtime(
    root: &Path,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    config: OpenAiRuntimeConfig,
    runner: Arc<dyn ProcessRunner>,
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    options: CodingLoopLiveRuntimeOptions,
) -> Result<Runtime, CliError> {
    let provider = OpenAiProvider::new(config.primary.provider);
    let context_compaction = config
        .context_compaction
        .map(openai_context_compaction_provider)
        .transpose()?;
    let approval_review = config
        .approval_review
        .map(openai_approval_review_provider)
        .transpose()?;
    build_coding_loop_runtime(
        CODING_LOOP_SUBAGENT_LIVE_SMOKE_SESSION_ID,
        root,
        admission,
        Arc::new(provider),
        ModelName::new(&config.primary.model).map_err(debug_openai_usage_error)?,
        runner,
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: false,
            approval_review,
            automatic_compaction: options.automatic_compaction,
            retry_policy: config.retry_policy,
            context_compaction,
            permissioned_process_runner_factory,
            skill_roots: options.skill_roots,
            subagents: options.subagents,
        },
    )
}

fn build_permission_network_smoke_runtime(
    root: &Path,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    config: OpenAiRuntimeConfig,
    runner: Arc<dyn ProcessRunner>,
    permissioned_process_runner_factory: Arc<dyn PermissionedProcessRunnerFactory>,
    automatic_compaction: AutomaticCompactionConfig,
) -> Result<Runtime, CliError> {
    let session_id = SessionId::new(PERMISSION_NETWORK_SMOKE_SESSION_ID).map_err(unexpected)?;
    let provider = OpenAiProvider::new(config.primary.provider);
    let mut builder = Runtime::builder(session_id)
        .automatic_compaction(automatic_compaction)
        .model_provider(
            Arc::new(provider),
            ModelName::new(&config.primary.model).map_err(debug_openai_usage_error)?,
        );
    if let Some(role_provider) = config
        .context_compaction
        .map(openai_context_compaction_provider)
        .transpose()?
    {
        builder = builder.model_provider_for_role(
            role_provider.role,
            role_provider.provider,
            role_provider.model,
        );
    }
    if let Some(role_provider) = config
        .approval_review
        .map(openai_approval_review_provider)
        .transpose()?
    {
        builder = builder.model_provider_for_role(
            role_provider.role,
            role_provider.provider,
            role_provider.model,
        );
    }

    let profile = WorkspaceCodingLoopProfile::new(workspace_tools_config(
        coding_loop_workspace_roots(root, &[]),
        false,
        false,
        None,
    )?)
    .map_err(unexpected)?
    .with_cli_bwrap_permissioned_process_runner(
        admission,
        runner,
        permissioned_process_runner_factory,
    );
    let mut builder = with_workspace_coding_loop_profile(builder, profile)?;
    if let Some(policy) = config.retry_policy {
        builder = builder.model_retry_policy(policy);
    }
    builder.build().map_err(unexpected)
}

#[cfg(test)]
pub(crate) fn build_scripted_permission_network_smoke_runtime(
    root: &Path,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    runner: Arc<dyn ProcessRunner>,
    permissioned_process_runner_factory: Arc<dyn PermissionedProcessRunnerFactory>,
    automatic_compaction: AutomaticCompactionConfig,
) -> Result<Runtime, CliError> {
    let session_id = SessionId::new(PERMISSION_NETWORK_SMOKE_SESSION_ID).map_err(unexpected)?;
    let provider = PermissionNetworkSmokeProvider::new()?;
    let review_provider = PermissionNetworkSmokeReviewProvider::new()?;
    let builder = Runtime::builder(session_id)
        .automatic_compaction(automatic_compaction)
        .model_provider(
            Arc::new(provider),
            ModelName::new("merry-permission-network-smoke-scripted").map_err(unexpected)?,
        )
        .model_provider_for_role(
            RuntimeModelRole::ApprovalReview,
            Arc::new(review_provider),
            ModelName::new("merry-permission-network-smoke-review-scripted").map_err(unexpected)?,
        );

    let profile = WorkspaceCodingLoopProfile::new(workspace_tools_config(
        coding_loop_workspace_roots(root, &[]),
        false,
        false,
        None,
    )?)
    .map_err(unexpected)?
    .with_cli_bwrap_permissioned_process_runner(
        admission,
        runner,
        permissioned_process_runner_factory,
    );
    with_workspace_coding_loop_profile(builder, profile)?
        .build()
        .map_err(unexpected)
}

fn coding_loop_subagent_live_smoke_config() -> Result<config::SubagentsConfig, CliError> {
    Ok(config::SubagentsConfig::enabled(
        merry_runtime::SubagentConfig::new(2, 1).map_err(unexpected)?,
    ))
}

struct CodingLoopLiveRuntimeOptions {
    automatic_compaction: AutomaticCompactionConfig,
    skill_roots: Vec<PathBuf>,
    subagents: config::SubagentsConfig,
}

fn permission_network_smoke_process_runner(
    workspace_root: &Path,
    merry_config: Option<&MerryConfig>,
) -> Result<ActionProcessBackend, CliError> {
    let path_rules = merry_config
        .map(MerryConfig::trusted_global_path_rules)
        .transpose()
        .map_err(unexpected)?
        .unwrap_or_default();
    let runner = BwrapProcessRunner::new_at_workspace_root(workspace_root)
        .with_path_rules(path_rules.clone());
    let permissioned_factory =
        BwrapPermissionedProcessRunnerFactory::new_at_workspace_root(workspace_root)
            .with_path_rules(path_rules);
    Ok(ActionProcessBackend::from_parts(
        Arc::new(runner),
        Arc::new(permissioned_factory),
    ))
}

fn permission_network_live_smoke_task() -> String {
    format!(
        "\
You are driving Merry's live permission-network smoke.

Use the available tools, one tool call per step. Do not answer from memory.

Required sequence:
1. Call `{process_tool}` with exactly this argv: [\"{program}\", \"{arg1}\", \"{arg2}\"].
2. The first process call is expected to fail because the default inner sandbox has no network.
3. If that first process call fails, call `request_permissions` for the exact same process action with requested network access:
   - reason: explain that the exact DNS lookup failed under the default inner sandbox and network is needed only for this smoke command.
   - requested: {{\"network\": true}}
   - for_action: {{\"kind\": \"process\", \"argv\": [\"{program}\", \"{arg1}\", \"{arg2}\"]}}
4. After `request_permissions` resolves, inspect the tool result. It should execute the exact planned process action under the approved per-action network profile.
5. Return a concise final answer only after the approved process result succeeds.

Constraints:
- Do not request any filesystem path permission.
- Do not request network before the first process attempt fails.
- Do not use shell strings, scripts, pipelines, env, stdin, git, cargo, curl, wget, or any command other than the exact argv above.
- Do not call any workspace patch/write tool.
",
        process_tool = CODING_LOOP_PROCESS_TOOL,
        program = PERMISSION_NETWORK_SMOKE_ARGV[0],
        arg1 = PERMISSION_NETWORK_SMOKE_ARGV[1],
        arg2 = PERMISSION_NETWORK_SMOKE_ARGV[2],
    )
}

fn coding_loop_live_smoke_task(relative_cwd: Option<&str>) -> String {
    let cwd = relative_cwd.unwrap_or(".");
    format!(
        "\
You are driving Merry's minimal live coding-loop smoke.

Use the available tools, one tool call per step. Do not answer from memory.

Required sequence:
1. Call `{process_tool}` with argv `[\"rg\", \"--files\"]` and cwd `{cwd}` to inspect the fixture.
2. Call `{read_tool}` with path `src/lib.rs` to read exact source.
3. Call `{patch_tool}` with one `patch` string:
   *** Begin Workspace Patch
   *** Update File: src/lib.rs
   -    \"{initial}\"
   +    \"{target}\"
   *** End Workspace Patch
4. Call `{process_tool}` with argv `[\"rg\", \"{target}\"]` and cwd `{cwd}` to verify.
5. After verification succeeds, return a concise final answer.

Constraints:
- Do not use shell strings, scripts, pipelines, env, stdin, git, cargo, or any command except the two exact rg argv values above.
- Do not modify any file except `src/lib.rs` through `{patch_tool}`.
- The final file must equal:

pub fn greeting() -> &'static str {{
    \"{target}\"
}}
",
        process_tool = CODING_LOOP_PROCESS_TOOL,
        read_tool = WORKSPACE_READ_FILE_TOOL,
        patch_tool = WORKSPACE_PATCH_TOOL,
        initial = CODING_LOOP_LIVE_SMOKE_INITIAL_VALUE,
        target = CODING_LOOP_LIVE_SMOKE_TARGET_VALUE,
    )
}

pub(crate) fn coding_loop_subagent_live_smoke_task() -> String {
    format!(
        "\
You are driving Merry's minimal live subagent smoke.

You must delegate the work to a child agent before you finish.

Required sequence:
1. Call `spawn_subagents` with exactly one child task.
2. The child task must use `workspace_read_file` and `workspace_patch` only.
3. The child task must read `{file}` and patch it from:
   {initial}to:
   {target}
4. The child task must declare `allowed_tools` as `[\"workspace_read_file\", \"workspace_patch\"]`.
5. The child task must declare `read_scope` and `write_scope` as `[\"{file}\"]`.
6. After spawning, call `wait_subagents` for the returned child id with mode `all`.
7. After the child reports completion, call `workspace_read_file` on `{file}` and verify the exact final content.
8. Return a concise final answer only after the verification read succeeds.

Constraints:
- The parent agent must not patch the fixture directly.
- Do not use more than one child task.
- Do not answer from memory.
- Keep the final result short.
",
        file = CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE,
        initial = CODING_LOOP_SUBAGENT_LIVE_SMOKE_INITIAL,
        target = CODING_LOOP_SUBAGENT_LIVE_SMOKE_TARGET,
    )
}

async fn assert_coding_loop_live_smoke_tool_sequence(
    runtime: &Runtime,
    events: &[RuntimeEvent],
) -> Result<(), CliError> {
    let mut pending_by_call_id = BTreeMap::new();
    let mut resolved_tool_names = Vec::new();
    let mut resolved_artifacts = Vec::new();
    for event in events {
        match &event.kind {
            RuntimeEventKind::ToolCallPending { call } => {
                pending_by_call_id.insert(call.id().clone(), call.clone());
            }
            RuntimeEventKind::ToolCallResolved { result } => {
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
        process_artifact_has_argv(text, ["rg", "--files"]) && text.contains("src/lib.rs")
    });
    let verified = process_artifact_texts.iter().any(|text| {
        process_artifact_has_argv(text, ["rg", CODING_LOOP_LIVE_SMOKE_TARGET_VALUE])
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

async fn assert_coding_loop_task_live_smoke_tool_sequence(
    runtime: &Runtime,
    events: &[RuntimeEvent],
    fixture: CodingLoopTaskSmokeFixture,
) -> Result<(), CliError> {
    let mut pending_by_call_id = BTreeMap::new();
    let mut resolved_tool_names = Vec::new();
    let mut resolved_artifacts = Vec::new();
    let mut resolved_read_paths = Vec::new();
    for event in events {
        match &event.kind {
            RuntimeEventKind::ToolCallPending { call } => {
                pending_by_call_id.insert(call.id().clone(), call.clone());
            }
            RuntimeEventKind::ToolCallResolved { result } => {
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
            process_artifact_has_cargo_package_argv(text, "check", fixture.package_name());
        saw_cargo_test |=
            process_artifact_has_cargo_package_argv(text, "test", fixture.package_name());
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

fn process_artifact_has_argv<const N: usize>(text: &str, expected: [&str; N]) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    value
        .get("intent")
        .and_then(|intent| intent.get("argv"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|argv| {
            argv.iter()
                .filter_map(serde_json::Value::as_str)
                .eq(expected)
        })
}

fn process_artifact_has_cargo_package_argv(text: &str, command: &str, package: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    let Some(argv) = value
        .get("intent")
        .and_then(|intent| intent.get("argv"))
        .and_then(serde_json::Value::as_array)
    else {
        return false;
    };
    matches!(
        argv.as_slice(),
        [cargo, actual_command, package_flag, actual_package]
            if cargo.as_str() == Some("cargo")
                && actual_command.as_str() == Some(command)
                && matches!(package_flag.as_str(), Some("-p" | "--package"))
                && actual_package.as_str() == Some(package)
    )
}
