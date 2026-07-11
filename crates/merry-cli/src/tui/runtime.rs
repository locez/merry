use crate::cli_error::{CliError, debug_openai_usage_error, unexpected};
use crate::coding_runtime::{
    HeadlessCodingRuntimeInput, action_process_runner, build_headless_coding_runtime,
    coding_agent_loop_config, coding_agent_process_admission, resume_headless_coding_runtime,
};
use crate::config::MerryConfig;
use crate::mcp_tools::discover_configured_mcp_tools;
use crate::provider_config::{
    RuntimePrimaryProviderConfig, RuntimeProviderBundle,
    runtime_primary_provider_from_config_with_override,
    runtime_provider_bundle_from_config_with_primary_override,
};
use crate::runtime_config::{
    action_process_backend_options, automatic_compaction_config, main_reasoning_effort,
    subagents_config,
};
use crate::sandbox::ChildHandoff as SandboxChildHandoff;
use crate::tui::preferences::{CompactionStrategy, TuiPreferences};
use crate::tui::session_list::{TuiSessionMetadata, TuiSessionStore, now_unix_ms};
use crate::tui::session_picker::SessionPickerSelection;
use merry_core::SessionId;
use merry_llm::GenerationConfig;
use merry_runtime::{
    AgentLoopControl, AgentLoopInput, AutomaticCompactionConfig, CitationCompactionPolicy,
    InteractivePrimaryModel, InteractiveRunEventStream, InteractiveSettingsUpdate,
    InteractiveSubagentSettings, Runtime, SessionTranscriptItem, SkillMetadata, StepContext,
};
use std::{env, num::NonZeroU64, path::PathBuf};

pub(crate) struct TuiRuntimeSession {
    pub(crate) workspace_root: PathBuf,
    pub(crate) model_label: String,
    pub(crate) reasoning_effort_label: Option<String>,
    pub(crate) metadata: TuiSessionMetadata,
    pub(crate) resumed: bool,
    runtime: Runtime,
    session_store: TuiSessionStore,
    pub(crate) stream: InteractiveRunEventStream,
    pub(crate) input: AgentLoopInput,
    pub(crate) control: AgentLoopControl,
    pub(crate) skills: Vec<SkillMetadata>,
    merry_config: MerryConfig,
}

pub(crate) async fn start_tui_runtime_session(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    merry_config: Option<&MerryConfig>,
    session_store: TuiSessionStore,
    selection: SessionPickerSelection,
    no_outer_sandbox: bool,
    preferences: &TuiPreferences,
) -> Result<TuiRuntimeSession, CliError> {
    let Some(admission) =
        coding_agent_process_admission(sandbox_child_handoff, no_outer_sandbox).await
    else {
        return Err(CliError::DebugUsage(
            "merry TUI requires the automatic bubblewrap sandbox".to_owned(),
        ));
    };
    let owned_config = merry_config.cloned().ok_or_else(|| {
        debug_openai_usage_error("Merry XDG provider config is required for runtime".to_owned())
    })?;

    let RuntimeProviderBundle {
        primary,
        context_compaction,
        approval_review,
        retry_policy,
    } = runtime_provider_bundle_from_config_with_primary_override(
        merry_config,
        preferences.provider.as_deref(),
        preference_model_override(&owned_config, preferences)?,
        debug_openai_usage_error,
    )?;
    let RuntimePrimaryProviderConfig { provider, model } = primary;
    let model_label = model.as_str().to_owned();
    let generation =
        generation_config_with_preferences(merry_config, preferences).map_err(unexpected)?;
    let reasoning_effort_label = generation
        .reasoning_effort()
        .map(|effort| effort.as_str().to_owned());
    let workspace_root = env::current_dir().map_err(unexpected)?;
    let (session_id, mut metadata, should_resume) = session_start(selection, &workspace_root);
    metadata.model = Some(model_label.clone());
    metadata.reasoning_effort = reasoning_effort_label.clone();
    let backend = action_process_runner(
        &workspace_root,
        action_process_backend_options(merry_config).map_err(unexpected)?,
    )?;
    let extra_tools = discover_configured_mcp_tools(merry_config).await?;
    let subagents = subagents_config(merry_config)
        .map_err(unexpected)?
        .with_overrides(
            preferences.subagents_enabled,
            preferences.subagent_max_threads,
        )
        .map_err(unexpected)?;
    let runtime_input = HeadlessCodingRuntimeInput {
        session_id: session_id.as_str(),
        root: &workspace_root,
        admission,
        provider,
        model,
        runner: backend.runner(),
        permissioned_process_runner_factory: backend.permissioned_factory(),
        extra_tools,
        allow_hidden_workspace_paths: false,
        automatic_compaction: automatic_compaction_config_with_preferences(
            merry_config,
            preferences,
        )?,
        retry_policy,
        context_compaction,
        approval_review,
        skill_roots: merry_config
            .map(MerryConfig::skill_roots)
            .transpose()
            .map_err(unexpected)?
            .unwrap_or_default(),
        subagents: crate::coding_runtime::CodingSubagentsConfig::runtime_controlled(
            subagents.is_enabled(),
            subagents.limits(),
        ),
    };
    let runtime = if should_resume {
        resume_headless_coding_runtime(runtime_input, session_store.session_state_store()).await?
    } else {
        build_headless_coding_runtime(runtime_input)?
    };
    let loop_config = coding_agent_loop_config()?;
    let skills = runtime.skills().await;
    let interactive = runtime
        .start_interactive_agent_run(
            StepContext::new(Default::default()).with_generation_config(generation),
            loop_config,
        )
        .map_err(unexpected)?;
    let (stream, input, control) = interactive.split();
    if preferences.context_window_tokens.is_some() {
        control
            .update_settings(
                InteractiveSettingsUpdate::default().with_context_window_tokens(
                    context_window_tokens_with_preferences(preferences)?,
                ),
            )
            .await
            .map_err(unexpected)?;
    }

    Ok(TuiRuntimeSession {
        workspace_root,
        model_label,
        reasoning_effort_label,
        metadata,
        resumed: should_resume,
        runtime,
        session_store,
        stream,
        input,
        control,
        skills,
        merry_config: owned_config,
    })
}

impl TuiRuntimeSession {
    pub(crate) fn replace_config(&mut self, config: MerryConfig) {
        self.merry_config = config;
    }

    pub(crate) async fn apply_preferences(
        &mut self,
        preferences: &TuiPreferences,
    ) -> Result<(), CliError> {
        let RuntimePrimaryProviderConfig { provider, model } =
            runtime_primary_provider_from_config_with_override(
                Some(&self.merry_config),
                preferences.provider.as_deref(),
                preference_model_override(&self.merry_config, preferences)?,
                debug_openai_usage_error,
            )?;
        let retry_policy = self
            .merry_config
            .provider_retry_policy()
            .map_err(unexpected)?
            .unwrap_or_default();
        let generation = generation_config_with_preferences(Some(&self.merry_config), preferences)
            .map_err(unexpected)?;
        let subagents = self
            .merry_config
            .subagents_config()
            .map_err(unexpected)?
            .with_overrides(
                preferences.subagents_enabled,
                preferences.subagent_max_threads,
            )
            .map_err(unexpected)?;
        let automatic_compaction =
            automatic_compaction_config_with_preferences(Some(&self.merry_config), preferences)?;
        let update = InteractiveSettingsUpdate::default()
            .with_primary_model(InteractivePrimaryModel::new(
                provider,
                model.clone(),
                retry_policy,
            ))
            .with_generation_config(generation.clone())
            .with_subagents(InteractiveSubagentSettings::new(
                subagents.is_enabled(),
                subagents.limits(),
            ))
            .with_automatic_compaction(automatic_compaction)
            .with_context_window_tokens(context_window_tokens_with_preferences(preferences)?);
        self.control
            .update_settings(update)
            .await
            .map_err(unexpected)?;

        self.model_label = model.as_str().to_owned();
        self.reasoning_effort_label = generation
            .reasoning_effort()
            .map(|effort| effort.as_str().to_owned());
        self.metadata.model = Some(self.model_label.clone());
        self.metadata.reasoning_effort = self.reasoning_effort_label.clone();
        Ok(())
    }

    pub(crate) fn set_title(&mut self, title: Option<String>) {
        self.metadata.title = title;
    }

    pub(crate) async fn save_on_exit(&mut self) -> Result<(), CliError> {
        self.metadata.mark_active(now_unix_ms());
        self.runtime
            .save_session_to(self.session_store.session_state_store())
            .await
            .map_err(unexpected)?;
        self.session_store
            .write_metadata(&self.metadata)
            .map_err(unexpected)?;
        Ok(())
    }

    pub(crate) async fn session_transcript(&self) -> Result<Vec<SessionTranscriptItem>, CliError> {
        self.runtime.session_transcript().await.map_err(unexpected)
    }
}

fn preference_model_override<'a>(
    config: &MerryConfig,
    preferences: &'a TuiPreferences,
) -> Result<Option<&'a str>, CliError> {
    let provider = match preferences.provider.as_deref() {
        Some(provider) => provider.to_owned(),
        None => config.default_provider().map_err(unexpected)?.alias,
    };
    Ok(preferences.model_for_provider(&provider))
}

fn session_start(
    selection: SessionPickerSelection,
    workspace_root: &std::path::Path,
) -> (SessionId, TuiSessionMetadata, bool) {
    let now = now_unix_ms();
    match selection {
        SessionPickerSelection::Resume(mut metadata) => {
            metadata.mark_active(now);
            (metadata.session_id.clone(), metadata, true)
        }
        SessionPickerSelection::New | SessionPickerSelection::Quit => {
            let session_id = default_tui_session_id();
            let metadata =
                TuiSessionMetadata::new(session_id.clone(), workspace_root.to_path_buf(), now);
            (session_id, metadata, false)
        }
    }
}

pub(crate) fn default_tui_session_id() -> merry_core::SessionId {
    crate::session_id::new_ephemeral_session_id()
}

fn generation_config_with_preferences(
    merry_config: Option<&MerryConfig>,
    preferences: &TuiPreferences,
) -> Result<GenerationConfig, crate::config::ConfigError> {
    let reasoning_effort = preferences
        .reasoning_effort
        .clone()
        .or(main_reasoning_effort(merry_config)?);
    Ok(GenerationConfig::default().with_reasoning_effort(reasoning_effort))
}

fn automatic_compaction_config_with_preferences(
    merry_config: Option<&MerryConfig>,
    preferences: &TuiPreferences,
) -> Result<AutomaticCompactionConfig, CliError> {
    let inherited = automatic_compaction_config(merry_config).map_err(unexpected)?;
    let policy = match preferences.compaction_strategy {
        None | Some(CompactionStrategy::Balanced) => inherited.policy(),
        Some(CompactionStrategy::Compact) => {
            CitationCompactionPolicy::new(128, Some(192), 6144, 1, 900, 12).map_err(unexpected)?
        }
        Some(CompactionStrategy::PreserveDetail) => {
            CitationCompactionPolicy::new(320, Some(384), 12_288, 4, 1800, 24)
                .map_err(unexpected)?
        }
    };
    let enabled = preferences
        .auto_compaction_enabled
        .unwrap_or_else(|| inherited.is_enabled());
    Ok(if enabled {
        AutomaticCompactionConfig::enabled(policy)
    } else {
        AutomaticCompactionConfig::disabled()
    })
}

fn context_window_tokens_with_preferences(
    preferences: &TuiPreferences,
) -> Result<Option<NonZeroU64>, CliError> {
    preferences
        .context_window_tokens
        .map(|tokens| {
            NonZeroU64::new(tokens)
                .ok_or_else(|| unexpected("context window preference must be greater than zero"))
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::XdgPaths;

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

    #[test]
    fn tui_reasoning_preference_overrides_provider_default() {
        let paths = XdgPaths::from_parts(std::path::PathBuf::from("/home/alice"), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "compat"
model = "gpt-test"
reasoning_effort = "low"

[providers.compat]
type = "openai-compatible"
api_key = "sk-test"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should exist");
        let mut preferences = TuiPreferences::default();
        preferences.reasoning_effort = Some(merry_llm::ReasoningEffort::new("high").unwrap());

        let generation = generation_config_with_preferences(Some(&config), &preferences)
            .expect("generation config should build");

        assert_eq!(
            generation.reasoning_effort().map(|effort| effort.as_str()),
            Some("high")
        );
    }
}
