mod support;

use std::fs;
use support::{merry_without_openai_env, merry_without_openai_env_and_xdg, write_xdg_config};

#[test]
fn debug_openai_help_writes_usage_to_stdout() {
    let output = merry_without_openai_env()
        .args(["debug", "openai", "--help"])
        .output()
        .expect("merry debug openai --help should run");

    assert!(
        output.status.success(),
        "debug openai help should exit successfully"
    );
    assert!(
        output.stderr.is_empty(),
        "debug openai help should not write stderr"
    );
    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    assert!(stdout.contains("Usage: merry debug openai"));
    assert!(stdout.contains("--input <TEXT>"));
    assert!(stdout.contains("--model <MODEL>"));
    assert!(stdout.contains("--max-output-tokens <N>"));
    assert!(stdout.contains("--debug-tool-result <TEXT>"));
    assert!(stdout.contains("Optional maximum output tokens"));
    assert!(stdout.contains("Require first step to call debug_echo"));
    assert!(!stdout.contains("Rejected until"));
    assert!(stdout.contains("MERRY_OPENAI_DEBUG=1"));
    assert!(stdout.contains("XDG_CONFIG_HOME"));
    assert!(stdout.contains("config.toml"));
    assert!(stdout.contains("api_key"));
    assert!(stdout.contains("api_key_file"));
    assert!(!stdout.contains("MERRY_OPENAI_API_KEY"));
    assert!(!stdout.contains("OPENAI_API_KEY"));
}

#[test]
fn debug_openai_requires_merry_openai_debug() {
    let output = merry_without_openai_env()
        .args(["debug", "openai", "--input", "hello", "--model", "gpt-test"])
        .output()
        .expect("merry debug openai should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "usage errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("MERRY_OPENAI_DEBUG=1"));
    assert!(stderr.contains("Usage: merry debug openai"));
}

#[test]
fn debug_openai_requires_input() {
    let output = merry_without_openai_env()
        .args(["debug", "openai", "--model", "gpt-test"])
        .output()
        .expect("merry debug openai should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "usage errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("the following required arguments were not provided"));
    assert!(stderr.contains("--input <TEXT>"));
    assert!(stderr.contains("Usage: merry debug openai"));
}

#[test]
fn debug_openai_rejects_unknown_option() {
    let output = merry_without_openai_env()
        .args(["debug", "openai", "--bad-option"])
        .output()
        .expect("merry debug openai should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "usage errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("unexpected argument '--bad-option'"));
    assert!(stderr.contains("Usage: merry debug openai"));
}

#[test]
fn debug_openai_requires_debug_tool_result_value() {
    let output = merry_without_openai_env()
        .args([
            "debug",
            "openai",
            "--input",
            "hello",
            "--model",
            "gpt-test",
            "--debug-tool-result",
        ])
        .output()
        .expect("merry debug openai should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "usage errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("a value is required for '--debug-tool-result <TEXT>'"));
    assert!(stderr.contains("try '--help'"));
}

#[test]
fn debug_openai_requires_xdg_provider_config_when_opted_in() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let output = merry_without_openai_env_and_xdg(&temp)
        .env("MERRY_OPENAI_DEBUG", "1")
        .args(["debug", "openai", "--input", "hello", "--model", "gpt-test"])
        .output()
        .expect("merry debug openai should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "usage errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("Merry XDG provider config is required"));
    assert!(stderr.contains("Usage: merry debug openai"));
}

#[test]
fn debug_openai_requires_configured_api_key_source_when_opted_in() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    write_xdg_config(
        &temp,
        r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"

[providers.openai-compatible]
base_url = "https://api.example.test/v1"
"#,
    );

    let output = merry_without_openai_env_and_xdg(&temp)
        .env("MERRY_OPENAI_DEBUG", "1")
        .args(["debug", "openai", "--input", "hello", "--model", "gpt-test"])
        .output()
        .expect("merry debug openai should run");

    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stdout.is_empty(),
        "config errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(
        stderr.contains(
            "providers.openai-compatible must set exactly one of api_key or api_key_file"
        )
    );
    assert!(!stderr.contains("Usage: merry debug openai"));
}

#[test]
fn debug_openai_rejects_blank_configured_api_key_when_opted_in() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    write_xdg_config(
        &temp,
        r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"

[providers.openai-compatible]
api_key = "  "
"#,
    );

    let output = merry_without_openai_env_and_xdg(&temp)
        .env("MERRY_OPENAI_DEBUG", "1")
        .args(["debug", "openai", "--input", "hello", "--model", "gpt-test"])
        .output()
        .expect("merry debug openai should run");

    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stdout.is_empty(),
        "config errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("api_key must not be blank"));
    assert!(!stderr.contains("Usage: merry debug openai"));
}

#[test]
fn debug_openai_rejects_ambiguous_configured_api_key_sources_when_opted_in() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    write_xdg_config(
        &temp,
        r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"

[providers.openai-compatible]
api_key = "sk-inline-secret"
api_key_file = "secrets/openai.key"
"#,
    );

    let output = merry_without_openai_env_and_xdg(&temp)
        .env("MERRY_OPENAI_DEBUG", "1")
        .args(["debug", "openai", "--input", "hello", "--model", "gpt-test"])
        .output()
        .expect("merry debug openai should run");

    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stdout.is_empty(),
        "config errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("must not set both api_key and api_key_file"));
    assert!(!stderr.contains("sk-inline-secret"));
    assert!(!stderr.contains("Usage: merry debug openai"));
}

#[test]
fn debug_openai_rejects_blank_configured_api_key_file_when_opted_in() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let secret_dir = temp.path().join("config/merry/secrets");
    fs::create_dir_all(&secret_dir).expect("secret dir should be created");
    fs::write(secret_dir.join("openai.key"), "  \n").expect("secret should write");
    write_xdg_config(
        &temp,
        r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"

[providers.openai-compatible]
api_key_file = "secrets/openai.key"
"#,
    );

    let output = merry_without_openai_env_and_xdg(&temp)
        .env("MERRY_OPENAI_DEBUG", "1")
        .args(["debug", "openai", "--input", "hello", "--model", "gpt-test"])
        .output()
        .expect("merry debug openai should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "usage errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("api_key_file"));
    assert!(stderr.contains("must not be blank"));
    assert!(stderr.contains("Usage: merry debug openai"));
}

#[test]
fn debug_openai_rejects_unsupported_configured_default_provider() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    write_xdg_config(
        &temp,
        r#"
[providers.default]
provider = "other"
model = "gpt-test"

[providers.openai-compatible]
api_key_file = "secrets/openai.key"
"#,
    );

    let output = merry_without_openai_env_and_xdg(&temp)
        .env("MERRY_OPENAI_DEBUG", "1")
        .args(["debug", "openai", "--input", "hello", "--model", "gpt-test"])
        .output()
        .expect("merry debug openai should run");

    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stdout.is_empty(),
        "config errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("unsupported default provider other"));
    assert!(!stderr.contains("Usage: merry debug openai"));
}

#[test]
fn debug_openai_rejects_zero_or_invalid_max_output_tokens() {
    for value in ["0", "not-a-number"] {
        let output = merry_without_openai_env()
            .args([
                "debug",
                "openai",
                "--input",
                "hello",
                "--model",
                "gpt-test",
                "--max-output-tokens",
                value,
            ])
            .output()
            .expect("merry debug openai should run");

        assert_eq!(output.status.code(), Some(2));
        assert!(
            output.stdout.is_empty(),
            "usage errors should not write stdout"
        );
        let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
        assert!(stderr.contains("--max-output-tokens"));
        assert!(stderr.contains("invalid value"));
    }
}
