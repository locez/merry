use crate::cli_error::{CliError, debug_openai_usage_error, stdout_error, unexpected};
use crate::coding_runtime::{
    action_process_runner, coding_agent_loop_config,
    coding_loop_smoke_admission_from_current_process,
};
use crate::config::MerryConfig;
use crate::debug::CodingLoopTaskSmokeTask;
use crate::runtime_config::{automatic_compaction_config, subagents_config};
use crate::sandbox::ChildHandoff as SandboxChildHandoff;
use merry_llm::GenerationConfig;
use merry_runtime::{AgentLoopConfig, StepContext, StepInput};
use tokio::io::{AsyncWriteExt, BufWriter};

use super::{
    CodingLoopLiveRuntimeOptions, PERMISSION_NETWORK_SMOKE_SESSION_ID,
    assert_coding_loop_live_smoke_tool_sequence, assert_coding_loop_smoke_result,
    assert_coding_loop_subagent_live_smoke_result, assert_coding_loop_task_live_smoke_result,
    assert_coding_loop_task_live_smoke_tool_sequence, assert_coding_loop_task_smoke_result,
    assert_permission_network_smoke_result, build_coding_loop_live_smoke_runtime,
    build_coding_loop_smoke_runtime, build_coding_loop_subagent_live_smoke_runtime,
    build_coding_loop_task_live_smoke_runtime, build_coding_loop_task_smoke_runtime,
    build_permission_network_smoke_runtime, coding_loop_live_smoke_task,
    coding_loop_subagent_live_smoke_config, coding_loop_subagent_live_smoke_task,
    permission_network_live_smoke_task, permission_network_smoke_process_runner,
    prepare_coding_loop_smoke_fixture, prepare_coding_loop_subagent_live_smoke_fixture,
    prepare_coding_loop_task_fixture, write_coding_loop_subagent_live_smoke_report,
    write_coding_loop_task_live_smoke_report, write_permission_network_smoke_report,
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

    let fixture = super::CodingLoopTaskSmokeFixture::for_task(task);
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

    let fixture = super::CodingLoopTaskSmokeFixture::for_task(task);
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
