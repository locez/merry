use super::home;
use crate::coding::{ApprovalPolicy, ProcessExecutionMode};
use crate::config::{CliDefaults, ConfigError, MerryConfig, XdgPaths};

const SANDBOXES: [(&str, ProcessExecutionMode); 3] = [
    ("with-sandbox", ProcessExecutionMode::OuterAndInner),
    ("no-sandbox", ProcessExecutionMode::Unrestricted),
    ("inner-sandbox", ProcessExecutionMode::InnerOnly),
];

const POLICIES: [(&str, ApprovalPolicy); 5] = [
    ("none", ApprovalPolicy::NoApproval),
    ("deny", ApprovalPolicy::Deny),
    ("model_only", ApprovalPolicy::ModelOnly),
    ("model_then_human", ApprovalPolicy::ModelThenHuman),
    ("human_only", ApprovalPolicy::HumanOnly),
];

fn load(text: &str) -> MerryConfig {
    MerryConfig::load_optional_from_text(Some(text), &XdgPaths::from_parts(home(), None, None))
        .expect("config should parse")
        .expect("config should be present")
}

fn cli_defaults(text: &str) -> CliDefaults {
    load(text).cli_defaults()
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
fn cli_defaults_to_no_sandbox_mode_and_no_approval_policy() {
    for text in ["", "[cli]\n"] {
        let defaults = cli_defaults(text);
        assert_eq!(defaults, CliDefaults::default(), "{text:?}");
        assert_eq!(defaults.process_execution_mode(), None, "{text:?}");
        assert_eq!(defaults.approval_policy(), None, "{text:?}");
    }
}

#[test]
fn cli_sandbox_and_approval_policy_each_resolve_alone() {
    for (name, mode) in SANDBOXES {
        let defaults = cli_defaults(&format!("[cli]\nsandbox = \"{name}\"\n"));
        assert_eq!(defaults.process_execution_mode(), Some(mode), "{name}");
        assert_eq!(defaults.approval_policy(), None, "{name}");
    }
    for (name, policy) in POLICIES {
        let defaults = cli_defaults(&format!("[cli]\napproval_policy = \"{name}\"\n"));
        assert_eq!(defaults.process_execution_mode(), None, "{name}");
        assert_eq!(defaults.approval_policy(), Some(policy), "{name}");
    }
}

/// The sandbox decides the execution boundary and the approval policy
/// decides who reviews requests inside it, so every combination is valid,
/// including `none` next to a sandbox and a human reviewer without one.
#[test]
fn cli_accepts_every_sandbox_and_approval_policy_combination() {
    for (sandbox, mode) in SANDBOXES {
        for (policy_name, policy) in POLICIES {
            let text =
                format!("[cli]\nsandbox = \"{sandbox}\"\napproval_policy = \"{policy_name}\"\n");
            let defaults = cli_defaults(&text);
            assert_eq!(defaults.process_execution_mode(), Some(mode), "{text:?}");
            assert_eq!(defaults.approval_policy(), Some(policy), "{text:?}");
        }
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
