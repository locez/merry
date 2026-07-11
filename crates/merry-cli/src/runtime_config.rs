use crate::cli_error::{CliError, unexpected};
use crate::coding_runtime::ActionProcessBackendOptions;
use crate::config::{self, EffectiveLogSettings, MerryConfig, XdgPaths};
use merry_core::SessionId;
use merry_llm::{GenerationConfig, ReasoningEffort};
use merry_runtime::{AutomaticCompactionConfig, Runtime, RuntimeBuilder};

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
    Ok(GenerationConfig::default().with_reasoning_effort(reasoning_effort))
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
    let path_rules = config
        .map(MerryConfig::trusted_global_path_rules)
        .transpose()?
        .unwrap_or_default();
    let network_allowed = config
        .map(MerryConfig::permissions_network_allowed)
        .unwrap_or(false);
    Ok(ActionProcessBackendOptions {
        path_rules,
        network_allowed,
    })
}

pub(crate) fn configured_runtime_builder(
    session_id: SessionId,
    config: Option<&MerryConfig>,
) -> Result<RuntimeBuilder, CliError> {
    Ok(Runtime::builder(session_id)
        .automatic_compaction(automatic_compaction_config(config).map_err(unexpected)?))
}

#[cfg(test)]
mod tests {
    use super::{configured_runtime_builder, generation_config, validate_loaded_config};
    use crate::config::{MerryConfig, XdgPaths};
    use crate::runtime_events::collect_runtime_step_events;
    use crate::testing::ScriptedProvider;
    use merry_llm::{
        FinishReason, GenerationConfig, ModelCapabilities, ModelEvent, ModelName, ModelOutput,
        ModelResponse,
    };
    use merry_runtime::{RuntimeModelRole, StepContext, StepInput};
    use std::{fs, path::PathBuf, sync::Arc};

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

    #[test]
    fn generation_config_uses_provider_default_reasoning_effort() {
        let paths = XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"
reasoning_effort = "low"

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
            Some("low")
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
                        "tail two assistant from configured builder",
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
            ModelCapabilities::new(true, true, false, true, Some(4_000), Some(16))
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
            StepInput::user_text(&"old user from configured builder ".repeat(220))
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
