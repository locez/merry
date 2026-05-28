use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

fn merry() -> Command {
    static COMMAND_COUNTER: AtomicUsize = AtomicUsize::new(0);
    let sequence = COMMAND_COUNTER.fetch_add(1, Ordering::SeqCst);
    let xdg_root = std::env::temp_dir().join(format!(
        "merry-cli-debug-test-{}-{sequence}",
        std::process::id()
    ));
    let mut command = Command::new(env!("CARGO_BIN_EXE_merry"));
    command
        .env("XDG_CONFIG_HOME", xdg_root.join("config"))
        .env("XDG_STATE_HOME", xdg_root.join("state"));
    command
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

fn merry_without_openai_env_and_xdg(temp: &tempfile::TempDir) -> Command {
    let mut command = merry_without_openai_env();
    command
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .env("XDG_STATE_HOME", temp.path().join("state"));
    command
}

fn write_xdg_config(temp: &tempfile::TempDir, text: &str) {
    let config_dir = temp.path().join("config/merry");
    fs::create_dir_all(&config_dir).expect("config dir should be created");
    fs::write(config_dir.join("config.toml"), text).expect("config should write");
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("merry-cli lives under crates/merry-cli")
        .to_path_buf()
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
fn debug_writes_configured_json_log_without_changing_stdout() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let config_dir = temp.path().join("config/merry");
    let state_dir = temp.path().join("state");
    fs::create_dir_all(&config_dir).expect("config dir should be created");
    let log_path = state_dir.join("merry/logs/merry.jsonl");
    fs::write(
        config_dir.join("config.toml"),
        format!(
            "[observability.log]\nenabled = true\nlevel = \"debug\"\nformat = \"json\"\npath = {:?}\n",
            log_path
        ),
    )
    .expect("config should write");

    let output = merry()
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .env("XDG_STATE_HOME", &state_dir)
        .arg("debug")
        .output()
        .expect("merry debug should run");

    assert!(output.status.success(), "debug should exit successfully");
    assert!(
        output.stderr.is_empty(),
        "debug should not write stderr when logging is file-backed"
    );
    assert_debug_output(&output.stdout, "debug-session");

    let log = fs::read_to_string(&log_path).expect("log file should exist");
    assert!(log.contains("runtime.step"));
    assert!(log.contains("debug-session"));
}

#[test]
fn debug_command_writes_runtime_action_logs_to_default_xdg_state_path() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let state_dir = temp.path().join("state");
    write_xdg_config(
        &temp,
        "[observability.log]\nenabled = true\nlevel = \"debug\"\nformat = \"json\"\n",
    );

    let output = merry_without_openai_env_and_xdg(&temp)
        .arg("debug")
        .output()
        .expect("merry debug should run");

    assert!(output.status.success(), "debug should exit successfully");
    assert!(
        output.stderr.is_empty(),
        "debug should not write stderr when logging is file-backed"
    );
    assert_debug_output(&output.stdout, "debug-session");

    let log_path = state_dir.join("merry/logs/merry.jsonl");
    let log = fs::read_to_string(&log_path).expect("default log file should exist");
    assert!(log.contains("runtime.step"));
    assert!(log.contains("debug-session"));
}

#[test]
fn debug_command_fails_clearly_when_default_log_parent_cannot_be_created() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let state_path = temp.path().join("state");
    write_xdg_config(
        &temp,
        "[observability.log]\nenabled = true\nlevel = \"info\"\nformat = \"json\"\n",
    );
    fs::write(&state_path, "not a directory").expect("state blocker should write");

    let output = merry_without_openai_env()
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .env("XDG_STATE_HOME", &state_path)
        .arg("debug")
        .output()
        .expect("merry debug should run");

    assert!(!output.status.success(), "debug should fail");
    assert!(
        output.stdout.is_empty(),
        "failed logging setup should not write command stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(
        stderr.contains("failed to create log directory")
            || stderr.contains("failed to open log file")
    );
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
fn shell_rustc_version_prints_process_stdout() {
    let output = merry()
        .args(["shell", "--", "rustc", "--version"])
        .output()
        .expect("merry shell should run");

    assert!(output.status.success(), "shell should exit successfully");
    assert!(output.stderr.is_empty(), "shell should not write stderr");

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    assert!(stdout.starts_with("rustc "));
    assert!(!stdout.contains("tool_call_pending"));
}

#[test]
fn shell_events_jsonl_records_exact_argv_and_resolves_success() {
    let output = merry()
        .args(["shell", "--events-jsonl", "--", "rg", "--files"])
        .output()
        .expect("merry shell should run");

    assert!(output.status.success(), "shell should exit successfully");
    assert!(output.stderr.is_empty(), "shell should not write stderr");

    let events = parse_jsonl(&output.stdout);
    let kinds = event_kinds(&events);
    assert!(kinds.contains(&"tool_call_pending"));
    assert!(kinds.contains(&"artifact_recorded"));
    assert!(kinds.contains(&"tool_call_resolved"));

    let pending = events
        .iter()
        .find(|event| event["kind"]["type"] == "tool_call_pending")
        .expect("shell tool call should be pending before execution");
    assert_eq!(
        pending["kind"]["call"]["arguments"]["argv"],
        serde_json::json!(["rg", "--files"])
    );
    assert_eq!(pending["kind"]["call"]["arguments"]["cwd"], ".");

    let resolved = events
        .iter()
        .find(|event| event["kind"]["type"] == "tool_call_resolved")
        .expect("shell tool call should resolve");
    assert_eq!(resolved["kind"]["result"]["call_id"], "call-shell-command");
    assert_eq!(resolved["kind"]["result"]["status"], "succeeded");
    assert!(resolved["kind"]["result"]["diagnostic"].is_null());
}

#[test]
fn shell_rg_files_prints_process_stdout() {
    let output = merry()
        .args(["shell", "--", "rg", "--files"])
        .output()
        .expect("merry shell should run");

    assert!(output.status.success(), "shell should exit successfully");
    assert!(output.stderr.is_empty(), "shell should not write stderr");

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    assert!(stdout.contains("Cargo.toml"));
    assert!(!stdout.contains("tool_call_pending"));
}

#[test]
fn shell_forbidden_command_denies_without_running_raw_command() {
    let output = merry()
        .args(["shell", "--events-jsonl", "--", "sh", "-c", "echo bad"])
        .output()
        .expect("merry shell should run");

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
        .args([
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
        .args([
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
    assert!(stdout.contains("coding-loop-smoke"));
    assert!(stdout.contains("coding-loop-live-smoke"));
    assert!(stdout.contains("coding-loop-task-smoke"));
    assert!(stdout.contains("coding-loop-task-live-smoke"));
}

#[test]
fn debug_coding_loop_smoke_requires_with_sandbox() {
    let output = merry_without_openai_env()
        .args(["debug", "coding-loop-smoke"])
        .output()
        .expect("merry debug coding-loop-smoke should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "usage errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("--with-sandbox"));
    assert!(stderr.contains("coding-loop-smoke"));
    assert!(stderr.contains("Usage: merry debug"));
}

#[test]
fn debug_coding_loop_live_smoke_requires_with_sandbox_before_config_or_network() {
    let output = merry_without_openai_env()
        .args(["debug", "coding-loop-live-smoke"])
        .output()
        .expect("merry debug coding-loop-live-smoke should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "usage errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("--with-sandbox"));
    assert!(stderr.contains("coding-loop-live-smoke"));
    assert!(stderr.contains("Usage: merry debug"));
    assert!(!stderr.contains("MERRY_OPENAI_API_KEY"));
}

#[test]
fn debug_coding_loop_task_smoke_requires_with_sandbox() {
    let output = merry_without_openai_env()
        .args(["debug", "coding-loop-task-smoke"])
        .output()
        .expect("merry debug coding-loop-task-smoke should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "usage errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("--with-sandbox"));
    assert!(stderr.contains("coding-loop-task-smoke"));
    assert!(stderr.contains("Usage: merry debug"));
}

#[test]
fn debug_coding_loop_task_live_smoke_requires_with_sandbox_before_config_or_network() {
    let output = merry_without_openai_env()
        .args(["debug", "coding-loop-task-live-smoke"])
        .output()
        .expect("merry debug coding-loop-task-live-smoke should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "usage errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("--with-sandbox"));
    assert!(stderr.contains("coding-loop-task-live-smoke"));
    assert!(stderr.contains("Usage: merry debug"));
    assert!(!stderr.contains("MERRY_OPENAI_API_KEY"));
}

#[test]
fn coding_loop_live_smoke_rejects_legacy_config_flag() {
    let output = merry_without_openai_env()
        .args([
            "debug",
            "coding-loop-live-smoke",
            "--config",
            ".merry/secrets/openai.env",
        ])
        .output()
        .expect("merry should run");

    assert_eq!(output.status.code(), Some(2));
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("unexpected argument") || stderr.contains("--config"));
}

#[test]
#[ignore = "requires Linux bubblewrap and local sandbox support"]
fn debug_coding_loop_smoke_runs_inside_real_bwrap_when_opted_in() {
    let mut command = merry_without_openai_env();
    let output = command
        .current_dir(repo_root())
        .args(["--with-sandbox", "debug", "coding-loop-smoke"])
        .output()
        .expect("merry --with-sandbox debug coding-loop-smoke should run");

    assert!(
        output.status.success(),
        "coding-loop smoke should exit successfully: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty(), "smoke should not write stderr");
    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    assert_eq!(stdout, "coding-loop-smoke: ok\n");
}

#[test]
#[ignore = "requires Linux bubblewrap, network access, and XDG OpenAI config"]
fn debug_coding_loop_live_smoke_runs_inside_real_bwrap_when_opted_in() {
    let mut command = merry_without_openai_env();
    let output = command
        .current_dir(repo_root())
        .args(["--with-sandbox", "debug", "coding-loop-live-smoke"])
        .output()
        .expect("merry --with-sandbox debug coding-loop-live-smoke should run");

    assert!(
        output.status.success(),
        "live coding-loop smoke should exit successfully: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "live smoke should not write stderr"
    );
    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    assert_eq!(stdout, "coding-loop-live-smoke: ok\n");
}

#[test]
#[ignore = "requires Linux bubblewrap and local sandbox support"]
fn debug_coding_loop_task_smoke_runs_inside_real_bwrap_when_opted_in() {
    let mut command = merry_without_openai_env();
    let output = command
        .current_dir(repo_root())
        .args(["--with-sandbox", "debug", "coding-loop-task-smoke"])
        .output()
        .expect("merry --with-sandbox debug coding-loop-task-smoke should run");

    assert!(
        output.status.success(),
        "coding-loop task smoke should exit successfully: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "task smoke should not write stderr"
    );
    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    assert_eq!(stdout, "coding-loop-task-smoke: ok\n");
}

#[test]
#[ignore = "requires Linux bubblewrap, network access, and XDG OpenAI config"]
fn debug_coding_loop_task_live_smoke_runs_inside_real_bwrap_when_opted_in() {
    let mut command = merry_without_openai_env();
    let output = command
        .current_dir(repo_root())
        .args(["--with-sandbox", "debug", "coding-loop-task-live-smoke"])
        .output()
        .expect("merry --with-sandbox debug coding-loop-task-live-smoke should run");

    assert!(
        output.status.success(),
        "live coding-loop task smoke should exit successfully: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "task live smoke should not write stderr"
    );
    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    assert_eq!(stdout, "coding-loop-task-live-smoke: ok\n");
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
    assert!(stdout.contains("XDG_CONFIG_HOME"));
    assert!(stdout.contains("config.toml"));
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

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "usage errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("providers.openai-compatible must set api_key_env or api_key_file"));
    assert!(stderr.contains("Usage: merry debug openai"));
}

#[test]
fn debug_openai_rejects_blank_configured_api_key_env_when_opted_in() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    write_xdg_config(
        &temp,
        r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"

[providers.openai-compatible]
api_key_env = "MERRY_OPENAI_API_KEY"
"#,
    );

    let output = merry_without_openai_env_and_xdg(&temp)
        .env("MERRY_OPENAI_DEBUG", "1")
        .env("MERRY_OPENAI_API_KEY", "  ")
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
