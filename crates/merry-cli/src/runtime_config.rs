use crate::cli_error::{CliError, unexpected};
use crate::coding::ActionProcessBackendOptions;
use crate::config::{self, EffectiveLogSettings, MerryConfig, XdgPaths};
use merry::coding::MemoryLayout;
use merry::paths::{ProtectedHostPaths, user_home_dir};
use merry::process::GpgAgentSockets;
use merry::providers::{GenerationConfig, ReasoningEffort, ServiceTier};
use merry::{CompactionConfig, HistoryRetention};
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
use std::path::Path;
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
    let _ = history_retention_config(Some(config))?;
    let _ = subagents_config(Some(config))?;
    let _ = config.trusted_global_path_rules()?;
    let _ = config.permission_review_rules()?;
    let _ = config.process_environment_overrides()?;
    let _ = config.skill_roots()?;
    let _ = config.runtime_models()?;
    let _ = config.profile();
    let _ = config.cli_defaults()?;
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
) -> Result<CompactionConfig, config::ConfigError> {
    config
        .map(MerryConfig::automatic_compaction_config)
        .transpose()
        .map(Option::unwrap_or_default)
}

pub(crate) fn history_retention_config(
    config: Option<&MerryConfig>,
) -> Result<HistoryRetention, config::ConfigError> {
    config
        .map(MerryConfig::history_retention)
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

/// Builds the action backend inputs that need no asynchronous discovery.
///
/// GnuPG socket discovery has to run on the async runtime, so this is the
/// synchronous base for [`prepared_action_process_backend_options`], which every
/// product surface uses. Trusted global configuration preauthorizes those
/// paths here; the per-action endpoint checks still happen in the process backend.
fn action_process_backend_options(
    config: Option<&MerryConfig>,
) -> Result<ActionProcessBackendOptions, config::ConfigError> {
    let home = config
        .map(|config| config.home().to_path_buf())
        .or_else(user_home_dir)
        .unwrap_or_else(|| {
            if cfg!(windows) {
                env::temp_dir()
            } else {
                PathBuf::from("/home/merry")
            }
        });
    let (config_dir, skill_roots, protected) = if let Some(config) = config {
        (
            config.config_dir().to_path_buf(),
            config.skill_roots()?,
            ProtectedHostPaths::new(config.private_process_paths()?),
        )
    } else {
        let paths = XdgPaths::from_env()?;
        let config_dir = paths.config_dir().to_path_buf();
        let default_skills = config_dir.join("skills");
        (
            config_dir,
            vec![default_skills],
            paths.protected_host_paths(),
        )
    };
    let memory_root = MemoryLayout::memory_root_from_env().ok();
    let mut options = ActionProcessBackendOptions::new().with_default_development_baseline(&home);
    if let Some(config) = config {
        options = options.extend_path_rules(config.trusted_global_path_rules()?);
    }
    // Merry's own non-secret config directory, configured skill roots, and
    // durable memory stay writable while state and managed credentials stay
    // denied through the shared ProtectedHostPaths contract.
    options = options
        .with_product_writable_paths(&config_dir, &skill_roots, memory_root.as_deref())
        .with_protected_host_paths(&protected);
    let environment_overrides: Vec<(OsString, OsString)> = config
        .map(MerryConfig::process_environment_overrides)
        .transpose()?
        .unwrap_or_default()
        .into_iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect();
    Ok(options
        .with_network_requests_allowed(config.is_none_or(MerryConfig::network_requests_allowed))
        .with_environment_overrides(environment_overrides)
        // Trusted global config is the user's own preauthorization for the
        // inner action sandbox; the endpoints are still validated per action.
        .with_host_integrations(
            config
                .map(MerryConfig::host_integrations)
                .unwrap_or_default(),
        ))
}

pub(crate) fn private_tool_paths(
    config: Option<&MerryConfig>,
) -> Result<Vec<PathBuf>, config::ConfigError> {
    let protected = if let Some(config) = config {
        ProtectedHostPaths::new(config.private_process_paths()?)
    } else {
        XdgPaths::from_env()?.protected_host_paths()
    };
    Ok(protected.into_vec())
}

pub(crate) async fn prepared_action_process_backend_options(
    config: Option<&MerryConfig>,
    mode: crate::coding::SandboxMode,
) -> Result<ActionProcessBackendOptions, CliError> {
    prepared_action_process_backend_options_with_overrides(
        config,
        mode,
        &crate::tui::preferences::TuiPermissionPreferences::default(),
    )
    .await
}

pub(crate) async fn prepared_action_process_backend_options_with_overrides(
    config: Option<&MerryConfig>,
    mode: crate::coding::SandboxMode,
    overrides: &crate::tui::preferences::TuiPermissionPreferences,
) -> Result<ActionProcessBackendOptions, CliError> {
    let mut options = action_process_backend_options(config).map_err(unexpected)?;
    if let Some(network) = overrides.network {
        options = options.with_network_requests_allowed(network);
    }
    if overrides.ssh_agent.is_some() || overrides.gpg_agent.is_some() || overrides.dbus.is_some() {
        let mut integrations = config
            .map(MerryConfig::host_integrations)
            .unwrap_or_default();
        set_host_integration(
            &mut integrations,
            merry::permissions::HostIntegration::SshAgent,
            overrides.ssh_agent,
        );
        set_host_integration(
            &mut integrations,
            merry::permissions::HostIntegration::GpgAgent,
            overrides.gpg_agent,
        );
        set_host_integration(
            &mut integrations,
            merry::permissions::HostIntegration::SessionBus,
            overrides.dbus,
        );
        options = options.with_host_integrations(integrations);
    }
    if !mode.uses_action_sandbox() {
        return Ok(options);
    }
    let paths = XdgPaths::from_env().map_err(unexpected)?;
    let home = config
        .map(MerryConfig::home)
        .unwrap_or_else(|| paths.home());
    let gpg_enabled = overrides.gpg_agent.unwrap_or_else(|| {
        config.is_some_and(|config| {
            config
                .host_integrations()
                .contains(&merry::permissions::HostIntegration::GpgAgent)
        })
    });
    match GpgAgentSockets::discover(home, options.environment_overrides())
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

fn set_host_integration(
    integrations: &mut Vec<merry::permissions::HostIntegration>,
    target: merry::permissions::HostIntegration,
    override_value: Option<bool>,
) {
    let Some(enabled) = override_value else {
        return;
    };
    if enabled {
        if !integrations.contains(&target) {
            integrations.push(target);
        }
    } else {
        integrations.retain(|item| *item != target);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod network_tests;

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
fn temp_home_outside_baseline_writes() -> tempfile::TempDir {
    let parent = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_dir() && !is_baseline_write_path(path))
        .or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_dir() && !is_baseline_write_path(path))
        })
        .expect("a writable directory outside /tmp, /var/tmp, and /dev");
    tempfile::tempdir_in(parent).expect("fixture home outside the baseline write allow list")
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
fn is_baseline_write_path(path: &Path) -> bool {
    [
        "/tmp",
        "/private/tmp",
        "/var/tmp",
        "/private/var/tmp",
        "/var/folders",
        "/private/var/folders",
        "/dev",
        "/dev/shm",
        "/dev/pts",
    ]
    .iter()
    .any(|root| path == Path::new(root) || path.starts_with(root))
}

#[cfg(test)]
mod tests {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    use super::temp_home_outside_baseline_writes;
    use super::{action_process_backend_options, generation_config, validate_loaded_config};
    use crate::config::{MerryConfig, XdgPaths};
    use merry::coding::MemoryLayout;
    use merry::permissions::PathAccessRuleSource;
    use merry::providers::ServiceTier;
    use std::{fs, path::PathBuf};

    #[test]
    fn configured_host_integrations_preauthorize_inner_actions() {
        use merry::permissions::HostIntegration;

        let paths = XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some("[permissions]\nssh_agent = true\ngpg_agent = true\ndbus = true\n"),
            &paths,
        )
        .unwrap()
        .unwrap();
        // `MerryConfig::host_integrations()` parsing is covered by the config
        // module; this test owns the mapping into the action backend options.
        let options = action_process_backend_options(Some(&config)).unwrap();
        assert_eq!(
            options.host_integrations(),
            [
                HostIntegration::SshAgent,
                HostIntegration::SessionBus,
                HostIntegration::GpgAgent,
            ]
        );

        let unconfigured =
            MerryConfig::load_optional_from_text(Some("[permissions]\nnetwork = true\n"), &paths)
                .unwrap()
                .unwrap();
        let options = action_process_backend_options(Some(&unconfigured)).unwrap();
        assert!(
            options.host_integrations().is_empty(),
            "only integrations enabled by trusted config may be preauthorized"
        );
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
        assert_eq!(readonly.access(), merry::permissions::PathAccess::ReadOnly);
        assert_eq!(readonly.source(), PathAccessRuleSource::TrustedGlobalConfig);

        let writable = options
            .path_rules()
            .iter()
            .find(|rule| rule.path() == std::path::Path::new("/srv/trusted-writable"))
            .expect("configured writable ceiling should remain visible to the inner runner");
        assert_eq!(writable.access(), merry::permissions::PathAccess::ReadWrite);
        assert_eq!(writable.source(), PathAccessRuleSource::TrustedGlobalConfig);
    }

    #[test]
    fn action_backend_grants_only_the_memory_root() {
        let root = MemoryLayout::memory_root_from_env().expect("this process has a home directory");
        let options =
            action_process_backend_options(None).expect("action backend options should build");
        let rule = options
            .path_rules()
            .iter()
            .find(|rule| rule.path() == root)
            .expect("memory root should be granted");
        assert_eq!(rule.access(), merry::permissions::PathAccess::ReadWrite);
        assert_eq!(rule.source(), PathAccessRuleSource::DurableMemory);
        assert!(options.path_rules().iter().all(|rule| {
            rule.source() != PathAccessRuleSource::DurableMemory || rule.path() == root
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
api_key_file = "secrets/opencode.key"
"#,
        )
        .expect("managed provider registry");
        let config = MerryConfig::load_optional(&paths)
            .expect("config should load")
            .expect("managed catalog should be present");

        validate_loaded_config(Some(&config), &paths)
            .expect("provider catalog should not require a default selection");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn product_private_paths_are_not_inherited_as_task_grants() {
        use merry::process::{LocalProcessBackend, ProcessBackend, ProcessBackendMode};
        use merry::process::{ProcessActionIntent, ProcessEnvPolicy, ProcessRunnerContext};
        let fixture = temp_home_outside_baseline_writes();
        let paths = XdgPaths::from_parts(
            fixture.path().to_path_buf(),
            Some(fixture.path().join("config")),
            Some(fixture.path().join("state")),
        )
        .with_data_home(fixture.path().join("data"));
        let custom_skills = fixture.path().join("custom-skills");
        fs::create_dir_all(paths.config_dir().join("skills")).unwrap();
        fs::create_dir_all(&custom_skills).unwrap();
        fs::create_dir_all(paths.state_dir()).unwrap();
        fs::create_dir_all(paths.managed_secrets_dir()).unwrap();
        let workspace = fixture.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let user_config = format!(
            "[skills]\nroots = [\"{}\"]\n",
            custom_skills.to_str().expect("utf8 skill root")
        );
        fs::write(paths.config_file(), &user_config).unwrap();
        let default_skill = paths.config_dir().join("skills/default.md");
        fs::write(&default_skill, "default skill fixture").unwrap();
        let custom_skill = custom_skills.join("custom.md");
        fs::write(&custom_skill, "custom skill fixture").unwrap();
        fs::write(paths.state_dir().join("session"), "private session fixture").unwrap();
        let credential = paths.managed_secrets_dir().join("test.key");
        fs::write(&credential, "synthetic credential fixture").unwrap();
        fs::write(
            paths.managed_providers_file(),
            r#"
version = 1

[providers.test]
type = "openai-compatible"
base_url = "https://provider.example.test"
api_key_file = "secrets/test.key"
"#,
        )
        .unwrap();
        let config = MerryConfig::load_optional(&paths).unwrap().unwrap();
        let options = action_process_backend_options(Some(&config)).unwrap();
        let backend =
            LocalProcessBackend::new(&workspace, ProcessBackendMode::Isolated, options).unwrap();
        let session = backend.new_session();

        // Merry's own non-secret config directory and configured skill roots are
        // readable and writable by sandboxed actions.
        for (path, expected) in [
            (paths.config_file().to_path_buf(), user_config.as_str()),
            (default_skill.clone(), "default skill fixture"),
            (custom_skill.clone(), "custom skill fixture"),
        ] {
            let intent = ProcessActionIntent::new(
                vec!["/bin/cat".into(), path.to_str().unwrap().into()],
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
            assert_eq!(output.stdout_text(), expected, "{output:?}");
        }
        for writable in [
            paths.config_dir().join("agent-note.toml"),
            paths.config_dir().join("skills/added.md"),
            custom_skills.join("added.md"),
        ] {
            let intent = ProcessActionIntent::new(
                vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf 'ok' > \"$1\"".into(),
                    "sh".into(),
                    writable.to_str().unwrap().into(),
                ],
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
            assert_eq!(output.status().exit_code(), Some(0), "{output:?}");
            assert_eq!(fs::read_to_string(&writable).unwrap(), "ok");
        }

        // Merry state and managed credentials remain strictly unreadable and unwritable.
        for path in [
            paths.state_dir().join("session"),
            paths.managed_providers_file(),
            credential.clone(),
        ] {
            let intent = ProcessActionIntent::new(
                vec!["/bin/cat".into(), path.to_str().unwrap().into()],
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
        let write_secret = ProcessActionIntent::new(
            vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf 'tampered' > \"$1\"".into(),
                "sh".into(),
                credential.to_str().unwrap().into(),
            ],
            None,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .unwrap();
        let output = session
            .runner()
            .run(write_secret, ProcessRunnerContext::new(Default::default()))
            .await
            .unwrap();
        assert_ne!(output.status().exit_code(), Some(0), "{output:?}");
        assert_eq!(
            fs::read_to_string(&credential).unwrap(),
            "synthetic credential fixture"
        );
    }

    #[test]
    fn generation_config_uses_provider_default_reasoning_effort() {
        let paths = XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
        let config = MerryConfig::load_with_managed_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"
reasoning_effort = "high"
"#,
            ),
            Some(
                r#"
version = 1

[providers.openai-compatible]
reasoning_effort = "low"
api_key_file = "secrets/openai.key"
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
        let config = MerryConfig::load_with_managed_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"
"#,
            ),
            Some(
                r#"
version = 1

[providers.openai-compatible]
api_key_file = "secrets/openai.key"
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
        let config = MerryConfig::load_with_managed_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"
service_tier = "priority"
"#,
            ),
            Some(
                r#"
version = 1

[providers.openai-compatible]
service_tier = "flex"
api_key_file = "secrets/openai.key"
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
        let config = MerryConfig::load_with_managed_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"
"#,
            ),
            Some(
                r#"
version = 1

[providers.openai-compatible]
service_tier = "flex"
api_key_file = "secrets/openai.key"
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
        let config = MerryConfig::load_with_managed_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"
"#,
            ),
            Some(
                r#"
version = 1

[providers.openai-compatible]
api_key_file = "secrets/openai.key"
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
        let config = MerryConfig::load_with_managed_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"
"#,
            ),
            Some(
                r#"
version = 1

[providers.openai-compatible]
service_tier = "turbo"
api_key_file = "secrets/openai.key"
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
        let config = MerryConfig::load_with_managed_from_text(
            Some(
                r#"
[providers.default]
provider = "anthropic"
model = "claude-test"
"#,
            ),
            Some(
                r#"
version = 1

[providers.anthropic]
service_tier = "priority"
api_key_file = "secrets/anthropic.key"
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
        let config = MerryConfig::load_with_managed_from_text(
            Some(
                r#"
[providers.default]
provider = "anthropic"
model = "claude-test"
service_tier = "priority"
"#,
            ),
            Some(
                r#"
version = 1

[providers.anthropic]
api_key_file = "secrets/anthropic.key"
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
}
