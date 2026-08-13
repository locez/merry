mod support;

use support::{event_kinds, merry, parse_jsonl};

#[test]
fn shell_help_writes_usage_to_stdout() {
    let output = merry()
        .args(["debug", "shell", "--help"])
        .output()
        .expect("merry debug shell --help should run");

    assert!(
        output.status.success(),
        "shell help should exit successfully"
    );
    assert!(
        output.stderr.is_empty(),
        "shell help should not write stderr"
    );
    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    assert!(stdout.contains("Usage: merry debug shell"));
    assert!(stdout.contains("-- <ARGV>") || stdout.contains("[-- <ARGV>]"));
    assert!(stdout.contains("ARGV"));
}

#[test]
fn shell_requires_argv() {
    let output = merry()
        .args(["debug", "shell"])
        .output()
        .expect("merry debug shell should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "usage errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("Usage: merry debug shell"));
    assert!(stderr.contains("ARGV") || stderr.contains("required"));
}

#[test]
fn shell_rejects_argv_without_separator() {
    let output = merry()
        .args(["debug", "shell", "rustc", "--version"])
        .output()
        .expect("merry debug shell should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "usage errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("Usage: merry debug shell") || stderr.contains("unexpected argument"));
    assert!(stderr.contains("rustc") || stderr.contains("ARGV"));
}

#[test]
fn shell_rustc_version_prints_process_stdout() {
    let output = merry()
        .args(["debug", "shell", "--", "rustc", "--version"])
        .output()
        .expect("merry debug shell should run");

    assert!(output.status.success(), "shell should exit successfully");
    assert!(output.stderr.is_empty(), "shell should not write stderr");

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    assert!(stdout.starts_with("rustc "));
    assert!(!stdout.contains("tool_call_pending"));
}

#[test]
fn shell_events_jsonl_records_exact_argv_and_resolves_success() {
    let output = merry()
        .args(["debug", "shell", "--events-jsonl", "--", "rg", "--files"])
        .output()
        .expect("merry debug shell should run");

    assert!(output.status.success(), "shell should exit successfully");
    assert!(output.stderr.is_empty(), "shell should not write stderr");

    let events = parse_jsonl(&output.stdout);
    let kinds = event_kinds(&events);
    assert!(kinds.contains(&"tool_call_pending"));
    assert!(kinds.contains(&"artifact_recorded"));
    assert!(kinds.contains(&"tool_call_resolved"));

    let pending = events
        .iter()
        .find(|event| event["payload"]["type"] == "tool_call_pending")
        .expect("shell tool call should be pending before execution");
    assert_eq!(
        pending["payload"]["call"]["arguments"]["command"],
        "rg --files"
    );
    assert_eq!(pending["payload"]["call"]["arguments"]["cwd"], ".");

    let resolved = events
        .iter()
        .find(|event| event["payload"]["type"] == "tool_call_resolved")
        .expect("shell tool call should resolve");
    assert_eq!(
        resolved["payload"]["result"]["call_id"],
        "call-shell-command"
    );
    assert_eq!(resolved["payload"]["result"]["status"], "succeeded");
    assert!(resolved["payload"]["result"]["diagnostic"].is_null());
}

#[test]
fn shell_rg_files_prints_process_stdout() {
    let output = merry()
        .args(["debug", "shell", "--", "rg", "--files"])
        .output()
        .expect("merry debug shell should run");

    assert!(output.status.success(), "shell should exit successfully");
    assert!(output.stderr.is_empty(), "shell should not write stderr");

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    assert!(stdout.contains("Cargo.toml"));
    assert!(!stdout.contains("tool_call_pending"));
}

#[test]
fn shell_forbidden_command_denies_without_running_raw_command() {
    let output = merry()
        .args([
            "debug",
            "shell",
            "--events-jsonl",
            "--",
            "sh",
            "-c",
            "echo bad",
        ])
        .output()
        .expect("merry debug shell should run");

    assert!(
        output.status.success(),
        "policy denial is a recorded runtime outcome"
    );
    assert!(output.stderr.is_empty(), "shell should not write stderr");

    let events = parse_jsonl(&output.stdout);
    let kinds = event_kinds(&events);
    assert!(kinds.contains(&"tool_call_pending"));
    assert!(kinds.contains(&"artifact_recorded"));
    assert!(kinds.contains(&"tool_call_resolved"));

    let resolved = events
        .iter()
        .find(|event| event["payload"]["type"] == "tool_call_resolved")
        .expect("shell tool call should resolve");
    assert_eq!(
        resolved["payload"]["result"]["call_id"],
        "call-shell-command"
    );
    assert_eq!(resolved["payload"]["result"]["status"], "failed");
    assert_eq!(
        resolved["payload"]["result"]["diagnostic"]["code"],
        "action_policy_denied"
    );
}

#[test]
fn shell_local_workspace_effect_denies_without_sandbox_admission_or_raw_cargo_output() {
    let output = merry()
        .args([
            "debug",
            "shell",
            "--events-jsonl",
            "--",
            "cargo",
            "test",
            "-p",
            "merry-runtime",
        ])
        .env_remove("MERRY_SANDBOX")
        .env_remove("MERRY_SANDBOX_VERSION")
        .output()
        .expect("merry debug shell should run");

    assert!(
        output.status.success(),
        "policy denial is a recorded runtime outcome"
    );
    assert!(output.stderr.is_empty(), "shell should not write stderr");

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    assert!(
        !stdout.contains("running "),
        "raw cargo test output should not appear in CLI stdout"
    );
    assert!(
        !stdout.contains("test result:"),
        "raw cargo test output should not appear in CLI stdout"
    );

    let events = parse_jsonl(&output.stdout);
    let kinds = event_kinds(&events);
    assert!(kinds.contains(&"tool_call_pending"));
    assert!(kinds.contains(&"artifact_recorded"));
    assert!(kinds.contains(&"tool_call_resolved"));

    let resolved = events
        .iter()
        .find(|event| event["payload"]["type"] == "tool_call_resolved")
        .expect("shell tool call should resolve");
    assert_eq!(
        resolved["payload"]["result"]["call_id"],
        "call-shell-command"
    );
    assert_eq!(resolved["payload"]["result"]["status"], "failed");
    assert_eq!(
        resolved["payload"]["result"]["diagnostic"]["code"],
        "action_policy_denied"
    );
}

#[test]
fn shell_spoofed_sandbox_markers_do_not_enable_local_workspace_effect() {
    let output = merry()
        .args([
            "debug",
            "shell",
            "--events-jsonl",
            "--",
            "cargo",
            "test",
            "-p",
            "merry-runtime",
        ])
        .env("MERRY_SANDBOX", "1")
        .env("MERRY_SANDBOX_VERSION", "1")
        .output()
        .expect("merry debug shell should run");

    assert!(
        output.status.success(),
        "policy denial is a recorded runtime outcome"
    );
    assert!(output.stderr.is_empty(), "shell should not write stderr");

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    assert!(
        !stdout.contains("running "),
        "raw cargo test output should not appear in CLI stdout"
    );
    assert!(
        !stdout.contains("test result:"),
        "raw cargo test output should not appear in CLI stdout"
    );

    let events = parse_jsonl(&output.stdout);
    let resolved = events
        .iter()
        .find(|event| event["payload"]["type"] == "tool_call_resolved")
        .expect("shell tool call should resolve");
    assert_eq!(
        resolved["payload"]["result"]["call_id"],
        "call-shell-command"
    );
    assert_eq!(resolved["payload"]["result"]["status"], "failed");
    assert_eq!(
        resolved["payload"]["result"]["diagnostic"]["code"],
        "action_policy_denied"
    );
}

#[test]
fn shell_spoofed_sandbox_markers_with_explicit_accept_do_not_enable_local_workspace_effect() {
    let output = merry()
        .args([
            "debug",
            "shell",
            "--accept-local-workspace-process-risk",
            "--events-jsonl",
            "--",
            "cargo",
            "test",
            "-p",
            "merry-runtime",
        ])
        .env("MERRY_SANDBOX", "1")
        .env("MERRY_SANDBOX_VERSION", "1")
        .output()
        .expect("merry debug shell should run");

    assert!(
        output.status.success(),
        "policy denial is a recorded runtime outcome"
    );
    assert!(output.stderr.is_empty(), "shell should not write stderr");

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    assert!(
        !stdout.contains("running "),
        "raw cargo test output should not appear in CLI stdout"
    );
    assert!(
        !stdout.contains("test result:"),
        "raw cargo test output should not appear in CLI stdout"
    );

    let events = parse_jsonl(&output.stdout);
    let resolved = events
        .iter()
        .find(|event| event["payload"]["type"] == "tool_call_resolved")
        .expect("shell tool call should resolve");
    assert_eq!(
        resolved["payload"]["result"]["call_id"],
        "call-shell-command"
    );
    assert_eq!(resolved["payload"]["result"]["status"], "failed");
    assert_eq!(
        resolved["payload"]["result"]["diagnostic"]["code"],
        "action_policy_denied"
    );
}

#[test]
fn shell_forged_hidden_handoff_markers_and_accept_do_not_enable_local_workspace_effect() {
    let output = merry()
        .args([
            "--merry-sandbox-child-handoff",
            "cli-bwrap-v1",
            "debug",
            "shell",
            "--accept-local-workspace-process-risk",
            "--events-jsonl",
            "--",
            "cargo",
            "test",
            "-p",
            "merry-runtime",
        ])
        .env("MERRY_SANDBOX", "1")
        .env("MERRY_SANDBOX_VERSION", "1")
        .env("HOME", "/home/merry")
        .env("TMPDIR", "/tmp")
        .output()
        .expect("merry debug shell should run");

    assert!(
        output.status.success(),
        "policy denial is a recorded runtime outcome"
    );
    assert!(output.stderr.is_empty(), "shell should not write stderr");

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    assert!(
        !stdout.contains("running "),
        "raw cargo test output should not appear in CLI stdout"
    );
    assert!(
        !stdout.contains("test result:"),
        "raw cargo test output should not appear in CLI stdout"
    );

    let events = parse_jsonl(&output.stdout);
    let resolved = events
        .iter()
        .find(|event| event["payload"]["type"] == "tool_call_resolved")
        .expect("shell tool call should resolve");
    assert_eq!(
        resolved["payload"]["result"]["call_id"],
        "call-shell-command"
    );
    assert_eq!(resolved["payload"]["result"]["status"], "failed");
    assert_eq!(
        resolved["payload"]["result"]["diagnostic"]["code"],
        "action_policy_denied"
    );
}
