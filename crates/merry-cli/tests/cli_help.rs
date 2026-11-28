mod support;

use support::merry;

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
    assert!(stdout.contains("resume"));
    assert!(stdout.contains("completions"));
    assert!(!stdout.contains("debug"));
    assert!(!stdout.contains("tui"));
}

#[test]
fn root_with_sandbox_help_writes_usage_to_stdout_without_reexec() {
    let output = merry()
        .args(["--sandbox", "workspace", "--help"])
        .env("PATH", "")
        .output()
        .expect("merry --sandbox workspace --help should run");

    assert!(output.status.success(), "help should exit successfully");
    assert!(output.stderr.is_empty(), "help should not write stderr");
    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    assert!(stdout.contains("Usage: merry [OPTIONS] [COMMAND]"));
    assert!(stdout.contains("run"));
    assert!(stdout.contains("cmd"));
    assert!(!stdout.contains("debug"));
    assert!(!stdout.contains("tui"));
}

#[test]
fn run_help_writes_usage_to_stdout() {
    let output = merry()
        .args(["run", "--help"])
        .output()
        .expect("merry run --help should run");

    assert!(output.status.success(), "run help should exit successfully");
    assert!(output.stderr.is_empty(), "run help should not write stderr");
    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    assert!(stdout.contains("Usage: merry run"));
    assert!(stdout.contains("--events-jsonl"));
    assert!(stdout.contains("--session-id"));
    assert!(stdout.contains("--resume"));
}
