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
    let config = MerryConfig::load_optional_from_text(
        Some(
            r#"
[providers.default]
provider = "opencode"
model = "deepseek-v4-pro"

[providers.opencode]
display_name = "OpenCode"
default_model = "deepseek-v4-pro"
reasoning_effort = "low"
type = "openai-compatible"
api_key = "sk-test"
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
        profile.default_model().map(merry_llm::ModelName::as_str),
        Some("deepseek-v4-pro")
    );
    assert_eq!(profile.source(), ProviderConfigSource::User);
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
    let config = MerryConfig::load_optional_from_text(
        Some(
            r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-4.1-mini"
reasoning_effort = "high"

[providers.openai-compatible]
base_url = "https://api.example.test/v1"
api_key_file = "secrets/openai.key"

[providers.retry]
enabled = true
max_attempts = 7
initial_delay_ms = 500
max_delay_ms = 120000
max_elapsed_ms = 300000
jitter = true
"#,
        ),
        &paths,
    )
    .expect("config should parse")
    .expect("config should be present");

    let provider = config
        .openai_compatible_provider()
        .expect("provider should validate");
    assert_eq!(provider.model.as_deref(), Some("gpt-4.1-mini"));
    assert_eq!(
        provider
            .reasoning_effort
            .as_ref()
            .map(|effort| effort.as_str()),
        Some("high")
    );
    assert_eq!(
        provider.base_url.as_deref(),
        Some("https://api.example.test/v1")
    );
    assert_eq!(
        provider.api_key,
        EffectiveOpenAiApiKeySource::File(PathBuf::from(
            "/home/alice/.config/merry/secrets/openai.key"
        ))
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
fn parses_inline_api_key_and_redacts_debug_output() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let config = MerryConfig::load_optional_from_text(
        Some(
            r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"

[providers.openai-compatible]
api_key = "sk-inline-secret"
"#,
        ),
        &paths,
    )
    .expect("config should parse")
    .expect("config should be present");
    let provider = config
        .openai_compatible_provider()
        .expect("provider should validate");

    assert_eq!(
        provider.resolve_api_key().expect("key should resolve"),
        "sk-inline-secret"
    );
    let debug = format!("{provider:?}");
    assert!(debug.contains("Inline(<redacted>)"));
    assert!(!debug.contains("sk-inline-secret"));
}

#[test]
fn default_level_service_tier_is_rejected_for_anthropic_providers() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let config = MerryConfig::load_optional_from_text(
        Some(
            r#"
[providers.default]
provider = "claude"
model = "claude-sonnet-test"
service_tier = "priority"

[providers.claude]
type = "anthropic"
api_key = "sk-ant-test"
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
    let config_text = |protocol: &str, tier_location: &str| {
        let (default_tier, provider_tier) = match tier_location {
            "default" => ("service_tier = \"ultrafast\"", ""),
            _ => ("", "service_tier = \"ultrafast\""),
        };
        format!(
            r#"
[providers.default]
provider = "compat"
model = "gpt-test"
{default_tier}

[providers.compat]
type = "openai-compatible"
protocol = "{protocol}"
{provider_tier}
api_key = "sk-test"
"#
        )
    };

    for tier_location in ["default", "named"] {
        let config = MerryConfig::load_optional_from_text(
            Some(&config_text("chat_completions", tier_location)),
            &paths,
        )
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

        let default = MerryConfig::load_optional_from_text(
            Some(&config_text("responses", tier_location)),
            &paths,
        )
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
    let openai = MerryConfig::load_optional_from_text(
        Some(
            r#"
[providers.default]
provider = "compat"
model = "gpt-test"

[providers.compat]
type = "openai-compatible"
protocol = "chat_completions"
api_key = "sk-test"
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

    let anthropic = MerryConfig::load_optional_from_text(
        Some(
            r#"
[providers.default]
provider = "claude"
model = "claude-sonnet-test"

[providers.claude]
type = "anthropic"
base_url = "https://anthropic.example.test"
api_version = "2023-06-01"
default_max_output_tokens = 2048
api_key = "sk-ant-test"
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
fn rejects_missing_or_ambiguous_api_key_sources() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let missing = MerryConfig::load_optional_from_text(
        Some(
            r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"

[providers.openai-compatible]
base_url = "https://api.example.test/v1"
"#,
        ),
        &paths,
    )
    .expect("TOML should parse")
    .expect("config should be present")
    .openai_compatible_provider()
    .expect_err("missing key source should fail");
    assert!(
        missing
            .to_string()
            .contains("exactly one of api_key or api_key_file")
    );

    let ambiguous = MerryConfig::load_optional_from_text(
        Some(
            r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"

[providers.openai-compatible]
api_key = "sk-inline-secret"
api_key_file = "secrets/openai.key"
"#,
        ),
        &paths,
    )
    .expect("TOML should parse")
    .expect("config should be present")
    .openai_compatible_provider()
    .expect_err("ambiguous key source should fail");
    assert!(
        ambiguous
            .to_string()
            .contains("must not set both api_key and api_key_file")
    );
}

#[test]
fn rejects_blank_or_control_character_inline_api_key() {
    let paths = XdgPaths::from_parts(home(), None, None);
    for api_key in ["  ", "sk-test\n"] {
        let error = MerryConfig::load_optional_from_text(
            Some(&format!(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"

[providers.openai-compatible]
api_key = {api_key:?}
"#
            )),
            &paths,
        )
        .expect("TOML should parse")
        .expect("config should be present")
        .openai_compatible_provider()
        .expect_err("invalid key should fail");

        assert!(error.to_string().contains("api_key"));
    }
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
        api_key: EffectiveOpenAiApiKeySource::File(PathBuf::from(
            "/home/alice/.config/merry/secrets/openai.key",
        )),
    };
    let debug = format!("{provider:?}");
    assert!(debug.contains("openai.key"));
    assert!(!debug.contains("sk-"));
}
