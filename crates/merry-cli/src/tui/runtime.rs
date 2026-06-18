use crate::cli_error::{CliError, debug_openai_usage_error, unexpected};
use crate::coding_runtime::{
    HeadlessCodingRuntimeInput, action_process_runner, build_headless_coding_runtime,
    coding_agent_loop_config, coding_loop_smoke_admission_from_current_process,
};
use crate::config::MerryConfig;
use crate::provider_config::{
    RuntimePrimaryProviderConfig, RuntimeProviderBundle, openai_provider_bundle,
    openai_provider_config_bundle,
};
use crate::runtime_config::{
    action_process_backend_options, automatic_compaction_config, subagents_config,
};
use crate::sandbox::ChildHandoff as SandboxChildHandoff;
use merry_runtime::{
    AgentLoopControl, AgentLoopInput, InteractiveRunEventStream, SkillMetadata, StepContext,
};
use std::{env, path::PathBuf};

pub(crate) struct TuiRuntimeSession {
    pub(crate) workspace_root: PathBuf,
    pub(crate) model_label: String,
    pub(crate) stream: InteractiveRunEventStream,
    pub(crate) input: AgentLoopInput,
    pub(crate) control: AgentLoopControl,
    pub(crate) skills: Vec<SkillMetadata>,
}

pub(crate) async fn start_tui_runtime_session(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    merry_config: Option<&MerryConfig>,
) -> Result<TuiRuntimeSession, CliError> {
    let Some(admission) =
        coding_loop_smoke_admission_from_current_process(sandbox_child_handoff).await
    else {
        return Err(CliError::DebugUsage(
            "merry must run via `merry --with-sandbox`".to_owned(),
        ));
    };

    let config = openai_provider_config_bundle(None, merry_config, debug_openai_usage_error)?;
    let RuntimeProviderBundle {
        primary,
        context_compaction,
        approval_review,
        retry_policy,
    } = openai_provider_bundle(config, unexpected)?;
    let RuntimePrimaryProviderConfig { provider, model } = primary;
    let model_label = model.as_str().to_owned();
    let workspace_root = env::current_dir().map_err(unexpected)?;
    let backend = action_process_runner(
        &workspace_root,
        action_process_backend_options(merry_config).map_err(unexpected)?,
    )?;
    let session_id = default_tui_session_id();
    let runtime = build_headless_coding_runtime(HeadlessCodingRuntimeInput {
        session_id: session_id.as_str(),
        root: &workspace_root,
        admission,
        provider,
        model,
        runner: backend.runner(),
        permissioned_process_runner_factory: backend.permissioned_factory(),
        allow_hidden_workspace_paths: false,
        automatic_compaction: automatic_compaction_config(merry_config).map_err(unexpected)?,
        retry_policy,
        context_compaction,
        approval_review,
        skill_roots: merry_config
            .map(MerryConfig::skill_roots)
            .transpose()
            .map_err(unexpected)?
            .unwrap_or_default(),
        subagents: subagents_config(merry_config).map_err(unexpected)?.into(),
    })?;
    let loop_config = coding_agent_loop_config()?;
    let skills = runtime.skills().await;
    let interactive = runtime
        .start_interactive_agent_run(StepContext::new(Default::default()), loop_config)
        .map_err(unexpected)?;
    let (stream, input, control) = interactive.split();

    Ok(TuiRuntimeSession {
        workspace_root,
        model_label,
        stream,
        input,
        control,
        skills,
    })
}

pub(crate) fn default_tui_session_id() -> merry_core::SessionId {
    crate::session_id::new_ephemeral_session_id()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tui_session_id_is_filesystem_safe() {
        let session_id = default_tui_session_id();

        assert!(!session_id.as_str().is_empty());
        assert!(
            session_id
                .as_str()
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        );
    }
}
