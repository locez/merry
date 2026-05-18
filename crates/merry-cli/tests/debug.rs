use serde_json::Value;
use std::process::Command;

fn merry() -> Command {
    Command::new(env!("CARGO_BIN_EXE_merry"))
}

fn merry_without_openai_env() -> Command {
    let mut command = merry();
    command
        .env_remove("MERRY_OPENAI_DEBUG")
        .env_remove("OPENAI_API_KEY")
        .env_remove("MERRY_OPENAI_MODEL")
        .env_remove("MERRY_OPENAI_BASE_URL")
        .env_remove("OPENAI_ORG_ID")
        .env_remove("OPENAI_PROJECT_ID");
    command
}

fn assert_debug_output(stdout: &[u8], expected_session_id: &str) {
    let text = std::str::from_utf8(stdout).expect("stdout should be utf-8");
    assert!(text.ends_with('\n'), "stdout should end with a newline");

    let lines = text.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 3, "debug should emit exactly 3 JSON lines");

    let events = lines
        .iter()
        .map(|line| serde_json::from_str::<Value>(line).expect("each line should be JSON"))
        .collect::<Vec<_>>();

    let expected_kinds = ["session_started", "step_started", "step_completed"];
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event["session_id"], expected_session_id);
        assert_eq!(event["sequence"], index as u64);
        assert_eq!(event["kind"]["type"], expected_kinds[index]);
    }
}

#[test]
fn debug_emits_default_runtime_lifecycle_as_json_lines() {
    let output = merry()
        .arg("debug")
        .output()
        .expect("merry debug should run");

    assert!(output.status.success(), "debug should exit successfully");
    assert!(output.stderr.is_empty(), "debug should not write stderr");
    assert_debug_output(&output.stdout, "debug-session");
}

#[test]
fn debug_accepts_custom_session_id_and_input() {
    let output = merry()
        .args([
            "debug",
            "--session-id",
            "custom-session",
            "--input",
            "hello",
        ])
        .output()
        .expect("merry debug should run");

    assert!(output.status.success(), "debug should exit successfully");
    assert!(output.stderr.is_empty(), "debug should not write stderr");
    assert_debug_output(&output.stdout, "custom-session");
}

#[test]
fn unknown_command_exits_with_usage_error() {
    let output = merry()
        .arg("unknown")
        .output()
        .expect("merry unknown should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "usage errors should not write stdout"
    );
    assert!(
        !output.stderr.is_empty(),
        "usage errors should write stderr"
    );
}

#[test]
fn invalid_session_id_exits_with_usage_error() {
    let output = merry()
        .args(["debug", "--session-id", " "])
        .output()
        .expect("merry debug should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "validation errors should not write stdout"
    );
    assert!(
        !output.stderr.is_empty(),
        "validation errors should write stderr"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("Usage: merry debug"));
    assert!(stderr.contains("--session-id <SESSION_ID>"));
    assert!(stderr.contains("--input <TEXT>"));
}

#[test]
fn missing_debug_flag_value_exits_with_debug_usage_error() {
    let output = merry()
        .args(["debug", "--input"])
        .output()
        .expect("merry debug should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "usage errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("--input requires a value"));
    assert!(stderr.contains("Usage: merry debug"));
    assert!(stderr.contains("--session-id <SESSION_ID>"));
    assert!(stderr.contains("--input <TEXT>"));
}

#[test]
fn debug_rejects_openai_after_debug_options_as_unexpected_argument() {
    let output = merry_without_openai_env()
        .args(["debug", "--input", "hello", "openai"])
        .output()
        .expect("merry debug should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "usage errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("unexpected debug argument: openai"));
    assert!(stderr.contains("Usage: merry debug"));
}

#[test]
fn root_help_writes_usage_to_stdout() {
    let output = merry()
        .arg("--help")
        .output()
        .expect("merry --help should run");

    assert!(output.status.success(), "help should exit successfully");
    assert!(output.stderr.is_empty(), "help should not write stderr");
    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    assert!(stdout.contains("Usage: merry <COMMAND>"));
    assert!(stdout.contains("debug"));
}

#[test]
fn debug_help_writes_usage_to_stdout() {
    let output = merry()
        .args(["debug", "--help"])
        .output()
        .expect("merry debug --help should run");

    assert!(
        output.status.success(),
        "debug help should exit successfully"
    );
    assert!(
        output.stderr.is_empty(),
        "debug help should not write stderr"
    );
    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    assert!(stdout.contains("Usage: merry debug"));
    assert!(stdout.contains("--session-id <SESSION_ID>"));
    assert!(stdout.contains("--input <TEXT>"));
    assert!(stdout.contains("openai"));
}

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
    assert!(stdout.contains("MERRY_OPENAI_DEBUG=1"));
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
    assert!(stderr.contains("--input requires a value"));
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
    assert!(stderr.contains("unknown debug openai option: --bad-option"));
    assert!(stderr.contains("Usage: merry debug openai"));
}

#[test]
fn debug_openai_requires_api_key_when_opted_in() {
    let output = merry_without_openai_env()
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
    assert!(stderr.contains("OPENAI_API_KEY"));
    assert!(stderr.contains("Usage: merry debug openai"));
}

#[test]
fn debug_openai_rejects_blank_api_key_when_opted_in() {
    let output = merry_without_openai_env()
        .env("MERRY_OPENAI_DEBUG", "1")
        .env("OPENAI_API_KEY", "")
        .args(["debug", "openai", "--input", "hello", "--model", "gpt-test"])
        .output()
        .expect("merry debug openai should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "usage errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("OPENAI_API_KEY must not be blank"));
    assert!(stderr.contains("Usage: merry debug openai"));
}

#[test]
fn debug_openai_requires_model_from_flag_or_env() {
    let output = merry_without_openai_env()
        .env("MERRY_OPENAI_DEBUG", "1")
        .env("OPENAI_API_KEY", "sk-test")
        .args(["debug", "openai", "--input", "hello"])
        .output()
        .expect("merry debug openai should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "usage errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("--model"));
    assert!(stderr.contains("MERRY_OPENAI_MODEL"));
    assert!(stderr.contains("Usage: merry debug openai"));
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
        assert!(stderr.contains("Usage: merry debug openai"));
    }
}

#[test]
fn debug_openai_rejects_max_output_tokens_until_runtime_generation_config_exists() {
    let output = merry_without_openai_env()
        .env("MERRY_OPENAI_DEBUG", "1")
        .env("OPENAI_API_KEY", "sk-test")
        .args([
            "debug",
            "openai",
            "--input",
            "hello",
            "--model",
            "gpt-test",
            "--max-output-tokens",
            "16",
        ])
        .output()
        .expect("merry debug openai should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "usage errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("--max-output-tokens is not supported"));
    assert!(stderr.contains("Runtime::step generation config"));
    assert!(stderr.contains("Usage: merry debug openai"));
}
