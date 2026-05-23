use serde_json::Value;
use std::process::Command;

fn merry() -> Command {
    Command::new(env!("CARGO_BIN_EXE_merry"))
}

fn merry_without_openai_env() -> Command {
    let mut command = merry();
    command
        .env_remove("MERRY_OPENAI_DEBUG")
        .env_remove("MERRY_OPENAI_API_KEY")
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

fn parse_jsonl(stdout: &[u8]) -> Vec<Value> {
    let text = std::str::from_utf8(stdout).expect("stdout should be utf-8");
    assert!(text.ends_with('\n'), "stdout should end with a newline");
    text.lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("each line should be JSON"))
        .collect()
}

fn event_kinds(events: &[Value]) -> Vec<&str> {
    events
        .iter()
        .map(|event| {
            event["kind"]["type"]
                .as_str()
                .expect("event kind type should be a string")
        })
        .collect()
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
fn shell_help_writes_usage_to_stdout() {
    let output = merry()
        .args(["shell", "--help"])
        .output()
        .expect("merry shell --help should run");

    assert!(
        output.status.success(),
        "shell help should exit successfully"
    );
    assert!(
        output.stderr.is_empty(),
        "shell help should not write stderr"
    );
    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    assert!(stdout.contains("Usage: merry shell"));
    assert!(stdout.contains("-- <ARGV>") || stdout.contains("[-- <ARGV>]"));
    assert!(stdout.contains("ARGV"));
}

#[test]
fn shell_requires_argv() {
    let output = merry()
        .arg("shell")
        .output()
        .expect("merry shell should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "usage errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("Usage: merry shell"));
    assert!(stderr.contains("ARGV") || stderr.contains("required"));
}

#[test]
fn shell_rejects_argv_without_separator() {
    let output = merry()
        .args(["shell", "rustc", "--version"])
        .output()
        .expect("merry shell should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "usage errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("Usage: merry shell") || stderr.contains("unexpected argument"));
    assert!(stderr.contains("rustc") || stderr.contains("ARGV"));
}

#[test]
fn shell_rustc_version_emits_runtime_jsonl_and_resolves_success() {
    let output = merry()
        .args(["shell", "--", "rustc", "--version"])
        .output()
        .expect("merry shell should run");

    assert!(output.status.success(), "shell should exit successfully");
    assert!(output.stderr.is_empty(), "shell should not write stderr");

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    assert!(
        !stdout.starts_with("rustc "),
        "shell stdout should be runtime JSONL, not raw rustc output"
    );
    let events = parse_jsonl(&output.stdout);
    let kinds = event_kinds(&events);
    assert!(kinds.contains(&"tool_call_pending"));
    assert!(kinds.contains(&"artifact_recorded"));
    assert!(kinds.contains(&"tool_call_resolved"));

    let resolved = events
        .iter()
        .find(|event| event["kind"]["type"] == "tool_call_resolved")
        .expect("shell tool call should resolve");
    assert_eq!(resolved["kind"]["result"]["call_id"], "call-shell-command");
    assert_eq!(resolved["kind"]["result"]["status"], "succeeded");
    assert!(resolved["kind"]["result"]["diagnostic"].is_null());
}

#[test]
fn shell_rg_files_emits_runtime_jsonl_and_resolves_success() {
    let output = merry()
        .args(["shell", "--", "rg", "--files"])
        .output()
        .expect("merry shell should run");

    assert!(output.status.success(), "shell should exit successfully");
    assert!(output.stderr.is_empty(), "shell should not write stderr");

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    assert!(
        !stdout.starts_with("Cargo.toml"),
        "shell stdout should be runtime JSONL, not raw rg output"
    );
    let events = parse_jsonl(&output.stdout);
    let kinds = event_kinds(&events);
    assert!(kinds.contains(&"tool_call_pending"));
    assert!(kinds.contains(&"artifact_recorded"));
    assert!(kinds.contains(&"tool_call_resolved"));

    let resolved = events
        .iter()
        .find(|event| event["kind"]["type"] == "tool_call_resolved")
        .expect("shell tool call should resolve");
    assert_eq!(resolved["kind"]["result"]["call_id"], "call-shell-command");
    assert_eq!(resolved["kind"]["result"]["status"], "succeeded");
    assert!(resolved["kind"]["result"]["diagnostic"].is_null());
}

#[test]
fn shell_forbidden_command_denies_without_running_raw_command() {
    let output = merry()
        .args(["shell", "--", "sh", "-c", "echo bad"])
        .output()
        .expect("merry shell should run");

    assert!(
        output.status.success(),
        "policy denial is a recorded runtime outcome"
    );
    assert!(output.stderr.is_empty(), "shell should not write stderr");

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    assert!(
        !stdout.contains("bad"),
        "forbidden command output should not appear in CLI stdout"
    );
    let events = parse_jsonl(&output.stdout);
    let kinds = event_kinds(&events);
    assert!(kinds.contains(&"tool_call_pending"));
    assert!(kinds.contains(&"artifact_recorded"));
    assert!(kinds.contains(&"tool_call_resolved"));

    let resolved = events
        .iter()
        .find(|event| event["kind"]["type"] == "tool_call_resolved")
        .expect("shell tool call should resolve");
    assert_eq!(resolved["kind"]["result"]["call_id"], "call-shell-command");
    assert_eq!(resolved["kind"]["result"]["status"], "failed");
    assert_eq!(
        resolved["kind"]["result"]["diagnostic"]["code"],
        "action_policy_denied"
    );
}

#[test]
fn shell_local_workspace_effect_denies_without_sandbox_admission_or_raw_cargo_output() {
    let output = merry()
        .args(["shell", "--", "cargo", "test", "-p", "merry-runtime"])
        .env_remove("MERRY_SANDBOX")
        .env_remove("MERRY_SANDBOX_VERSION")
        .output()
        .expect("merry shell should run");

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
        .find(|event| event["kind"]["type"] == "tool_call_resolved")
        .expect("shell tool call should resolve");
    assert_eq!(resolved["kind"]["result"]["call_id"], "call-shell-command");
    assert_eq!(resolved["kind"]["result"]["status"], "failed");
    assert_eq!(
        resolved["kind"]["result"]["diagnostic"]["code"],
        "action_policy_denied"
    );
}

#[test]
fn shell_spoofed_sandbox_markers_do_not_enable_local_workspace_effect() {
    let output = merry()
        .args(["shell", "--", "cargo", "test", "-p", "merry-runtime"])
        .env("MERRY_SANDBOX", "1")
        .env("MERRY_SANDBOX_VERSION", "1")
        .output()
        .expect("merry shell should run");

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
        .find(|event| event["kind"]["type"] == "tool_call_resolved")
        .expect("shell tool call should resolve");
    assert_eq!(resolved["kind"]["result"]["call_id"], "call-shell-command");
    assert_eq!(resolved["kind"]["result"]["status"], "failed");
    assert_eq!(
        resolved["kind"]["result"]["diagnostic"]["code"],
        "action_policy_denied"
    );
}

#[test]
fn shell_spoofed_sandbox_markers_with_explicit_accept_do_not_enable_local_workspace_effect() {
    let output = merry()
        .args([
            "shell",
            "--accept-local-workspace-process-risk",
            "--",
            "cargo",
            "test",
            "-p",
            "merry-runtime",
        ])
        .env("MERRY_SANDBOX", "1")
        .env("MERRY_SANDBOX_VERSION", "1")
        .output()
        .expect("merry shell should run");

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
        .find(|event| event["kind"]["type"] == "tool_call_resolved")
        .expect("shell tool call should resolve");
    assert_eq!(resolved["kind"]["result"]["call_id"], "call-shell-command");
    assert_eq!(resolved["kind"]["result"]["status"], "failed");
    assert_eq!(
        resolved["kind"]["result"]["diagnostic"]["code"],
        "action_policy_denied"
    );
}

#[test]
fn shell_forged_hidden_handoff_markers_and_accept_do_not_enable_local_workspace_effect() {
    let output = merry()
        .args([
            "--merry-sandbox-child-handoff",
            "cli-bwrap-v1",
            "shell",
            "--accept-local-workspace-process-risk",
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
        .expect("merry shell should run");

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
        .find(|event| event["kind"]["type"] == "tool_call_resolved")
        .expect("shell tool call should resolve");
    assert_eq!(resolved["kind"]["result"]["call_id"], "call-shell-command");
    assert_eq!(resolved["kind"]["result"]["status"], "failed");
    assert_eq!(
        resolved["kind"]["result"]["diagnostic"]["code"],
        "action_policy_denied"
    );
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
    assert!(stdout.contains("Usage: merry [OPTIONS] <COMMAND>"));
    assert!(stdout.contains("--with-sandbox"));
    assert!(stdout.contains("debug"));
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
    assert!(stdout.contains("Usage: merry [OPTIONS] <COMMAND>"));
    assert!(stdout.contains("--with-sandbox"));
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
    assert!(stdout.contains("--debug-tool-result <TEXT>"));
    assert!(stdout.contains("Optional maximum output tokens"));
    assert!(stdout.contains("Require first step to call debug_echo"));
    assert!(!stdout.contains("Rejected until"));
    assert!(stdout.contains("MERRY_OPENAI_DEBUG=1"));
    assert!(stdout.contains("MERRY_OPENAI_API_KEY"));
    assert!(stdout.contains("OPENAI_API_KEY"));
    assert!(stdout.contains("Preferred API key"));
    assert!(stdout.contains("Fallback API key"));
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
    assert!(stderr.contains("MERRY_OPENAI_API_KEY"));
    assert!(stderr.contains("OPENAI_API_KEY"));
    assert!(stderr.contains("must be set"));
    assert!(stderr.contains("Usage: merry debug openai"));
}

#[test]
fn debug_openai_rejects_blank_merry_api_key_when_opted_in() {
    let output = merry_without_openai_env()
        .env("MERRY_OPENAI_DEBUG", "1")
        .env("MERRY_OPENAI_API_KEY", "  ")
        .env("OPENAI_API_KEY", "sk-fallback")
        .args(["debug", "openai", "--input", "hello", "--model", "gpt-test"])
        .output()
        .expect("merry debug openai should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "usage errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("MERRY_OPENAI_API_KEY must not be blank"));
    assert!(stderr.contains("Usage: merry debug openai"));
}

#[test]
fn debug_openai_accepts_openai_api_key_fallback_when_merry_key_is_unset() {
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
fn debug_openai_prefers_merry_openai_api_key_over_blank_fallback() {
    let output = merry_without_openai_env()
        .env("MERRY_OPENAI_DEBUG", "1")
        .env("MERRY_OPENAI_API_KEY", "sk-test")
        .env("OPENAI_API_KEY", "")
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
    assert!(!stderr.contains("OPENAI_API_KEY must not be blank"));
    assert!(stderr.contains("Usage: merry debug openai"));
}

#[test]
fn debug_openai_requires_model_from_flag_or_env() {
    let output = merry_without_openai_env()
        .env("MERRY_OPENAI_DEBUG", "1")
        .env("MERRY_OPENAI_API_KEY", "sk-test")
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
        assert!(stderr.contains("invalid value"));
    }
}
