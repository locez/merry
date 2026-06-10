#![allow(dead_code)]

use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

pub fn merry() -> Command {
    static COMMAND_COUNTER: AtomicUsize = AtomicUsize::new(0);
    let sequence = COMMAND_COUNTER.fetch_add(1, Ordering::SeqCst);
    let xdg_root =
        std::env::temp_dir().join(format!("merry-cli-test-{}-{sequence}", std::process::id()));
    let mut command = Command::new(env!("CARGO_BIN_EXE_merry"));
    command
        .env("XDG_CONFIG_HOME", xdg_root.join("config"))
        .env("XDG_STATE_HOME", xdg_root.join("state"));
    command
}

pub fn merry_without_openai_env() -> Command {
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

pub fn merry_without_openai_env_and_xdg(temp: &tempfile::TempDir) -> Command {
    let mut command = merry_without_openai_env();
    command
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .env("XDG_STATE_HOME", temp.path().join("state"));
    command
}

pub fn write_xdg_config(temp: &tempfile::TempDir, text: &str) {
    let config_dir = temp.path().join("config/merry");
    fs::create_dir_all(&config_dir).expect("config dir should be created");
    fs::write(config_dir.join("config.toml"), text).expect("config should write");
}

pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("merry-cli lives under crates/merry-cli")
        .to_path_buf()
}

pub fn assert_debug_output(stdout: &[u8], expected_session_id: &str) {
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
        assert_eq!(event["payload"]["type"], expected_kinds[index]);
    }
}

pub fn parse_jsonl(stdout: &[u8]) -> Vec<Value> {
    let text = std::str::from_utf8(stdout).expect("stdout should be utf-8");
    assert!(text.ends_with('\n'), "stdout should end with a newline");
    text.lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("each line should be JSON"))
        .collect()
}

pub fn event_kinds(events: &[Value]) -> Vec<&str> {
    events
        .iter()
        .map(|event| {
            event["payload"]["type"]
                .as_str()
                .expect("event kind type should be a string")
        })
        .collect()
}
