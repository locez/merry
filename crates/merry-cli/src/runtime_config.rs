use crate::config::{self, EffectiveLogSettings, MerryConfig, XdgPaths};
use crate::{CliError, unexpected};
use merry_core::SessionId;
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

pub(crate) fn configured_runtime_builder(
    session_id: SessionId,
    config: Option<&MerryConfig>,
) -> Result<RuntimeBuilder, CliError> {
    Ok(Runtime::builder(session_id)
        .automatic_compaction(automatic_compaction_config(config).map_err(unexpected)?))
}

#[cfg(test)]
mod tests {
    use super::configured_runtime_builder;
    use crate::config::{MerryConfig, XdgPaths};
    use crate::runtime_events::collect_runtime_step_events;
    use crate::test_support::ScriptedProvider;
    use merry_llm::{
        FinishReason, ModelCapabilities, ModelEvent, ModelName, ModelOutput, ModelResponse,
    };
    use merry_runtime::{RuntimeModelRole, StepContext, StepInput};
    use std::{path::PathBuf, sync::Arc};

    #[tokio::test]
    async fn configured_runtime_builder_applies_auto_compaction_config() {
        let paths = XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[runtime.auto_compaction]
retained_raw_tail_items = 4
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
            ModelCapabilities::new(true, true, false, true, Some(420), Some(16))
                .expect("valid capabilities"),
        );
        let compactor = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text(
                    r#"{
                      "claims": [
                        {
                          "id": "c1",
                          "kind": "completed_action",
                          "text": "Configured builder compacted the old turn only.",
                          "refs": ["r1", "r2"]
                        }
                      ],
                      "working_intent": null
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

        collect_runtime_step_events(
            &runtime,
            StepInput::user_text(&"old user from configured builder ".repeat(70))
                .expect("valid input"),
            StepContext::default(),
        )
        .await
        .expect("old step should run");
        collect_runtime_step_events(
            &runtime,
            StepInput::user_text("tail one user from configured builder").expect("valid input"),
            StepContext::default(),
        )
        .await
        .expect("tail one step should run");
        collect_runtime_step_events(
            &runtime,
            StepInput::user_text("tail two user from configured builder").expect("valid input"),
            StepContext::default(),
        )
        .await
        .expect("tail two step should run");
        collect_runtime_step_events(
            &runtime,
            StepInput::user_text("current user from configured builder").expect("valid input"),
            StepContext::default(),
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
