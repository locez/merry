use super::home;
use crate::coding::ProcessExecutionMode;
use crate::config::{CliDefaultOptions, ConfigError, MerryConfig, XdgPaths};

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

fn invalid_message(text: &str) -> String {
    match load(text).cli_default_options() {
        Ok(options) => panic!("{text:?} should be rejected, resolved {options:?}"),
        Err(ConfigError::Invalid(message)) => message,
        Err(error) => panic!("{text:?} should be an invalid-config error, got {error}"),
    }
}

#[test]
fn cli_default_options_default_to_no_mode_and_reviewed() {
    for text in ["", "[cli]\n", "[cli]\ndefault_options = []\n"] {
        let defaults = load(text)
            .cli_default_options()
            .expect("empty defaults are valid");
        assert_eq!(defaults, CliDefaultOptions::default(), "{text:?}");
        assert_eq!(defaults.process_execution_mode(), None, "{text:?}");
        assert!(!defaults.fully_trusted(), "{text:?}");
    }
}

#[test]
fn cli_default_options_map_names_to_modes() {
    for (text, mode, fully_trusted) in [
        (
            "[cli]\ndefault_options = \"no-sandbox\"\n",
            Some(ProcessExecutionMode::Unrestricted),
            false,
        ),
        (
            "[cli]\ndefault_options = [\"inner-sandbox\"]\n",
            Some(ProcessExecutionMode::InnerOnly),
            false,
        ),
        (
            "[cli]\ndefault_options = [\"with-sandbox\", \"fully-trusted\"]\n",
            Some(ProcessExecutionMode::OuterAndInner),
            true,
        ),
        (
            "[cli]\ndefault_options = [\"fully-trusted\", \"inner-sandbox\"]\n",
            Some(ProcessExecutionMode::InnerOnly),
            true,
        ),
        ("[cli]\ndefault_options = \"fully-trusted\"\n", None, true),
    ] {
        let defaults = load(text)
            .cli_default_options()
            .expect("default options should map to modes");
        assert_eq!(defaults.process_execution_mode(), mode, "{text:?}");
        assert_eq!(defaults.fully_trusted(), fully_trusted, "{text:?}");
    }
}

#[test]
fn cli_default_options_reject_raw_flags_and_unknown_names() {
    let message = parse_error("[cli]\ndefault_options = \"--no-sandbox\"\n");
    assert!(
        message.contains("unknown variant `--no-sandbox`"),
        "{message}"
    );
    assert!(
        message.contains("`with-sandbox`, `no-sandbox`, `inner-sandbox`, `fully-trusted`"),
        "{message}"
    );

    let message = parse_error("[cli]\ndefault_options = [\"inner-sandbox\", \"trusted\"]\n");
    assert!(message.contains("unknown variant `trusted`"), "{message}");
}

#[test]
fn cli_default_options_reject_repeated_and_conflicting_sandbox_modes() {
    let message =
        invalid_message("[cli]\ndefault_options = [\"fully-trusted\", \"fully-trusted\"]\n");
    assert!(message.contains("repeats fully-trusted"), "{message}");

    let message = invalid_message("[cli]\ndefault_options = [\"no-sandbox\", \"with-sandbox\"]\n");
    assert!(
        message.contains("cannot combine no-sandbox with with-sandbox"),
        "{message}"
    );

    let message = invalid_message(
        "[cli]\ndefault_options = [\"inner-sandbox\", \"fully-trusted\", \"no-sandbox\"]\n",
    );
    assert!(
        message.contains("cannot combine inner-sandbox with no-sandbox"),
        "{message}"
    );
}

#[test]
fn cli_table_rejects_unknown_keys_and_non_string_options() {
    for text in [
        "[cli]\ndefault_option = \"no-sandbox\"\n",
        "[cli]\ndefault_options = true\n",
        "[cli]\ndefault_options = [1]\n",
        "[cli]\ndefault_options = { mode = \"no-sandbox\" }\n",
    ] {
        let message = parse_error(text);
        assert!(!message.is_empty(), "{text:?}");
    }
}
