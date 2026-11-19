use crate::cli_error::{CliError, unexpected};
use crate::coding::ActionProcessBackendOptions;
use crate::config::{self, EffectiveLogSettings, MerryConfig, XdgPaths};
use merry_core::SessionId;
use merry_llm::{GenerationConfig, ReasoningEffort, ServiceTier};
use merry_runtime::{
    CompactionConfig, MemoryLayout, PathAccess, PathAccessRule, PathAccessRuleSource, Runtime,
    RuntimeBuilder,
};
use std::{
    env,
    ffi::OsString,
    path::{Component, Path, PathBuf},
};

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

/// Paths ordinary local development commands can use without another grant.
///
/// Cargo's registry and git database, and the XDG cache, are writable. Binaries
/// and rustup toolchains stay read-only. Credentials under the home directory
/// are not included.
fn default_development_path_rules(home: &Path) -> Vec<PathAccessRule> {
    let cargo_home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".cargo"));
    let rustup_home = env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".rustup"));
    let cache_home = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".cache"));

    [
        (home.join(".local/bin"), PathAccess::ReadOnly),
        (cargo_home.join("bin"), PathAccess::ReadOnly),
        (cargo_home.join("registry"), PathAccess::ReadWrite),
        (cargo_home.join("git"), PathAccess::ReadWrite),
        (rustup_home.join("toolchains"), PathAccess::ReadOnly),
        (cache_home, PathAccess::ReadWrite),
    ]
    .into_iter()
    .filter(|(path, _)| is_clean_absolute_path(path))
    .map(|(path, access)| {
        PathAccessRule::new(
            path,
            access,
            PathAccessRuleSource::DefaultDevelopmentBaseline,
        )
    })
    .collect()
}

fn is_clean_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
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
        .or_else(|| env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/home/merry"));
    let mut path_rules = default_development_path_rules(&home);
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
    // The memory tree is outside the workspace. Grant only that root, whether
    // or not the workspace itself is writable. Config and state stay denied.
    if let Ok(memory_root) = MemoryLayout::memory_root_from_env()
        && is_clean_absolute_path(&memory_root)
    {
        path_rules.push(PathAccessRule::new(
            memory_root,
            PathAccess::ReadWrite,
            PathAccessRuleSource::DurableMemory,
        ));
    }
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
        .with_environment_overrides(environment_overrides)
        // Trusted global config is the user's own preauthorization for the
        // inner action sandbox; the endpoints are still validated per action.
        .with_host_integrations(
            config
                .map(MerryConfig::host_integrations)
                .unwrap_or_default(),
        ))
}

pub(crate) fn configured_runtime_builder(
    session_id: SessionId,
    config: Option<&MerryConfig>,
) -> Result<RuntimeBuilder, CliError> {
    let review_rules = config
        .map(MerryConfig::permission_review_rules)
        .transpose()
        .map_err(unexpected)?
        .unwrap_or_default();
    Ok(Runtime::builder(session_id)
        .automatic_compaction(automatic_compaction_config(config).map_err(unexpected)?)
        .permission_review_rules(review_rules))
}

pub(crate) async fn prepared_action_process_backend_options(
    config: Option<&MerryConfig>,
    mode: crate::coding::SandboxMode,
) -> Result<ActionProcessBackendOptions, CliError> {
    let options = action_process_backend_options(config).map_err(unexpected)?;
    if !mode.uses_action_sandbox() {
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

#[cfg(test)]
fn is_baseline_write_path(path: &Path) -> bool {
    ["/tmp", "/var/tmp", "/dev", "/dev/shm", "/dev/pts"]
        .iter()
        .any(|root| path == Path::new(root) || path.starts_with(root))
}

#[cfg(test)]
mod tests {
    use super::{
        action_process_backend_options, generation_config, temp_home_outside_baseline_writes,
        validate_loaded_config,
    };
    use crate::config::{MerryConfig, XdgPaths};
    use merry_llm::ServiceTier;
    use merry_runtime::{MemoryLayout, PathAccessRuleSource};
    use std::{fs, path::PathBuf};

    #[test]
    fn configured_host_integrations_preauthorize_inner_actions() {
        use merry_runtime::HostIntegration;

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
        assert_eq!(readonly.access(), merry_runtime::PathAccess::ReadOnly);
        assert_eq!(readonly.source(), PathAccessRuleSource::TrustedGlobalConfig);

        let writable = options
            .path_rules()
            .iter()
            .find(|rule| rule.path() == std::path::Path::new("/srv/trusted-writable"))
            .expect("configured writable ceiling should remain visible to the inner runner");
        assert_eq!(writable.access(), merry_runtime::PathAccess::ReadWrite);
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
        assert_eq!(rule.access(), merry_runtime::PathAccess::ReadWrite);
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
        let fixture = temp_home_outside_baseline_writes();
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
        let credential = paths
            .config_dir()
            .parent()
            .expect("merry config dir has a parent")
            .join("provider-key");
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
}
