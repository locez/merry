use super::home;
use crate::coding::{AutoFallback, PermissionMode};
use crate::config::{
    BrandMarkMode, CliDefaults, CopyButtonPosition, EffectiveProviderConfig, LogFormat, LogLevel,
    MerryConfig, XdgPaths,
};
use merry_runtime::PathAccess;
use std::{
    fs,
    path::{Path, PathBuf},
};

#[test]
fn missing_config_is_allowed_for_commands_without_provider_requirement() {
    let loaded =
        MerryConfig::load_optional_from_text(None, &XdgPaths::from_parts(home(), None, None))
            .expect("missing config should not fail optional load");
    assert!(loaded.is_none());
}

#[test]
fn loads_managed_provider_without_user_default_provider() {
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
    .expect("managed providers file");

    let config = MerryConfig::load_optional(&paths)
        .expect("managed config should load")
        .expect("managed provider should create config");
    let profile = config
        .provider_profile("opencode")
        .expect("managed profile should resolve");

    assert_eq!(profile.display_name(), "OpenCode");
    let EffectiveProviderConfig::OpenAiCompatible(provider) = config
        .provider_by_alias("opencode")
        .expect("managed provider should materialize")
    else {
        panic!("OpenAI-compatible provider expected");
    };
    assert_eq!(
        provider.resolve_api_key().expect("key should resolve"),
        "sk-test"
    );
}

#[test]
fn parses_mcp_http_servers() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let config = MerryConfig::load_optional_from_text(
        Some(
            r#"
[mcp.context7]
url = "https://mcp.example.test/mcp"
headers = { Authorization = "Bearer test-token" }
tools = ["resolve-library-id", "get-library-docs"]
"#,
        ),
        &paths,
    )
    .expect("config should parse")
    .expect("config should be present");

    let servers = config
        .mcp_servers()
        .expect("MCP server config should validate");
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].id(), "context7");
    assert_eq!(servers[0].url(), "https://mcp.example.test/mcp");
    assert_eq!(
        servers[0].headers(),
        &[("Authorization".to_owned(), "Bearer test-token".to_owned())]
    );
    assert_eq!(
        servers[0].tools().expect("tools allowlist should parse"),
        &[
            "resolve-library-id".to_owned(),
            "get-library-docs".to_owned()
        ]
    );
}

#[test]
fn parses_observability_config() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let config = MerryConfig::load_optional_from_text(
        Some(
            r#"
[global]
profile = "default"

[observability.log]
enabled = true
level = "debug"
format = "json"
"#,
        ),
        &paths,
    )
    .expect("config should parse")
    .expect("config should be present");

    let log = config
        .effective_log_settings(&paths)
        .expect("log settings should validate")
        .expect("logging should be enabled");
    assert_eq!(log.level, LogLevel::Debug);
    assert_eq!(log.format, LogFormat::Json);
    assert_eq!(log.path, paths.default_log_file());
}

#[test]
fn parses_tui_theme_and_keymap_config() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let config = MerryConfig::load_optional_from_text(
        Some(
            r##"
[tui.theme]
status = "cyan"
muted = "dark_gray"
focus = "yellow"
assistant = "white"
selection = "blue"
tool_keyword = "light_cyan"
command = "light_blue"
diff_add = "green"
diff_delete = "red"
warning = "yellow"
error = "red"
risk = "magenta"
success = "green"

[tui.keymap]
submit = "enter"
cancel_input_or_quit = "ctrl+c"
insert_newline = "ctrl+j"
paste_image = "ctrl+v"
interrupt = "esc"
quit = "ctrl+q"
scroll_up = "pageup"
scroll_down = "pagedown"
review_previous_user_input = "ctrl+u"
open_session_in_browser = "ctrl+g"
history_previous = "up"
history_next = "down"
"##,
        ),
        &paths,
    )
    .expect("config should parse")
    .expect("config should be present");

    let tui = config.tui_config().expect("tui config should validate");
    assert!(tui.show_successful_command_output);
    assert_eq!(tui.theme.status.as_deref(), Some("cyan"));
    assert_eq!(tui.theme.assistant.as_deref(), Some("white"));
    assert_eq!(tui.theme.tool_keyword.as_deref(), Some("light_cyan"));
    assert_eq!(tui.theme.command.as_deref(), Some("light_blue"));
    assert_eq!(tui.keymap.submit.as_deref(), Some("enter"));
    assert_eq!(tui.keymap.cancel_input_or_quit.as_deref(), Some("ctrl+c"));
    assert_eq!(tui.keymap.insert_newline.as_deref(), Some("ctrl+j"));
    assert_eq!(tui.keymap.paste_image.as_deref(), Some("ctrl+v"));
    assert_eq!(
        tui.keymap.open_session_in_browser.as_deref(),
        Some("ctrl+g")
    );
    assert_eq!(tui.keymap.scroll_up.as_deref(), Some("pageup"));
    assert_eq!(
        tui.keymap.review_previous_user_input.as_deref(),
        Some("ctrl+u")
    );
    assert_eq!(tui.keymap.history_previous.as_deref(), Some("up"));
}

#[test]
fn rejects_selection_copy_keymap_config() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let error = MerryConfig::load_optional_from_text(
        Some("[tui.keymap]\ncopy_selection = 'ctrl+shift+c'"),
        &paths,
    )
    .expect_err("selection copy has no configurable shortcut");
    assert!(error.to_string().contains("unknown field `copy_selection`"));
}

#[test]
fn successful_command_output_defaults_to_visible() {
    let paths = XdgPaths::from_parts(home(), None, None);
    for (text, expected) in [
        ("[global]\nprofile = 'default'", true),
        ("[tui]", true),
        ("[tui]\nshow_successful_command_output = false", false),
        ("[tui]\nshow_successful_command_output = true", true),
    ] {
        let config = MerryConfig::load_optional_from_text(Some(text), &paths)
            .expect("config should parse")
            .expect("config should be present");
        assert_eq!(
            config.tui_config().unwrap().show_successful_command_output,
            expected
        );
    }
    assert!(
        MerryConfig::load_optional_from_text(
            Some("[tui]\nshow_successful_command_output = 'true'"),
            &paths,
        )
        .is_err()
    );
}

#[test]
fn parses_skill_config_roots() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let config = MerryConfig::load_optional_from_text(
        Some(
            r#"
[skills]
enabled = true
roots = ["skills", "~/shared-skills", "/opt/company/skills"]
"#,
        ),
        &paths,
    )
    .expect("config should parse")
    .expect("config should be present");

    let skills = config.skill_roots().expect("skill roots should resolve");
    assert_eq!(
        skills,
        vec![
            PathBuf::from("/home/alice/.config/merry/skills"),
            PathBuf::from("/home/alice/shared-skills"),
            PathBuf::from("/opt/company/skills"),
        ]
    );
}

#[test]
fn missing_skills_config_uses_default_user_skill_root() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let missing = MerryConfig::load_optional_from_text(Some(""), &paths)
        .expect("config should parse")
        .expect("config should be present");
    assert_eq!(
        missing.skill_roots().expect("missing skills is valid"),
        vec![PathBuf::from("/home/alice/.config/merry/skills")]
    );
}

#[test]
fn disabled_skills_return_no_roots() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let disabled = MerryConfig::load_optional_from_text(
        Some("[skills]\nenabled = false\nroots = [\"skills\"]\n"),
        &paths,
    )
    .expect("config should parse")
    .expect("config should be present");
    assert_eq!(
        disabled.skill_roots().expect("disabled skills is valid"),
        Vec::<PathBuf>::new()
    );
}

#[test]
fn example_config_toml_matches_current_schema_and_resolves_user_defaults() {
    let example = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/config.toml"
    ));
    let paths = XdgPaths::from_parts(home(), None, None);
    let config = MerryConfig::load_optional_from_text(Some(example), &paths)
        .expect("example config should parse")
        .expect("example config should be present");

    assert_eq!(config.profile(), Some("default"));
    assert_eq!(
        config.cli_defaults().expect("example cli defaults"),
        CliDefaults::new(None, Some(PermissionMode::Auto), Some(AutoFallback::Ask),),
        "the user-facing example should not preselect a sandbox mode and should \
         spell out auto review with a human fallback"
    );
    assert!(
        config
            .effective_log_settings(&paths)
            .expect("example log settings should validate")
            .is_none(),
        "the user-facing example should not enable persistent logging by default"
    );

    let tui = config
        .tui_config()
        .expect("example TUI config should validate");
    assert!(tui.show_successful_command_output);
    assert_eq!(tui.brand_mark, BrandMarkMode::Emoji);
    assert_eq!(tui.copy_button_position, CopyButtonPosition::TopRight);
    assert_eq!(
        tui.theme.preset,
        Some(crate::tui::theme::ThemePreset::Sakura)
    );
    assert_eq!(tui.theme.transparent, Some(false));
    assert_eq!(tui.theme.status, None);
    assert_eq!(tui.theme.assistant, None);
    assert_eq!(tui.theme.tool_keyword, None);
    assert_eq!(tui.theme.command, None);
    assert_eq!(tui.keymap.submit.as_deref(), Some("enter"));
    assert_eq!(tui.keymap.cancel_input_or_quit.as_deref(), Some("ctrl+c"));
    assert_eq!(tui.keymap.insert_newline.as_deref(), Some("ctrl+j"));
    assert_eq!(tui.keymap.scroll_up.as_deref(), Some("pageup"));
    assert_eq!(tui.keymap.open_command_details.as_deref(), Some("alt+t"));
    assert_eq!(tui.keymap.open_session_in_browser.as_deref(), Some("alt+g"));
    assert_eq!(
        tui.keymap.review_previous_user_input.as_deref(),
        Some("alt+u")
    );
    assert_eq!(tui.keymap.history_previous.as_deref(), Some("up"));

    let snowflake =
        MerryConfig::load_optional_from_text(Some("[tui]\nbrand_mark = 'snowflake'"), &paths)
            .expect("snowflake brand_mark should parse")
            .expect("config should be present");
    assert_eq!(
        snowflake.tui_config().expect("valid tui config").brand_mark,
        BrandMarkMode::Snowflake
    );

    for (raw, expected) in [
        ("top-left", CopyButtonPosition::TopLeft),
        ("top-right", CopyButtonPosition::TopRight),
        ("bottom-left", CopyButtonPosition::BottomLeft),
        ("bottom-right", CopyButtonPosition::BottomRight),
        ("bottom_left", CopyButtonPosition::BottomLeft),
    ] {
        let parsed = MerryConfig::load_optional_from_text(
            Some(&format!("[tui]\ncopy_button_position = '{raw}'")),
            &paths,
        )
        .expect("copy_button_position should parse")
        .expect("config should be present");
        assert_eq!(
            parsed
                .tui_config()
                .expect("valid tui config")
                .copy_button_position,
            expected
        );
    }
    assert!(
        MerryConfig::load_optional_from_text(
            Some("[tui]\ncopy_button_position = 'center'"),
            &paths,
        )
        .is_err(),
        "unknown copy_button_position should fail validation"
    );

    let preset = MerryConfig::load_optional_from_text(
        Some("[tui.theme]\npreset = 'tokyo-night'\ntransparent = true\nheader_bg = '#112233'"),
        &paths,
    )
    .expect("named preset should parse")
    .expect("config should be present");
    let preset_tui = preset.tui_config().expect("valid preset config");
    assert_eq!(
        preset_tui.theme.preset,
        Some(crate::tui::theme::ThemePreset::TokyoNight)
    );
    assert_eq!(preset_tui.theme.transparent, Some(true));
    assert_eq!(preset_tui.theme.header_bg.as_deref(), Some("#112233"));

    assert!(
        MerryConfig::load_optional_from_text(
            Some("[tui.theme]\npreset = 'nonexistent-theme'"),
            &paths,
        )
        .is_err(),
        "unknown theme preset should be rejected"
    );
    assert_eq!(tui.keymap.history_search.as_deref(), Some("ctrl+r"));
    assert_eq!(tui.keymap.toggle_comment.as_deref(), Some("alt+c"));

    assert!(
        config
            .configured_default_provider()
            .expect("example config without credentials should validate")
            .is_none(),
        "example config.toml must not define inline or file-based provider credentials"
    );
    let retry = config
        .provider_retry_policy()
        .expect("example retry policy should validate")
        .expect("example retry policy should be configured");
    assert!(retry.enabled());
    assert_eq!(retry.max_attempts(), 6);
    assert_eq!(retry.max_delay(), std::time::Duration::from_secs(120));
    assert_eq!(retry.max_elapsed(), std::time::Duration::from_secs(300));
    assert!(retry.jitter());
    let auto_compaction = config
        .automatic_compaction_config()
        .expect("example auto compaction config should validate");
    assert!(auto_compaction.is_enabled());
    let policy = auto_compaction.policy();
    assert_eq!(policy.target_output_tokens(), None);
    assert_eq!(policy.max_accepted_output_bytes(), None);
    assert_eq!(policy.retained_model_turns(), 5);
    assert_eq!(
        config
            .skill_roots()
            .expect("example skill roots should validate"),
        vec![PathBuf::from("/home/alice/.config/merry/skills")]
    );
    let trusted_path_rules = config
        .trusted_global_path_rules()
        .expect("example trusted path rules should validate");
    assert_eq!(trusted_path_rules.len(), 4);
    assert_eq!(trusted_path_rules[0].path(), Path::new("/etc"));
    assert_eq!(trusted_path_rules[0].access(), PathAccess::ReadOnly);
    assert_eq!(trusted_path_rules[1].path(), Path::new("/var/log"));
    assert_eq!(trusted_path_rules[1].access(), PathAccess::ReadOnly);
    assert_eq!(
        trusted_path_rules[2].path(),
        Path::new("/home/alice/.config/merry/company-readonly")
    );
    assert_eq!(trusted_path_rules[2].access(), PathAccess::ReadOnly);
    assert_eq!(
        trusted_path_rules[3].path(),
        Path::new("/home/alice/.config/merry/company-work")
    );
    assert_eq!(trusted_path_rules[3].access(), PathAccess::ReadWrite);

    let models = config
        .runtime_models()
        .expect("example runtime model roles should validate");
    assert!(models.context_compaction.is_none());
    assert!(models.approval_review.is_none());

    let subagents = config
        .subagents_config()
        .expect("example [subagents] config should validate");
    assert!(subagents.is_enabled());
    assert_eq!(subagents.limits().max_threads(), 6);
    assert_eq!(subagents.limits().max_depth(), 1);
    assert_eq!(subagents.limits().foreground_wait_budget_secs(), 120);
    assert_eq!(
        subagents.limits().limit_behavior(),
        merry_runtime::SubagentLimitBehavior::Queue
    );
    assert_eq!(subagents.roles()["explore"].max_model_turns, Some(30));
    assert_eq!(subagents.roles()["coder"].max_model_turns, Some(80));
    assert_eq!(subagents.roles()["review"].max_model_turns, Some(40));
}

#[test]
fn disabled_logging_has_no_effective_log_settings() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let config = MerryConfig::load_optional_from_text(
        Some(
            r#"
[observability.log]
enabled = false
level = "debug"
format = "json"
"#,
        ),
        &paths,
    )
    .expect("config should parse")
    .expect("config should be present");

    assert!(
        config
            .effective_log_settings(&paths)
            .expect("settings should validate")
            .is_none()
    );
}

#[test]
fn rejects_invalid_log_level_format_and_relative_log_path() {
    let paths = XdgPaths::from_parts(home(), None, None);

    let invalid_level = MerryConfig::load_optional_from_text(
        Some("[observability.log]\nenabled = true\nlevel = \"verbose\"\nformat = \"json\"\n"),
        &paths,
    )
    .expect_err("invalid level should fail");
    assert!(invalid_level.to_string().contains("level"));

    let invalid_format = MerryConfig::load_optional_from_text(
        Some("[observability.log]\nenabled = true\nlevel = \"info\"\nformat = \"yaml\"\n"),
        &paths,
    )
    .expect_err("invalid format should fail");
    assert!(invalid_format.to_string().contains("format"));

    let relative_path = MerryConfig::load_optional_from_text(
            Some(
                "[observability.log]\nenabled = true\nlevel = \"info\"\nformat = \"json\"\npath = \"logs/merry.jsonl\"\n",
            ),
            &paths,
        )
        .expect("TOML should parse")
        .expect("config should be present")
        .effective_log_settings(&paths)
        .expect_err("relative log path should fail");
    assert!(relative_path.to_string().contains("absolute"));
}

#[test]
fn relocates_legacy_managed_provider_directory_and_resolves_legacy_secret_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = XdgPaths::from_parts(
        temp.path().join("home"),
        Some(temp.path().join("config")),
        Some(temp.path().join("state")),
    )
    .with_data_home(temp.path().join("data"));

    let legacy_dir = paths.config_dir().join("managed");
    let legacy_secrets = legacy_dir.join("secrets");
    std::fs::create_dir_all(&legacy_secrets).expect("create legacy secrets dir");
    std::fs::write(
        legacy_secrets.join("deepseek-1-0.key"),
        "sk-legacy-secret\n",
    )
    .expect("write legacy secret");
    std::fs::write(
        legacy_dir.join("providers.toml"),
        r#"
version = 1

[providers.deepseek]
display_name = "DeepSeek"
default_model = "deepseek-chat"
type = "openai-compatible"
protocol = "chat_completions"
base_url = "https://api.deepseek.com/v1"
api_key_file = "managed/secrets/deepseek-1-0.key"
"#,
    )
    .expect("write legacy registry");

    let config = MerryConfig::load_optional(&paths)
        .expect("config should relocate and load")
        .expect("relocated registry should be loaded");

    assert!(
        !legacy_dir.exists(),
        "legacy managed directory should be moved out of writable config_dir"
    );
    assert!(paths.managed_providers_file().is_file());
    let expected_secret = paths.managed_secrets_dir().join("deepseek-1-0.key");
    assert_eq!(
        std::fs::read_to_string(&expected_secret).expect("moved secret should be readable"),
        "sk-legacy-secret\n"
    );

    let provider = config
        .provider_by_alias("deepseek")
        .expect("relocated provider should resolve");
    match provider {
        crate::config::EffectiveProviderConfig::OpenAiCompatible(openai) => {
            assert_eq!(openai.api_key_file, expected_secret);
            assert_eq!(
                openai.resolve_api_key().expect("key should read"),
                "sk-legacy-secret"
            );
        }
        other => panic!("expected openai-compatible provider, got {other:?}"),
    }
    assert_eq!(
        config
            .private_process_paths()
            .expect("private process paths should resolve"),
        vec![paths.state_dir().to_path_buf(), paths.managed_config_dir(),],
        "relocated managed/secrets/... path should stay covered by providers_dir"
    );
}

#[test]
fn does_not_overwrite_existing_providers_dir_when_legacy_managed_dir_also_exists() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = XdgPaths::from_parts(
        temp.path().join("home"),
        Some(temp.path().join("config")),
        Some(temp.path().join("state")),
    )
    .with_data_home(temp.path().join("data"));

    let legacy_dir = paths.config_dir().join("managed");
    std::fs::create_dir_all(&legacy_dir).expect("create legacy dir");
    std::fs::write(
        legacy_dir.join("providers.toml"),
        "version = 1\n[providers.legacy]\ntype = \"openai-compatible\"\napi_key_file = \"managed/secrets/old.key\"\n",
    )
    .expect("write legacy registry");

    std::fs::create_dir_all(paths.managed_secrets_dir()).expect("create new providers dir");
    std::fs::write(
        paths.managed_providers_file(),
        "version = 1\n[providers.current]\ntype = \"openai-compatible\"\napi_key_file = \"secrets/new.key\"\n",
    )
    .expect("write new registry");

    let config = MerryConfig::load_optional(&paths)
        .expect("config should load existing providers dir")
        .expect("config should be present");

    assert_eq!(config.provider_aliases(), vec!["current".to_owned()]);
    assert!(
        legacy_dir.exists(),
        "legacy directory should be left untouched when target already exists"
    );
}
