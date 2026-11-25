use super::*;
use crate::coding::{AutoFallback, PermissionMode, SandboxMode};
use crate::config::{BrandMarkMode, CopyButtonPosition, HeaderItemKind, TextSizingMode};
use merry_llm::{ReasoningEffort, ServiceTier};
use std::path::PathBuf;

#[test]
fn settings_defaults_keep_reasoning_effort_per_provider() {
    let paths = crate::config::XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
    let config = crate::config::MerryConfig::load_with_managed_from_text(
        Some(
            r#"
[providers.default]
provider = "compat"
model = "gpt-test"
reasoning_effort = "high"
"#,
        ),
        Some(
            r#"
version = 1

[providers.compat]
type = "openai-compatible"
reasoning_effort = "low"
api_key_file = "managed/secrets/compat.key"

[providers.alt]
type = "openai-compatible"
reasoning_effort = "max ultra"
api_key_file = "managed/secrets/alt.key"
"#,
        ),
        &paths,
    )
    .expect("config should parse")
    .expect("config should exist");

    let defaults =
        TuiSettingsDefaults::from_config(Some(&config)).expect("settings defaults should load");

    assert_eq!(
        defaults
            .reasoning_effort_for_provider(Some("compat"))
            .map(ReasoningEffort::as_str),
        Some("high")
    );
    assert_eq!(
        defaults
            .reasoning_effort_for_provider(Some("alt"))
            .map(ReasoningEffort::as_str),
        Some("max ultra")
    );
}

#[tokio::test]
async fn preferences_store_round_trips_tui_roles_and_permissions() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let store = TuiPreferencesStore::new(temp.path().join("merry/tui-preferences.toml"));
    let mut preferences = TuiPreferences {
        code_theme: CodeTheme::CatppuccinMocha,
        animations: Some(false),
        brand_mark: Some(BrandMarkMode::Snowflake),
        max_reading_width: Some(120),
        text_sizing: Some(TextSizingMode::Auto),
        header_items: Some(vec![HeaderItemKind::Model, HeaderItemKind::Tokens]),
        mouse_scroll_lines: Some(6),
        show_successful_command_output: Some(false),
        copy_button_position: Some(CopyButtonPosition::BottomLeft),
        keymap: BTreeMap::from([("toggle_plan".to_owned(), "ctrl+k".to_owned())]),
        provider: Some("anthropic".to_owned()),
        context_compaction_model: Some(RoleModelOverride {
            provider: Some("compat".to_owned()),
            model: Some("gpt-5-mini".to_owned()),
            reasoning_effort: None,
        }),
        approval_review_model: Some(RoleModelOverride {
            provider: Some("compat".to_owned()),
            model: Some("gpt-5-nano".to_owned()),
            reasoning_effort: None,
        }),
        subagent_default_model: Some(RoleModelOverride {
            provider: Some("anthropic".to_owned()),
            model: Some("claude-sonnet".to_owned()),
            reasoning_effort: Some(ReasoningEffort::new("medium").expect("valid effort")),
        }),
        subagent_roles: BTreeMap::from([(
            "explore".to_owned(),
            SubagentRolePreferenceOverride {
                enabled: Some(true),
                provider: Some("compat".to_owned()),
                model: Some("gpt-5-mini".to_owned()),
                reasoning_effort: Some(ReasoningEffort::new("low").expect("valid effort")),
                max_model_turns: Some(12),
                timeout_secs: Some(180),
            },
        )]),
        context_window_tokens: Some(272_000),
        retained_model_turns: Some(4),
        subagents_enabled: Some(true),
        subagent_max_threads: Some(6),
        subagent_limit_behavior: Some(SubagentLimitBehaviorPref::Fail),
        max_parallel_tool_calls: Some(8),
        permissions: TuiPermissionPreferences {
            sandbox: Some(SandboxMode::Workspace),
            permission_mode: Some(PermissionMode::Auto),
            auto_fallback: Some(AutoFallback::Deny),
            network: Some(false),
            ssh_agent: Some(true),
            gpg_agent: Some(false),
            dbus: Some(false),
        },
        ..TuiPreferences::default()
    };
    preferences
        .set_model_for_provider("anthropic", Some("claude-test"))
        .expect("valid model preference");
    preferences
        .set_reasoning_effort_for_provider("anthropic", Some(ReasoningEffort::new("high").unwrap()))
        .expect("valid reasoning preference");
    preferences
        .set_service_tier_for_provider("anthropic", Some(ServiceTier::Fast))
        .expect("valid service tier preference");

    store.save(&preferences).await.expect("preferences save");
    let loaded = store.load().await.expect("preferences load");

    assert_eq!(loaded, preferences);
    assert!(store.path().is_file());
}

#[tokio::test]
async fn migrates_legacy_model_into_the_selected_provider_history() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let path = temp.path().join("merry/tui-preferences.toml");
    tokio::fs::create_dir_all(path.parent().expect("preferences parent"))
        .await
        .expect("preferences parent");
    tokio::fs::write(
        &path,
        r#"
version = 1
provider = "opencode"
model = "deepseek-v4-pro"
"#,
    )
    .await
    .expect("v1 preferences");
    let store = TuiPreferencesStore::new(path);

    let migrated = store
        .load_with_default_provider(Some("anthropic"))
        .await
        .expect("v1 preferences should migrate");

    assert_eq!(migrated.provider.as_deref(), Some("opencode"));
    assert_eq!(
        migrated.model_for_provider("opencode"),
        Some("deepseek-v4-pro")
    );
    assert_eq!(migrated.model_for_provider("anthropic"), None);
}

#[tokio::test]
async fn migrates_version_two_reasoning_only_to_its_selected_provider() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let path = temp.path().join("merry/tui-preferences.toml");
    tokio::fs::create_dir_all(path.parent().expect("preferences parent"))
        .await
        .expect("preferences parent");
    tokio::fs::write(
        &path,
        r#"
version = 2
provider = "opencode"
models = { opencode = "model-a", anthropic = "model-b" }
reasoning_effort = "max ultra"
"#,
    )
    .await
    .expect("v2 preferences");
    let store = TuiPreferencesStore::new(path);

    let migrated = store
        .load_with_default_provider(Some("anthropic"))
        .await
        .expect("v2 preferences should migrate");

    assert_eq!(migrated.model_for_provider("opencode"), Some("model-a"));
    assert_eq!(migrated.model_for_provider("anthropic"), Some("model-b"));
    assert_eq!(
        migrated
            .reasoning_effort_for_provider("opencode")
            .map(ReasoningEffort::as_str),
        Some("max ultra")
    );
    assert_eq!(migrated.reasoning_effort_for_provider("anthropic"), None);
}

#[test]
fn clear_provider_state_cleans_dangling_role_bindings() {
    let mut preferences = TuiPreferences {
        provider: Some("old".to_owned()),
        context_compaction_model: Some(RoleModelOverride {
            provider: Some("old".to_owned()),
            model: Some("gpt-old".to_owned()),
            reasoning_effort: None,
        }),
        subagent_roles: BTreeMap::from([(
            "explore".to_owned(),
            SubagentRolePreferenceOverride {
                enabled: Some(true),
                provider: Some("old".to_owned()),
                model: Some("gpt-old".to_owned()),
                reasoning_effort: None,
                max_model_turns: Some(10),
                timeout_secs: None,
            },
        )]),
        ..TuiPreferences::default()
    };
    preferences
        .set_model_for_provider("old", Some("gpt-old"))
        .expect("valid");

    preferences
        .clear_provider_state("old")
        .expect("clear should succeed");

    assert_eq!(preferences.model_for_provider("old"), None);
    assert_eq!(preferences.context_compaction_model, None);
    let explore = preferences
        .subagent_roles
        .get("explore")
        .expect("non-provider overrides survive");
    assert_eq!(explore.provider, None);
    assert_eq!(explore.model, None);
    assert_eq!(explore.max_model_turns, Some(10));
}

#[test]
fn provider_state_keeps_one_model_and_reasoning_pair() {
    let mut preferences = TuiPreferences::default();
    preferences
        .set_model_and_reasoning_for_provider(
            "opencode",
            "model-a",
            ReasoningEffort::new("high").expect("valid effort"),
        )
        .expect("valid provider state");
    preferences
        .set_model_and_reasoning_for_provider(
            "opencode",
            "model-b",
            ReasoningEffort::new("max ultra").expect("valid effort"),
        )
        .expect("valid provider state");

    assert_eq!(preferences.model_for_provider("opencode"), Some("model-b"));
    assert_eq!(
        preferences
            .reasoning_effort_for_provider("opencode")
            .map(ReasoningEffort::as_str),
        Some("max ultra")
    );
}
