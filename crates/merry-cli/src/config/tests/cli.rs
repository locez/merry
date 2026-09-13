use super::home;
use crate::coding::ProcessExecutionMode;
use crate::config::{CliDefaults, ConfigError, MerryConfig, XdgPaths};

fn load(text: &str) -> MerryConfig {
    MerryConfig::load_optional_from_text(Some(text), &XdgPaths::from_parts(home(), None, None))
        .expect("config should parse")
        .expect("config should be present")
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
fn cli_defaults_to_no_sandbox_mode_and_reviewed() {
    for text in ["", "[cli]\n", "[cli]\nfully_trusted = false\n"] {
        let defaults = load(text).cli_defaults();
        assert_eq!(defaults, CliDefaults::default(), "{text:?}");
        assert_eq!(defaults.process_execution_mode(), None, "{text:?}");
        assert!(!defaults.fully_trusted(), "{text:?}");
    }
}

#[test]
fn cli_sandbox_and_fully_trusted_map_to_modes() {
    for (text, mode, fully_trusted) in [
        (
            "[cli]\nsandbox = \"no-sandbox\"\n",
            Some(ProcessExecutionMode::Unrestricted),
            false,
        ),
        (
            "[cli]\nsandbox = \"with-sandbox\"\n",
            Some(ProcessExecutionMode::OuterAndInner),
            false,
        ),
        (
            "[cli]\nsandbox = \"inner-sandbox\"\nfully_trusted = true\n",
            Some(ProcessExecutionMode::InnerOnly),
            true,
        ),
        ("[cli]\nfully_trusted = true\n", None, true),
    ] {
        let defaults = load(text).cli_defaults();
        assert_eq!(defaults.process_execution_mode(), mode, "{text:?}");
        assert_eq!(defaults.fully_trusted(), fully_trusted, "{text:?}");
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

    let message = parse_error("[cli]\nsandbox = \"fully-trusted\"\n");
    assert!(
        message.contains("unknown variant `fully-trusted`"),
        "{message}"
    );
}

#[test]
fn cli_table_rejects_unknown_keys_and_wrong_types() {
    for text in [
        "[cli]\ndefault_options = [\"no-sandbox\"]\n",
        "[cli]\nsandbox_mode = \"no-sandbox\"\n",
        "[cli]\nsandbox = [\"no-sandbox\"]\n",
        "[cli]\nsandbox = true\n",
        "[cli]\nfully_trusted = \"true\"\n",
    ] {
        let message = parse_error(text);
        assert!(!message.is_empty(), "{text:?}");
    }
}
