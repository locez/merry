use crate::cli_error::{CliError, unexpected};
use crate::coding::ActionProcessBackendOptions;
use crate::config::{self, EffectiveLogSettings, MerryConfig, XdgPaths};
use crate::sandbox::default_inner_development_path_rules;
use merry_core::SessionId;
use merry_llm::{GenerationConfig, ReasoningEffort, ServiceTier};
use merry_runtime::{
    AutomaticCompactionConfig, PathAccess, PathAccessRule, PathAccessRuleSource, Runtime,
    RuntimeBuilder,
};
use std::{env, ffi::OsString, path::PathBuf};

pub(crate) fn validate_loaded_config(
    config: Option<&MerryConfig>,
    paths: &XdgPaths,
) -> Result<(), config::ConfigError> {
    let _ = paths.state_dir();
    let Some(config) = config else {
        return Ok(());
    };
    let _ = effective_log_settings(Some(config), paths)?;
    let _ = automatic_compaction_config(Some(config))?;
    let _ = subagents_config(Some(config))?;
    let _ = config.trusted_global_path_rules()?;
    let _ = config.process_environment_overrides()?;
    let _ = config.skill_roots()?;
    let _ = config.runtime_models()?;
    let _ = config.profile();
    let tui_config = config.tui_config()?;
    crate::config::validate_tui_config(&tui_config)?;
    config.validate_provider_settings_if_present()?;
    Ok(())
}

pub(crate) fn effective_log_settings(
    config: Option<&MerryConfig>,
    paths: &XdgPaths,
) -> Result<Option<EffectiveLogSettings>, config::ConfigError> {
    config
        .map(|config| config.effective_log_settings(paths))
        .transpose()
        .map(Option::flatten)
}

pub(crate) fn automatic_compaction_config(
    config: Option<&MerryConfig>,
) -> Result<AutomaticCompactionConfig, config::ConfigError> {
    config
        .map(MerryConfig::automatic_compaction_config)
        .transpose()
        .map(Option::unwrap_or_default)
}

pub(crate) fn subagents_config(
    config: Option<&MerryConfig>,
) -> Result<config::SubagentsConfig, config::ConfigError> {
    config
        .map(MerryConfig::subagents_config)
        .transpose()
        .map(Option::unwrap_or_default)
}

pub(crate) fn generation_config(
    config: Option<&MerryConfig>,
) -> Result<GenerationConfig, config::ConfigError> {
    let reasoning_effort = main_reasoning_effort(config)?;
    let service_tier = main_service_tier(config)?;
    Ok(GenerationConfig::default()
        .with_reasoning_effort(reasoning_effort)
        .with_service_tier(service_tier))
}

pub(crate) fn main_service_tier(
    config: Option<&MerryConfig>,
) -> Result<Option<ServiceTier>, config::ConfigError> {
    Ok(config
        .map(MerryConfig::configured_default_provider)
        .transpose()?
        .flatten()
        .and_then(|provider| provider.service_tier))
}

pub(crate) fn main_reasoning_effort(
    config: Option<&MerryConfig>,
) -> Result<Option<ReasoningEffort>, config::ConfigError> {
    Ok(config
        .map(MerryConfig::configured_default_provider)
        .transpose()?
        .flatten()
        .and_then(|provider| provider.reasoning_effort))
}

pub(crate) fn action_process_backend_options(
    config: Option<&MerryConfig>,
) -> Result<ActionProcessBackendOptions, config::ConfigError> {
    let home = config
        .map(|config| config.home().to_path_buf())
        .or_else(|| env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/home/merry"));
    let mut path_rules = default_inner_development_path_rules(&home);
    if let Some(config) = config {
        path_rules.extend(config.trusted_global_path_rules()?);
    }
    let private_paths = if let Some(config) = config {
        config.private_process_paths()?
    } else {
        let paths = XdgPaths::from_env()?;
        vec![
            paths.config_dir().to_path_buf(),
            paths.state_dir().to_path_buf(),
        ]
    };
    path_rules.extend(private_paths.into_iter().map(|path| {
        PathAccessRule::new(path, PathAccess::Deny, PathAccessRuleSource::ProductPrivate)
    }));
    let environment_overrides: Vec<(OsString, OsString)> = config
        .map(MerryConfig::process_environment_overrides)
        .transpose()?
        .unwrap_or_default()
        .into_iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect();
    Ok(ActionProcessBackendOptions::new()
        .with_path_rules(path_rules)
        .with_network_requests_allowed(config.is_none_or(MerryConfig::network_requests_allowed))
        .with_environment_overrides(environment_overrides))
}

pub(crate) fn configured_runtime_builder(
    session_id: SessionId,
    config: Option<&MerryConfig>,
) -> Result<RuntimeBuilder, CliError> {
    Ok(Runtime::builder(session_id)
        .automatic_compaction(automatic_compaction_config(config).map_err(unexpected)?))
}

pub(crate) async fn prepared_action_process_backend_options(
    config: Option<&MerryConfig>,
    mode: crate::coding::ProcessExecutionMode,
) -> Result<ActionProcessBackendOptions, CliError> {
    let options = action_process_backend_options(config).map_err(unexpected)?;
    if !mode.uses_inner_sandbox() {
        return Ok(options);
    }
    let paths = XdgPaths::from_env().map_err(unexpected)?;
    let home = config
        .map(MerryConfig::home)
        .unwrap_or_else(|| paths.home());
    let gpg_enabled = config.is_some_and(|config| {
        config
            .host_integrations()
            .contains(&merry_runtime::HostIntegration::GpgAgent)
    });
    match merry_process::GpgAgentSockets::discover(home, options.environment_overrides())
        .await
        .map_err(unexpected)?
    {
        Some(sockets) => {
            if gpg_enabled {
                sockets.validate_public_key_access().map_err(unexpected)?;
            }
            Ok(options.with_gpg_agent_sockets(sockets))
        }
        None if gpg_enabled => Err(unexpected(
            "gpg_agent requires gpgconf in PATH for socket discovery",
        )),
        None => Ok(options),
    }
}

#[cfg(all(test, target_os = "linux"))]
mod network_tests;

#[cfg(test)]
mod tests {
    use super::{
        action_process_backend_options, configured_runtime_builder, generation_config,
        validate_loaded_config,
    };
    use crate::config::{MerryConfig, XdgPaths};
    use crate::runtime_events::collect_runtime_step_events;
    use crate::testing::ScriptedProvider;
    use merry_llm::{
        FinishReason, GenerationConfig, ModelCapabilities, ModelEvent, ModelName, ModelOutput,
        ModelResponse, ServiceTier,
    };
    use merry_runtime::{PathAccessRuleSource, RuntimeModelRole, StepContext, StepInput};
    use std::{fs, path::PathBuf, sync::Arc};

    #[test]
    fn configured_host_integrations_do_not_preauthorize_inner_actions() {
        let paths = XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some("[permissions]\nssh_agent = true\ngpg_agent = true\ndbus = true\n"),
            &paths,
        )
        .unwrap()
        .unwrap();
        assert_eq!(config.host_integrations().len(), 3);
        let options = action_process_backend_options(Some(&config)).unwrap();
        assert!(options.host_integrations().is_empty());
    }

    #[test]
    fn action_backend_preauthorizes_explicit_path_access() {
        let paths = XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[permissions]
readonly_paths = ["/srv/trusted-readonly"]
readwrite_paths = ["/srv/trusted-writable"]
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let options = action_process_backend_options(Some(&config))
            .expect("action backend options should build");
        let readonly = options
            .path_rules()
            .iter()
            .find(|rule| rule.path() == std::path::Path::new("/srv/trusted-readonly"))
            .expect("configured read-only path should remain visible to the inner runner");
        assert_eq!(readonly.access(), merry_runtime::PathAccess::ReadOnly);
        assert_eq!(readonly.source(), PathAccessRuleSource::TrustedGlobalConfig);

        let writable = options
            .path_rules()
            .iter()
            .find(|rule| rule.path() == std::path::Path::new("/srv/trusted-writable"))
            .expect("configured writable ceiling should remain visible to the inner runner");
        assert_eq!(writable.access(), merry_runtime::PathAccess::ReadWrite);
        assert_eq!(writable.source(), PathAccessRuleSource::TrustedGlobalConfig);
        assert!(options.path_rules().iter().any(|rule| {
            rule.source() == PathAccessRuleSource::DefaultDevelopmentBaseline
                && rule.access() == merry_runtime::PathAccess::ReadOnly
        }));
    }

    #[test]
    fn managed_only_provider_catalog_passes_eager_config_validation() {
        let temp = tempfile::tempdir().expect("temp dir");
        let paths = XdgPaths::from_parts(
            temp.path().join("home"),
            Some(temp.path().join("config")),
            Some(temp.path().join("state")),
        );
        fs::create_dir_all(paths.managed_secrets_dir()).expect("managed secrets dir");
        fs::write(
            paths.managed_secrets_dir().join("opencode.key"),
            "sk-test\n",
        )
        .expect("managed secret");
        fs::write(
            paths.managed_providers_file(),
            r#"
version = 1

[providers.opencode]
display_name = "OpenCode"
default_model = "deepseek-v4-pro"
type = "openai-compatible"
base_url = "https://opencode.example.test/v1"
api_key_file = "managed/secrets/opencode.key"
"#,
        )
        .expect("managed provider registry");
        let config = MerryConfig::load_optional(&paths)
            .expect("config should load")
            .expect("managed catalog should be present");

        validate_loaded_config(Some(&config), &paths)
            .expect("provider catalog should not require a default selection");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn product_private_paths_are_not_inherited_as_task_grants() {
        use merry_process::{LocalProcessBackend, ProcessBackend, ProcessBackendMode};
        use merry_runtime::{ProcessActionIntent, ProcessEnvPolicy, ProcessRunnerContext};
        let fixture = tempfile::tempdir().unwrap();
        let paths = XdgPaths::from_parts(
            fixture.path().to_path_buf(),
            Some(fixture.path().join("config")),
            Some(fixture.path().join("state")),
        );
        fs::create_dir_all(paths.config_dir()).unwrap();
        fs::create_dir_all(paths.state_dir()).unwrap();
        let workspace = fixture.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        fs::write(paths.config_file(), "private config fixture").unwrap();
        fs::write(paths.state_dir().join("session"), "private session fixture").unwrap();
        let credential = paths.config_base_dir().join("provider-key");
        fs::write(&credential, "synthetic credential fixture").unwrap();
        let config = MerryConfig::load_optional_from_text(Some("[providers.test]\ntype = 'openai-compatible'\nbase_url = 'https://provider.example.test'\napi_key_file = '../provider-key'\n"), &paths).unwrap().unwrap();
        let options = action_process_backend_options(Some(&config)).unwrap();
        let backend =
            LocalProcessBackend::new(&workspace, ProcessBackendMode::Isolated, options).unwrap();
        let session = backend.new_session();
        for path in [
            paths.config_file().to_path_buf(),
            paths.state_dir().join("session"),
            credential,
        ] {
            let intent = ProcessActionIntent::new(
                vec!["/usr/bin/cat".into(), path.to_str().unwrap().into()],
                None,
                ProcessEnvPolicy::empty(),
                None,
                1024,
                1024,
            )
            .unwrap();
            let output = session
                .runner()
                .run(intent, ProcessRunnerContext::new(Default::default()))
                .await
                .unwrap();
            assert_eq!(output.stdout_text(), "", "{output:?}");
        }
    }

    #[test]
    fn generation_config_uses_provider_default_reasoning_effort() {
        let paths = XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"
reasoning_effort = "high"

[providers.openai-compatible]
reasoning_effort = "low"
api_key = "sk-test"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let generation = generation_config(Some(&config)).expect("generation config should load");

        assert_eq!(
            generation.reasoning_effort().map(|effort| effort.as_str()),
            Some("high")
        );
    }

    #[test]
    fn generation_config_preserves_provider_reasoning_default_when_unset() {
        let paths = XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"

[providers.openai-compatible]
api_key = "sk-test"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let generation = generation_config(Some(&config)).expect("generation config should load");

        assert_eq!(
            generation.reasoning_effort().map(|effort| effort.as_str()),
            None
        );
    }

    #[test]
    fn generation_config_uses_provider_default_service_tier_over_named_provider() {
        let paths = XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"
service_tier = "priority"

[providers.openai-compatible]
service_tier = "flex"
api_key = "sk-test"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let generation = generation_config(Some(&config)).expect("generation config should load");

        assert_eq!(generation.service_tier(), Some(ServiceTier::Priority));
    }

    #[test]
    fn generation_config_falls_back_to_named_provider_service_tier() {
        let paths = XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"

[providers.openai-compatible]
service_tier = "flex"
api_key = "sk-test"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let generation = generation_config(Some(&config)).expect("generation config should load");

        assert_eq!(generation.service_tier(), Some(ServiceTier::Flex));
    }

    #[test]
    fn generation_config_omits_service_tier_when_unset() {
        let paths = XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"

[providers.openai-compatible]
api_key = "sk-test"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let generation = generation_config(Some(&config)).expect("generation config should load");

        assert_eq!(generation.service_tier(), None);
    }

    #[test]
    fn invalid_service_tier_is_rejected() {
        let paths = XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"

[providers.openai-compatible]
service_tier = "turbo"
api_key = "sk-test"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let error = generation_config(Some(&config)).expect_err("service tier should be rejected");

        assert!(
            error
                .to_string()
                .contains("providers.openai-compatible.service_tier is invalid"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn service_tier_is_rejected_for_anthropic_providers() {
        let paths = XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "anthropic"
model = "claude-test"

[providers.anthropic]
service_tier = "priority"
api_key = "sk-test"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let error = generation_config(Some(&config)).expect_err("service tier should be rejected");

        assert!(
            error
                .to_string()
                .contains("service_tier is only supported for openai-compatible providers"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn default_level_service_tier_is_rejected_for_anthropic_providers() {
        let paths = XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "anthropic"
model = "claude-test"
service_tier = "priority"

[providers.anthropic]
api_key = "sk-test"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let error = generation_config(Some(&config)).expect_err("service tier should be rejected");

        assert!(
            error.to_string().contains(
                "providers.default.service_tier is only supported for openai-compatible providers"
            ),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn configured_runtime_builder_applies_auto_compaction_config() {
        let paths = XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[runtime.auto_compaction]
retained_model_turns = 2
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");
        let primary = ScriptedProvider::new(vec![
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("old assistant from configured builder")],
                    FinishReason::Stop,
                    None,
                ),
            })],
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text(
                        "tail one assistant from configured builder",
                    )],
                    FinishReason::Stop,
                    None,
                ),
            })],
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text(
                        &"tail two assistant from configured builder ".repeat(300),
                    )],
                    FinishReason::Stop,
                    None,
                ),
            })],
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("final after configured compaction")],
                    FinishReason::Stop,
                    None,
                ),
            })],
        ])
        .with_capabilities(
            ModelCapabilities::new(true, true, false, true, Some(12_000), Some(16))
                .expect("valid capabilities"),
        );
        let compactor = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text(
                    r#"{
                      "confirmed_decisions": [],
                      "rejected_approaches": [],
                      "constraints_preferences_boundaries": [],
                      "corrected_misunderstandings": [],
                      "durable_conclusions": [
                        {
                          "id": "c1",
                          "text": "Configured builder compacted the old turn only.",
                          "refs": ["h0", "h1"]
                        }
                      ],
                      "open_questions": [],
                      "current_progress_and_next_steps": [],
                      "exact_details": [],
                      "handoffs": []
                    }"#,
                )],
                FinishReason::Stop,
                None,
            ),
        })]]);
        let runtime = configured_runtime_builder(
            merry_core::SessionId::new("configured-builder-auto-compaction").unwrap(),
            Some(&config),
        )
        .expect("configured builder should build")
        .model_provider(
            Arc::new(primary.clone()),
            ModelName::new("debug-model").unwrap(),
        )
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            ModelName::new("debug-compactor").unwrap(),
        )
        .build()
        .expect("runtime should build");
        let context = StepContext::default().with_generation_config(
            GenerationConfig::new(Some(16), false).expect("valid generation config"),
        );

        collect_runtime_step_events(
            &runtime,
            StepInput::user_text(&"old user from configured builder ".repeat(850))
                .expect("valid input"),
            context.clone(),
        )
        .await
        .expect("old step should run");
        collect_runtime_step_events(
            &runtime,
            StepInput::user_text("tail one user from configured builder").expect("valid input"),
            context.clone(),
        )
        .await
        .expect("tail one step should run");
        collect_runtime_step_events(
            &runtime,
            StepInput::user_text("tail two user from configured builder").expect("valid input"),
            context.clone(),
        )
        .await
        .expect("tail two step should run");
        collect_runtime_step_events(
            &runtime,
            StepInput::user_text("current user from configured builder").expect("valid input"),
            context,
        )
        .await
        .expect("current step should run");

        let compactor_requests = compactor.recorded_requests();
        assert_eq!(compactor_requests.len(), 1);
        let compaction_text = compactor_requests[0]
            .messages()
            .iter()
            .map(|message| message.content().as_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(compaction_text.contains("old user from configured builder"));
        assert!(!compaction_text.contains("tail one user from configured builder"));
        assert!(!compaction_text.contains("tail two user from configured builder"));
        assert!(!compaction_text.contains("current user from configured builder"));
    }
}
