use super::home;
use crate::config::provider::{EffectiveOpenAiApiKeySource, ProviderConfigSource};
use crate::config::{EffectiveProviderConfig, LogFormat, LogLevel, MerryConfig, XdgPaths};
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
api_key_file = "managed/secrets/opencode.key"
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
    assert_eq!(profile.source(), ProviderConfigSource::Managed);
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
fn rejects_user_and_managed_alias_collision() {
    let temp = tempfile::tempdir().expect("temp dir");
    let paths = XdgPaths::from_parts(
        temp.path().join("home"),
        Some(temp.path().join("config")),
        Some(temp.path().join("state")),
    );
    fs::create_dir_all(paths.managed_config_dir()).expect("managed config dir");
    fs::write(
        paths.config_file(),
        r#"
[providers.default]
provider = "opencode"
model = "model-a"

[providers.opencode]
api_key = "sk-user"
"#,
    )
    .expect("user config");
    fs::write(
        paths.managed_providers_file(),
        r#"
version = 1

[providers.opencode]
display_name = "Managed OpenCode"
default_model = "model-b"
type = "openai-compatible"
api_key = "sk-managed"
"#,
    )
    .expect("managed config");

    let error = MerryConfig::load_optional(&paths).expect_err("collision should fail");

    assert!(error.to_string().contains("opencode"));
    assert!(error.to_string().contains("both user and managed"));
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
submit_next = "enter"
submit_backlog = "ctrl+b"
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
resume_suspended = "ctrl+n"
discard_suspended = "ctrl+d"
"##,
        ),
        &paths,
    )
    .expect("config should parse")
    .expect("config should be present");

    let tui = config.tui_config().expect("tui config should validate");
    assert_eq!(tui.theme.status.as_deref(), Some("cyan"));
    assert_eq!(tui.theme.assistant.as_deref(), Some("white"));
    assert_eq!(tui.theme.tool_keyword.as_deref(), Some("light_cyan"));
    assert_eq!(tui.theme.command.as_deref(), Some("light_blue"));
    assert_eq!(tui.keymap.submit_next.as_deref(), Some("enter"));
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
    assert_eq!(tui.theme.status.as_deref(), Some("light_magenta"));
    assert_eq!(tui.theme.assistant.as_deref(), Some("white"));
    assert_eq!(tui.theme.tool_keyword.as_deref(), Some("light_cyan"));
    assert_eq!(tui.theme.command.as_deref(), Some("light_blue"));
    assert_eq!(tui.keymap.submit_next.as_deref(), Some("enter"));
    assert_eq!(tui.keymap.cancel_input_or_quit.as_deref(), Some("ctrl+c"));
    assert_eq!(tui.keymap.insert_newline.as_deref(), Some("ctrl+j"));
    assert_eq!(tui.keymap.scroll_up.as_deref(), Some("pageup"));
    assert_eq!(
        tui.keymap.open_session_in_browser.as_deref(),
        Some("ctrl+g")
    );
    assert_eq!(
        tui.keymap.review_previous_user_input.as_deref(),
        Some("ctrl+u")
    );
    assert_eq!(tui.keymap.history_previous.as_deref(), Some("up"));

    let provider = config
        .openai_compatible_provider()
        .expect("example provider should validate");
    assert_eq!(provider.model.as_deref(), Some("gpt-4.1-mini"));
    assert_eq!(
        provider
            .reasoning_effort
            .as_ref()
            .map(|effort| effort.as_str()),
        None
    );
    assert_eq!(
        provider.base_url.as_deref(),
        Some("https://api.openai.com/v1")
    );
    assert_eq!(
        provider.api_key,
        EffectiveOpenAiApiKeySource::File(PathBuf::from(
            "/home/alice/.config/merry/secrets/openai.key"
        ))
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
    assert_eq!(trusted_path_rules.len(), 5);
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
    assert_eq!(trusted_path_rules[4].path(), Path::new("/home/alice/.ssh"));
    assert_eq!(trusted_path_rules[4].access(), PathAccess::Deny);

    let models = config
        .runtime_models()
        .expect("example runtime model roles should validate");
    let context_compaction = models
        .context_compaction
        .expect("example should configure context compaction model role");
    assert_eq!(context_compaction.provider, "openai-compatible");
    assert_eq!(context_compaction.model, "gpt-4.1-mini");
    let approval_review = models
        .approval_review
        .expect("example should configure approval review model role");
    assert_eq!(approval_review.provider, "openai-compatible");
    assert_eq!(approval_review.model, "gpt-4.1-mini");
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
