use super::*;
use crate::config::{XdgPaths, managed_provider::derive_provider_alias};
use std::{collections::BTreeSet, path::PathBuf};

fn home() -> PathBuf {
    PathBuf::from("/home/alice")
}

#[test]
fn derives_readable_unique_managed_provider_aliases() {
    let used = BTreeSet::from(["opencode".to_owned(), "opencode-2".to_owned()]);

    assert_eq!(
        derive_provider_alias("OpenCode", &used)
            .expect("alias should derive")
            .as_str(),
        "opencode-3"
    );
    assert_eq!(
        derive_provider_alias("OpenCode Gateway", &BTreeSet::new())
            .expect("alias should derive")
            .as_str(),
        "opencode-gateway"
    );
    assert!(derive_provider_alias("Default", &BTreeSet::new()).is_err());
}

#[test]
fn provider_profile_preserves_display_name_and_default_model() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let config = MerryConfig::load_with_managed_from_text(
        Some(
            r#"
[providers.default]
provider = "opencode"
model = "deepseek-v4-pro"
"#,
        ),
        Some(
            r#"
version = 1

[providers.opencode]
display_name = "OpenCode"
default_model = "deepseek-v4-pro"
reasoning_effort = "low"
type = "openai-compatible"
api_key_file = "secrets/opencode.key"
"#,
        ),
        &paths,
    )
    .expect("config should parse")
    .expect("config should exist");

    let profile = config
        .provider_profile("opencode")
        .expect("profile should resolve");

    assert_eq!(profile.alias().as_str(), "opencode");
    assert_eq!(profile.display_name(), "OpenCode");
    assert_eq!(
        profile
            .default_model()
            .map(merry::providers::ModelName::as_str),
        Some("deepseek-v4-pro")
    );
    assert_eq!(profile.protocol(), Some(OpenAiProtocol::Responses));
    assert_eq!(
        profile.reasoning_effort().map(ReasoningEffort::as_str),
        Some("low")
    );
    let provider = config
        .provider_by_alias("opencode")
        .expect("provider should resolve");
    assert!(matches!(
        provider,
        EffectiveProviderConfig::OpenAiCompatible(provider)
            if provider.reasoning_effort.as_ref().map(ReasoningEffort::as_str) == Some("low")
    ));
}

#[test]
fn parses_provider_config_and_retry_policy() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let config = MerryConfig::load_with_managed_from_text(
        Some(
            r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-4.1-mini"
reasoning_effort = "high"

[providers.retry]
enabled = true
max_attempts = 7
initial_delay_ms = 500
max_delay_ms = 120000
max_elapsed_ms = 300000
jitter = true
"#,
        ),
        Some(
            r#"
version = 1

[providers.openai-compatible]
type = "openai-compatible"
base_url = "https://api.example.test/v1"
api_key_file = "secrets/openai.key"
"#,
        ),
        &paths,
    )
    .expect("config should parse")
    .expect("config should be present");

    let default = config.default_provider().expect("provider should validate");
    assert_eq!(default.model, "gpt-4.1-mini");
    assert_eq!(
        default
            .reasoning_effort
            .as_ref()
            .map(|effort| effort.as_str()),
        Some("high")
    );
    let super::EffectiveProviderConfig::OpenAiCompatible(provider) = default.provider else {
        panic!("expected openai-compatible provider");
    };
    assert_eq!(
        provider.base_url.as_deref(),
        Some("https://api.example.test/v1")
    );
    assert_eq!(
        provider.api_key_file,
        PathBuf::from("/home/alice/.local/share/merry/providers/secrets/openai.key")
    );
    let retry = config
        .provider_retry_policy()
        .expect("retry policy should validate")
        .expect("retry policy should be configured");
    assert!(retry.enabled());
    assert_eq!(retry.max_attempts(), 7);
    assert_eq!(retry.initial_delay(), std::time::Duration::from_millis(500));
    assert_eq!(retry.max_delay(), std::time::Duration::from_secs(120));
    assert_eq!(retry.max_elapsed(), std::time::Duration::from_secs(300));
    assert!(retry.jitter());
}

#[test]
fn rejects_named_providers_in_config_toml() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let err = MerryConfig::load_optional_from_text(
        Some(
            r#"
[providers.openai-compatible]
base_url = "https://api.example.test/v1"
api_key = "sk-inline-secret"
"#,
        ),
        &paths,
    )
    .expect_err("named providers in config.toml must be rejected");
    assert!(matches!(err, ConfigError::Parse { .. }), "{err}");
    assert!(!err.to_string().contains("sk-inline-secret"));
}

#[test]
fn default_level_service_tier_is_rejected_for_anthropic_providers() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let config = MerryConfig::load_with_managed_from_text(
        Some(
            r#"
[providers.default]
provider = "claude"
model = "claude-sonnet-test"
service_tier = "priority"
"#,
        ),
        Some(
            r#"
version = 1

[providers.claude]
type = "anthropic"
api_key_file = "managed/secrets/claude.key"
"#,
        ),
        &paths,
    )
    .expect("config should parse")
    .expect("config should exist");

    let error = config
        .default_provider()
        .expect_err("an Anthropic default provider must reject a service tier");

    assert!(
        error.to_string().contains(
            "providers.default.service_tier is only supported for openai-compatible providers"
        ),
        "unexpected error: {error}"
    );
}

#[test]
fn ultrafast_service_tier_requires_the_responses_protocol() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let config_texts = |protocol: &str, tier_location: &str| {
        let (default_tier, provider_tier) = match tier_location {
            "default" => ("service_tier = \"ultrafast\"", ""),
            _ => ("", "service_tier = \"ultrafast\""),
        };
        (
            format!(
                r#"
[providers.default]
provider = "compat"
model = "gpt-test"
{default_tier}
"#
            ),
            format!(
                r#"
version = 1

[providers.compat]
type = "openai-compatible"
protocol = "{protocol}"
{provider_tier}
api_key_file = "managed/secrets/compat.key"
"#
            ),
        )
    };

    for tier_location in ["default", "named"] {
        let (user, managed) = config_texts("chat_completions", tier_location);
        let config = MerryConfig::load_with_managed_from_text(Some(&user), Some(&managed), &paths)
            .expect("config should parse")
            .expect("config should exist");

        let error = config
            .default_provider()
            .expect_err("ultrafast is a Responses-only tier");

        assert!(
            error
                .to_string()
                .contains("requires protocol = \"responses\""),
            "unexpected error for the {tier_location} service_tier: {error}"
        );

        let (user, managed) = config_texts("responses", tier_location);
        let default = MerryConfig::load_with_managed_from_text(Some(&user), Some(&managed), &paths)
            .expect("config should parse")
            .expect("config should exist")
            .default_provider()
            .expect("the Responses protocol accepts ultrafast");

        assert_eq!(default.service_tier, Some(ServiceTier::Ultrafast));
    }
}

#[test]
fn parses_chat_completions_protocol_and_anthropic_provider() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let openai = MerryConfig::load_with_managed_from_text(
        Some(
            r#"
[providers.default]
provider = "compat"
model = "gpt-test"
"#,
        ),
        Some(
            r#"
version = 1

[providers.compat]
type = "openai-compatible"
protocol = "chat_completions"
api_key_file = "managed/secrets/compat.key"
"#,
        ),
        &paths,
    )
    .expect("config should parse")
    .expect("config should exist")
    .default_provider()
    .expect("default provider should validate");
    let EffectiveProviderConfig::OpenAiCompatible(openai) = openai.provider else {
        panic!("OpenAI-compatible provider expected");
    };
    assert_eq!(openai.alias, "compat");
    assert_eq!(openai.protocol, OpenAiProtocol::ChatCompletions);

    let anthropic = MerryConfig::load_with_managed_from_text(
        Some(
            r#"
[providers.default]
provider = "claude"
model = "claude-sonnet-test"
"#,
        ),
        Some(
            r#"
version = 1

[providers.claude]
type = "anthropic"
base_url = "https://anthropic.example.test"
api_version = "2023-06-01"
default_max_output_tokens = 2048
api_key_file = "managed/secrets/claude.key"
"#,
        ),
        &paths,
    )
    .expect("config should parse")
    .expect("config should exist")
    .default_provider()
    .expect("default provider should validate");
    assert_eq!(anthropic.alias, "claude");
    assert_eq!(anthropic.model, "claude-sonnet-test");
    let EffectiveProviderConfig::Anthropic(anthropic) = anthropic.provider else {
        panic!("Anthropic provider expected");
    };
    assert_eq!(anthropic.alias, "claude");
    assert_eq!(anthropic.default_max_output_tokens, Some(2048));
}

#[test]
fn rejects_inline_api_key_or_missing_api_key_file_in_managed_store() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let inline = MerryConfig::load_with_managed_from_text(
        None,
        Some(
            r#"
version = 1

[providers.openai-compatible]
type = "openai-compatible"
api_key = "sk-inline-secret"
"#,
        ),
        &paths,
    )
    .expect_err("managed store must reject inline api_key");
    assert!(matches!(inline, ConfigError::Parse { .. }), "{inline}");

    let missing_file = MerryConfig::load_with_managed_from_text(
        None,
        Some(
            r#"
version = 1

[providers.openai-compatible]
type = "openai-compatible"
"#,
        ),
        &paths,
    )
    .expect("managed TOML should parse")
    .expect("config should be present")
    .provider_by_alias("openai-compatible")
    .expect_err("managed provider without api_key_file should fail");
    assert!(
        missing_file.to_string().contains("missing api_key_file"),
        "{missing_file}"
    );
}

#[test]
fn redacted_provider_debug_does_not_include_api_key_file_contents() {
    let provider = EffectiveOpenAiProviderConfig {
        model: Some("gpt-test".to_owned()),
        reasoning_effort: Some(ReasoningEffort::new("high").expect("valid effort")),
        service_tier: Some(ServiceTier::Priority),
        alias: "openai-compatible".to_owned(),
        protocol: OpenAiProtocol::Responses,
        base_url: Some("https://api.example.test/v1".to_owned()),
        api_key_file: PathBuf::from("/home/alice/.local/share/merry/providers/secrets/openai.key"),
    };
    let debug = format!("{provider:?}");
    assert!(debug.contains("openai.key"));
    assert!(!debug.contains("sk-"));
}
