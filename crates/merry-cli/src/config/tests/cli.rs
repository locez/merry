use super::home;
use crate::coding::{AutoFallback, PermissionMode, SandboxMode};
use crate::config::{CliDefaults, ConfigError, MerryConfig, XdgPaths};

fn load(text: &str) -> MerryConfig {
    MerryConfig::load_optional_from_text(Some(text), &XdgPaths::from_parts(home(), None, None))
        .expect("config should parse")
        .expect("config should be present")
}

fn cli_defaults(text: &str) -> CliDefaults {
    load(text)
        .cli_defaults()
        .expect("cli defaults should resolve")
}

fn parse_error(text: &str) -> String {
    let paths = XdgPaths::from_parts(home(), None, None);
    match MerryConfig::load_optional_from_text(Some(text), &paths) {
        Ok(config) => panic!("{text:?} should fail to parse, loaded {config:?}"),
        Err(error @ ConfigError::Parse { .. }) => error.to_string(),
        Err(error) => panic!("{text:?} should be a parse error, got {error}"),
    }
}

#[test]
fn cli_defaults_to_unset_sandbox_and_permission_mode() {
    for text in ["", "[cli]\n"] {
        let defaults = cli_defaults(text);
        assert_eq!(defaults.sandbox(), None, "{text:?}");
        assert_eq!(defaults.permission_mode(), None, "{text:?}");
        assert_eq!(defaults.auto_fallback(), None, "{text:?}");
    }
}

#[test]
fn cli_sandbox_and_permission_mode_each_resolve_alone() {
    assert_eq!(
        cli_defaults("[cli]\nsandbox = \"workspace\"\n").sandbox(),
        Some(SandboxMode::Workspace)
    );
    assert_eq!(
        cli_defaults("[cli]\nsandbox = \"off\"\n").sandbox(),
        Some(SandboxMode::Off)
    );
    assert_eq!(
        cli_defaults("[cli]\npermission_mode = \"auto\"\n").permission_mode(),
        Some(PermissionMode::Auto)
    );
    assert_eq!(
        cli_defaults("[cli]\npermission_mode = \"ask\"\n").permission_mode(),
        Some(PermissionMode::Ask)
    );
    assert_eq!(
        cli_defaults("[cli]\npermission_mode = \"always-approve\"\n").permission_mode(),
        Some(PermissionMode::AlwaysApprove)
    );
    assert_eq!(
        cli_defaults("[cli]\npermission_mode = \"deny\"\n").permission_mode(),
        Some(PermissionMode::Deny)
    );
    assert_eq!(
        cli_defaults("[cli]\nauto_fallback = \"deny\"\n").auto_fallback(),
        Some(AutoFallback::Deny)
    );
}

#[test]
fn cli_accepts_auto_with_either_fallback() {
    for fallback in [AutoFallback::Ask, AutoFallback::Deny] {
        let name = match fallback {
            AutoFallback::Ask => "ask",
            AutoFallback::Deny => "deny",
        };
        let defaults = cli_defaults(&format!(
            "[cli]\npermission_mode = \"auto\"\nauto_fallback = \"{name}\"\n"
        ));
        assert_eq!(defaults.permission_mode(), Some(PermissionMode::Auto));
        assert_eq!(defaults.auto_fallback(), Some(fallback));
    }
}

#[test]
fn cli_rejects_auto_fallback_on_a_non_auto_mode() {
    let error = load("[cli]\npermission_mode = \"ask\"\nauto_fallback = \"deny\"\n")
        .cli_defaults()
        .expect_err("fallback does not apply to ask");
    assert!(error.to_string().contains("auto_fallback"), "{error}");
}

#[test]
fn cli_rejects_removed_sandbox_and_approval_names() {
    for text in [
        "[cli]\nsandbox = \"with-sandbox\"\n",
        "[cli]\nsandbox = \"no-sandbox\"\n",
        "[cli]\nsandbox = \"inner-sandbox\"\n",
        "[cli]\napproval_policy = \"model_then_human\"\n",
        "[cli]\npermission_mode = \"model_then_human\"\n",
        "[cli]\npermission_mode = \"human_only\"\n",
    ] {
        let message = parse_error(text);
        assert!(!message.is_empty(), "{text:?}: {message}");
    }
}

#[test]
fn cli_table_rejects_unknown_keys_and_wrong_types() {
    for text in [
        "[cli]\nsandbox = true\n",
        "[cli]\npermission_mode = true\n",
        "[cli]\nauto_fallback = [\"ask\"]\n",
    ] {
        let message = parse_error(text);
        assert!(!message.is_empty(), "{text:?}");
    }
}
