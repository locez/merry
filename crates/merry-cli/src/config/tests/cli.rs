use super::home;
use crate::coding::{ApprovalPolicy, ProcessExecutionMode};
use crate::config::{CliDefaults, ConfigError, MerryConfig, XdgPaths};

fn load(text: &str) -> MerryConfig {
    MerryConfig::load_optional_from_text(Some(text), &XdgPaths::from_parts(home(), None, None))
        .expect("config should parse")
        .expect("config should be present")
}

fn cli_defaults(text: &str) -> CliDefaults {
    load(text)
        .cli_defaults()
        .unwrap_or_else(|error| panic!("{text:?} [cli] defaults should validate: {error}"))
}

fn parse_error(text: &str) -> String {
    let paths = XdgPaths::from_parts(home(), None, None);
    match MerryConfig::load_optional_from_text(Some(text), &paths) {
        Ok(config) => panic!("{text:?} should fail to parse, loaded {config:?}"),
        Err(error @ ConfigError::Parse { .. }) => error.to_string(),
        Err(error) => panic!("{text:?} should be a parse error, got {error}"),
    }
}

fn invalid_error(text: &str) -> String {
    match load(text).cli_defaults() {
        Ok(defaults) => panic!("{text:?} should be rejected, resolved {defaults:?}"),
        Err(error @ ConfigError::Invalid(_)) => error.to_string(),
        Err(error) => panic!("{text:?} should be an invalid-config error, got {error}"),
    }
}

#[test]
fn cli_defaults_to_no_sandbox_mode_and_no_approval_policy() {
    for text in ["", "[cli]\n"] {
        let defaults = cli_defaults(text);
        assert_eq!(defaults, CliDefaults::default(), "{text:?}");
        assert_eq!(defaults.process_execution_mode(), None, "{text:?}");
        assert_eq!(defaults.approval_policy(), None, "{text:?}");
    }
}

#[test]
fn cli_sandbox_and_approval_policy_map_to_modes() {
    for (text, mode, policy) in [
        (
            "[cli]\nsandbox = \"no-sandbox\"\n",
            Some(ProcessExecutionMode::Unrestricted),
            None,
        ),
        (
            "[cli]\nsandbox = \"with-sandbox\"\n",
            Some(ProcessExecutionMode::OuterAndInner),
            None,
        ),
        (
            "[cli]\nsandbox = \"no-sandbox\"\napproval_policy = \"none\"\n",
            Some(ProcessExecutionMode::Unrestricted),
            Some(ApprovalPolicy::NoApproval),
        ),
        (
            "[cli]\napproval_policy = \"deny\"\n",
            None,
            Some(ApprovalPolicy::Deny),
        ),
        (
            "[cli]\nsandbox = \"with-sandbox\"\napproval_policy = \"model_only\"\n",
            Some(ProcessExecutionMode::OuterAndInner),
            Some(ApprovalPolicy::ModelOnly),
        ),
        (
            "[cli]\nsandbox = \"inner-sandbox\"\napproval_policy = \"model_then_human\"\n",
            Some(ProcessExecutionMode::InnerOnly),
            Some(ApprovalPolicy::ModelThenHuman),
        ),
        (
            "[cli]\napproval_policy = \"human_only\"\n",
            None,
            Some(ApprovalPolicy::HumanOnly),
        ),
    ] {
        let defaults = cli_defaults(text);
        assert_eq!(defaults.process_execution_mode(), mode, "{text:?}");
        assert_eq!(defaults.approval_policy(), policy, "{text:?}");
    }
}

#[test]
fn cli_none_approval_policy_requires_no_sandbox() {
    for (text, found) in [
        ("[cli]\napproval_policy = \"none\"\n", "sandbox is not set"),
        (
            "[cli]\nsandbox = \"with-sandbox\"\napproval_policy = \"none\"\n",
            "sandbox = \"with-sandbox\"",
        ),
        (
            "[cli]\nsandbox = \"inner-sandbox\"\napproval_policy = \"none\"\n",
            "sandbox = \"inner-sandbox\"",
        ),
    ] {
        let message = invalid_error(text);
        assert!(
            message.starts_with("Merry config is invalid: "),
            "{message}"
        );
        assert!(
            message.contains("[cli] approval_policy = \"none\" runs without any sandbox"),
            "{message}"
        );
        assert!(
            message.contains("requires sandbox = \"no-sandbox\""),
            "{message}"
        );
        assert!(message.ends_with(found), "{message}");
    }
}

#[test]
fn cli_sandbox_rejects_raw_flags_and_unknown_names() {
    let message = parse_error("[cli]\nsandbox = \"--no-sandbox\"\n");
    assert!(
        message.contains("unknown variant `--no-sandbox`"),
        "{message}"
    );
    assert!(
        message.contains("`with-sandbox`, `no-sandbox`, `inner-sandbox`"),
        "{message}"
    );

    let message = parse_error("[cli]\nsandbox = \"none\"\n");
    assert!(message.contains("unknown variant `none`"), "{message}");
}

#[test]
fn cli_approval_policy_rejects_old_and_unknown_names() {
    for value in [
        "auto",
        "trusted",
        "model",
        "human",
        "model-then-human",
        "fully-trusted",
        "--approval-policy",
        "yes",
    ] {
        let message = parse_error(&format!("[cli]\napproval_policy = \"{value}\"\n"));
        assert!(
            message.contains(&format!("unknown variant `{value}`")),
            "{value}: {message}"
        );
        assert!(
            message.contains("`none`, `deny`, `model_only`, `model_then_human`, `human_only`"),
            "{value}: {message}"
        );
    }
}

#[test]
fn cli_table_rejects_unknown_keys_and_wrong_types() {
    for text in [
        "[cli]\ndefault_options = [\"no-sandbox\"]\n",
        "[cli]\nsandbox_mode = \"no-sandbox\"\n",
        "[cli]\nfully_trusted = true\n",
        "[cli]\nsandbox = [\"no-sandbox\"]\n",
        "[cli]\nsandbox = true\n",
        "[cli]\napproval_policy = true\n",
        "[cli]\napproval_policy = [\"none\"]\n",
    ] {
        let message = parse_error(text);
        assert!(!message.is_empty(), "{text:?}");
    }
}
