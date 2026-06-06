mod support;

use std::fs;
use support::{
    assert_debug_output, merry, merry_without_openai_env, merry_without_openai_env_and_xdg,
    write_xdg_config,
};

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
