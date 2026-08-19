mod support;

use support::{merry, merry_without_openai_env};

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
    assert!(stderr.contains("a value is required for '--input <TEXT>'"));
    assert!(stderr.contains("try '--help'"));
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
    assert!(stderr.contains("the subcommand 'openai' cannot be used with '--input <TEXT>'"));
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
    assert!(stdout.contains("Usage: merry [OPTIONS] [COMMAND]"));
    assert!(stdout.contains("run"));
    assert!(stdout.contains("cmd"));
    assert!(stdout.contains("debug"));
    assert!(!stdout.contains("tui"));
}

#[test]
fn root_with_sandbox_help_writes_usage_to_stdout_without_reexec() {
    let output = merry()
        .args(["--with-sandbox", "--help"])
        .env("PATH", "")
        .output()
        .expect("merry --with-sandbox --help should run");

    assert!(output.status.success(), "help should exit successfully");
    assert!(output.stderr.is_empty(), "help should not write stderr");
    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    assert!(stdout.contains("Usage: merry [OPTIONS] [COMMAND]"));
    assert!(stdout.contains("run"));
    assert!(stdout.contains("cmd"));
    assert!(stdout.contains("debug"));
    assert!(!stdout.contains("tui"));
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
