use crate::cli_error::{CliError, debug_openai_usage_error, unexpected};
use crate::coding::{
    CodingPermissionPolicy, HeadlessCodingRuntimeInput, ProcessExecutionMode,
    action_process_runner_for_mode, build_headless_coding_with_policy_composition,
    coding_agent_process_admission, resume_headless_coding_composition_with_policy,
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
    AgentLoopControl, AgentLoopInput, AutomaticCompactionConfig, ChannelPermissionAdmissionSource,
    InteractivePrimaryModel, InteractiveRunEventStream, InteractiveSettingsUpdate,
    InteractiveSubagentSettings, PermissionReviewRequest, Runtime, SessionTranscriptItem,
    SkillMetadata, StepContext, SubagentActivityReceiver,
};
use std::{collections::VecDeque, env, num::NonZeroU64, path::PathBuf, sync::Arc};
use tokio::sync::mpsc;

pub(crate) struct TuiRuntimeSession {
    pub(crate) workspace_root: PathBuf,
    pub(crate) model_label: String,
    pub(crate) reasoning_effort_label: Option<String>,
    pub(crate) metadata: TuiSessionMetadata,
    pub(crate) resumed: bool,
    runtime: Runtime,
    session_store: TuiSessionStore,
    pub(crate) stream: InteractiveRunEventStream,
    pub(crate) subagent_activity: SubagentActivityReceiver,
    pub(crate) input: AgentLoopInput,
    pub(crate) control: AgentLoopControl,
    pub(crate) skills: Vec<SkillMetadata>,
    merry_config: MerryConfig,
    pub(crate) permission_requests: mpsc::Receiver<PermissionReviewRequest>,
    pending_permission_reviews: VecDeque<PermissionReviewRequest>,
}

pub(crate) async fn start_tui_runtime_session(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    merry_config: Option<&MerryConfig>,
    session_store: TuiSessionStore,
    selection: SessionPickerSelection,
    process_execution_mode: ProcessExecutionMode,
    fully_trusted: bool,
    preferences: &TuiPreferences,
) -> Result<TuiRuntimeSession, CliError> {
    let Some(_admission) =
        coding_agent_process_admission(sandbox_child_handoff, process_execution_mode).await
    else {
        return Err(CliError::DebugUsage(
            "merry TUI requires the configured outer sandbox".to_owned(),
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
    let backend = action_process_runner_for_mode(
        &workspace_root,
        action_process_backend_options(merry_config).map_err(unexpected)?,
        process_execution_mode,
    )?;
    let (permission_source, permission_requests) = ChannelPermissionAdmissionSource::channel(8);
    let permission_source = Arc::new(permission_source);
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
        provider,
        model,
        process_backend: backend,
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
        subagents: crate::coding::CodingSubagentsConfig::runtime_controlled(
            subagents.is_enabled(),
            subagents.limits(),
        ),
        workspace_tool_limits: None,
    };
    let permission_policy = if fully_trusted {
        CodingPermissionPolicy::fully_trusted()
    } else {
        CodingPermissionPolicy::model_then_host_fallback(permission_source)
    };
    let coding_runtime = if should_resume {
        resume_headless_coding_composition_with_policy(
            runtime_input,
            session_store.session_state_store(),
            permission_policy,
        )
        .await?
    } else {
        build_headless_coding_with_policy_composition(runtime_input, permission_policy)?
    };
    let loop_config = coding_runtime.loop_config();
    let runtime = coding_runtime.into_runtime();
    let skills = runtime.skills().await;
    let interactive = runtime
        .start_interactive_agent_run(
            StepContext::new(Default::default()).with_generation_config(generation),
            loop_config,
        )
        .map_err(unexpected)?;
    let subagent_activity = runtime.subscribe_subagent_activity();
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
        subagent_activity,
        input,
        control,
        skills,
        merry_config: owned_config,
        permission_requests,
        pending_permission_reviews: VecDeque::new(),
    })
}

impl TuiRuntimeSession {
    pub(crate) fn enqueue_permission_review(
        &mut self,
        request: PermissionReviewRequest,
    ) -> Option<(String, String)> {
        if request.is_cancelled() {
            return None;
        }
        let view = permission_review_view(&request);
        let should_open = self.pending_permission_reviews.is_empty();
        self.pending_permission_reviews.push_back(request);
        should_open.then_some(view)
    }

    pub(crate) fn prune_cancelled_permission_reviews(
        &mut self,
    ) -> Option<Option<(String, String)>> {
        let mut pruned = false;
        while self
            .pending_permission_reviews
            .front()
            .is_some_and(PermissionReviewRequest::is_cancelled)
        {
            self.pending_permission_reviews.pop_front();
            pruned = true;
        }
        pruned.then(|| {
            self.pending_permission_reviews
                .front()
                .map(permission_review_view)
        })
    }

    pub(crate) fn resolve_permission_review(
        &mut self,
        approval_id: &str,
        approved: bool,
    ) -> Result<Option<(String, String)>, CliError> {
        let Some(request) = self.pending_permission_reviews.pop_front() else {
            return Ok(None);
        };
        if request.approval_id() != approval_id {
            self.pending_permission_reviews.push_front(request);
            return Err(unexpected(
                "permission review overlay did not match pending request",
            ));
        }
        let result = if approved {
            request.approve("Approved by the Merry permission review UI.")
        } else {
            request.deny("Rejected by the Merry permission review UI.")
        };
        result.map_err(unexpected)?;
        Ok(self
            .pending_permission_reviews
            .front()
            .map(permission_review_view))
    }

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
        write_session_metadata(&self.session_store, &self.metadata).await?;
        Ok(())
    }

    pub(crate) async fn save_now(&mut self) -> Result<(), CliError> {
        self.metadata.mark_active(now_unix_ms());
        self.control
            .save_session_to(self.session_store.session_state_store())
            .await
            .map_err(unexpected)?;
        write_session_metadata(&self.session_store, &self.metadata).await?;
        Ok(())
    }

    pub(crate) async fn session_transcript(&self) -> Result<Vec<SessionTranscriptItem>, CliError> {
        self.runtime.session_transcript().await.map_err(unexpected)
    }

    pub(crate) async fn plan_snapshot(&self) -> Result<Option<merry_core::PlanSnapshot>, CliError> {
        self.runtime.plan_snapshot().await.map_err(unexpected)
    }
}

async fn write_session_metadata(
    store: &TuiSessionStore,
    metadata: &TuiSessionMetadata,
) -> Result<(), CliError> {
    let store = store.clone();
    let metadata = metadata.clone();
    tokio::task::spawn_blocking(move || store.write_metadata(&metadata))
        .await
        .map_err(unexpected)?
        .map_err(unexpected)
}

fn permission_review_view(request: &PermissionReviewRequest) -> (String, String) {
    let permission_request = request.request();
    let mut lines = vec![format!("approval_id: {}", request.approval_id())];
    if let Some(failure) = request.review_failure() {
        lines.push(format!("AI review fallback: {failure}"));
    }
    if let Some(reason) = permission_request.reason() {
        lines.push(format!("reason: {reason}"));
    }
    if permission_request.is_action_review() {
        lines.push("review: high-risk process action".to_owned());
    }
    lines.push(format!("action: {}", permission_request.action().summary()));
    lines.push("requested capabilities:".to_owned());
    for capability in permission_request.requested() {
        match capability {
            merry_runtime::RequestedCapability::Network => {
                lines.push("  - network".to_owned());
            }
            merry_runtime::RequestedCapability::Path(path) => {
                lines.push(format!(
                    "  - path {} ({})",
                    path.path(),
                    path.access().as_str()
                ));
            }
            merry_runtime::RequestedCapability::HostIntegration(integration) => {
                lines.push(format!("  - host integration {}", integration.as_str()));
            }
        }
    }
    (request.approval_id(), lines.join("\n"))
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
        Some(CompactionStrategy::Compact) => inherited
            .policy()
            .with_retained_model_turns(3)
            .map_err(unexpected)?,
        Some(CompactionStrategy::PreserveDetail) => inherited
            .policy()
            .with_retained_model_turns(7)
            .map_err(unexpected)?,
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

    #[test]
    fn tui_compaction_strategies_only_change_retained_model_turns() {
        let paths = XdgPaths::from_parts(std::path::PathBuf::from("/home/alice"), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[runtime.auto_compaction]
target_output_tokens = 9000
max_accepted_output_bytes = 72000
retained_model_turns = 5
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should exist");

        for (strategy, expected_turns) in [
            (CompactionStrategy::Compact, 3),
            (CompactionStrategy::Balanced, 5),
            (CompactionStrategy::PreserveDetail, 7),
        ] {
            let mut preferences = TuiPreferences::default();
            preferences.compaction_strategy = Some(strategy);
            let policy = automatic_compaction_config_with_preferences(Some(&config), &preferences)
                .expect("TUI compaction config should build")
                .policy();

            assert_eq!(policy.retained_model_turns(), expected_turns);
            assert_eq!(policy.target_output_tokens(), Some(9_000));
            assert_eq!(policy.max_accepted_output_bytes(), Some(72_000));
        }
    }
}
