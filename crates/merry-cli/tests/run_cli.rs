mod support;

use std::{io::Write, process::Stdio};
use support::{merry_without_openai_env_and_xdg, write_xdg_config};

/// Config that resolves a provider without reaching one: session selection and
/// resume run before the first model call, so an offline endpoint is enough.
const OFFLINE_PROVIDER_CONFIG: &str = "\
[providers.default]
provider = \"openai-compatible\"
model = \"offline-model\"

[providers.openai-compatible]
type = \"openai-compatible\"
protocol = \"responses\"
base_url = \"http://127.0.0.1:9/v1\"
api_key = \"sk-offline-not-used\"
";

#[test]
fn mcp_outage_warns_on_stderr_but_reaches_the_runtime_and_preserves_jsonl_output() {
    let temp = tempfile::tempdir().unwrap();
    write_xdg_config(
        &temp,
        &format!(
            "{OFFLINE_PROVIDER_CONFIG}\n[providers.retry]\nenabled = false\n[mcp.offline]\nurl = 'http://127.0.0.1:9/mcp'\n"
        ),
    );
    let output = merry_without_openai_env_and_xdg(&temp)
        .args([
            "--no-sandbox",
            "run",
            "--events-jsonl",
            "--session-id",
            "mcp-outage",
            "continue without MCP",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("Warning: MCP offline:"), "{stderr}");
    assert!(stderr.contains("Merry continues"), "{stderr}");
    let events = support::parse_jsonl(&output.stdout);
    assert!(
        events.iter().any(|event| event["type"] == "step_started"),
        "{events:?}"
    );
    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(session_state_path(&temp, "mcp-outage")).unwrap())
            .unwrap();
    assert_eq!(
        state["external_tool_catalog"]["entries"],
        serde_json::json!([])
    );
}

#[test]
fn run_refuses_a_whitespace_only_stdin_task_before_emitting_events() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    write_xdg_config(&temp, OFFLINE_PROVIDER_CONFIG);

    let mut child = merry_without_openai_env_and_xdg(&temp)
        .args(["--no-sandbox", "run", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("merry run should start");
    child
        .stdin
        .take()
        .expect("stdin should be piped")
        .write_all(b"  \n\t")
        .expect("whitespace task should write");

    let output = child.wait_with_output().expect("merry run should finish");

    assert_eq!(
        output.status.code(),
        Some(2),
        "a whitespace-only stdin task should be a usage error"
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr should be utf-8");
    assert!(
        stderr.contains("the task read from stdin is empty"),
        "the refusal should explain that the stdin task was empty: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "a run rejected before startup should not emit runtime events"
    );
}

#[test]
fn run_refuses_a_session_id_that_would_escape_the_session_store() {
    let temp = tempfile::tempdir().expect("tempdir should be created");

    let output = merry_without_openai_env_and_xdg(&temp)
        .args(["--no-sandbox", "run", "--session-id", "../escape", "a task"])
        .output()
        .expect("merry run should run");

    assert_eq!(
        output.status.code(),
        Some(2),
        "an unsafe session id should be a usage error"
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr should be utf-8");
    assert!(
        stderr.contains("--session-id"),
        "the refusal should name the flag that was wrong: {stderr}"
    );
}

#[test]
fn run_refuses_to_both_start_and_resume_a_session() {
    let temp = tempfile::tempdir().expect("tempdir should be created");

    let output = merry_without_openai_env_and_xdg(&temp)
        .args([
            "--no-sandbox",
            "run",
            "--session-id",
            "one",
            "--resume",
            "two",
            "a task",
        ])
        .output()
        .expect("merry run should run");

    assert!(
        !output.status.success(),
        "starting and resuming one run should be refused"
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr should be utf-8");
    assert!(
        stderr.contains("--session-id") && stderr.contains("--resume"),
        "the refusal should name both flags: {stderr}"
    );
}

#[test]
fn run_reports_an_unknown_resume_id_without_starting_a_session() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    write_xdg_config(&temp, OFFLINE_PROVIDER_CONFIG);

    let output = merry_without_openai_env_and_xdg(&temp)
        .args(["--no-sandbox", "run", "--resume", "never-existed", "a task"])
        .output()
        .expect("merry run should run");

    assert!(
        !output.status.success(),
        "resuming a session that was never saved should fail"
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr should be utf-8");
    assert!(
        stderr.contains("could not resume session never-existed"),
        "the failure should name the session that could not be resumed: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "a run that never started should not emit runtime events"
    );
}

/// `state.json` under the store layout `XDG_STATE_HOME` selects.
fn session_state_path(temp: &tempfile::TempDir, session_id: &str) -> std::path::PathBuf {
    temp.path()
        .join("state/merry/sessions")
        .join(session_id)
        .join("state.json")
}

fn session_metadata_path(temp: &tempfile::TempDir, session_id: &str) -> std::path::PathBuf {
    temp.path()
        .join("state/merry/sessions")
        .join(session_id)
        .join("meta.json")
}

#[test]
fn a_headless_run_writes_picker_metadata_with_headless_origin() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    write_xdg_config(&temp, OFFLINE_PROVIDER_CONFIG);

    let output = merry_without_openai_env_and_xdg(&temp)
        .args([
            "--no-sandbox",
            "run",
            "--session-id",
            "picker-visible",
            "a task",
        ])
        .output()
        .expect("merry run should run");
    assert!(
        !output.status.success(),
        "the offline provider should fail the run"
    );

    let metadata_path = session_metadata_path(&temp, "picker-visible");
    let metadata: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&metadata_path).expect("headless metadata should be written"),
    )
    .expect("headless metadata should be valid JSON");
    assert_eq!(metadata["headless"], true);
    assert_eq!(metadata["session_id"], "picker-visible");
}

#[test]
fn run_refuses_a_session_id_that_already_has_saved_state() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    write_xdg_config(&temp, OFFLINE_PROVIDER_CONFIG);
    let state_path = session_state_path(&temp, "taken");
    std::fs::create_dir_all(state_path.parent().expect("state path has a session dir"))
        .expect("session dir should be created");
    std::fs::write(&state_path, b"existing state").expect("existing state should be written");

    let output = merry_without_openai_env_and_xdg(&temp)
        .args(["--no-sandbox", "run", "--session-id", "taken", "a task"])
        .output()
        .expect("merry run should run");

    assert_eq!(
        output.status.code(),
        Some(2),
        "reusing a saved session id should be a usage error"
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr should be utf-8");
    assert!(
        stderr.contains("--resume taken"),
        "the refusal should point at the way to continue the session: {stderr}"
    );
    assert_eq!(
        std::fs::read(&state_path).expect("saved state should still be readable"),
        b"existing state",
        "a refused run must not replace the saved session"
    );
    assert!(
        output.stdout.is_empty(),
        "a run refused before startup should not emit runtime events"
    );
}

#[test]
fn a_saved_run_blocks_reusing_its_session_id_but_not_resuming_it() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    write_xdg_config(&temp, OFFLINE_PROVIDER_CONFIG);

    let first = merry_without_openai_env_and_xdg(&temp)
        .args([
            "--no-sandbox",
            "run",
            "--session-id",
            "saved-once",
            "a task",
        ])
        .output()
        .expect("merry run should run");
    assert!(
        !first.status.success(),
        "the offline provider should fail the first run"
    );
    let state_path = session_state_path(&temp, "saved-once");
    assert!(
        state_path.exists(),
        "a settled run should save its session state"
    );
    let saved = std::fs::read(&state_path).expect("saved state should be readable");

    let reused = merry_without_openai_env_and_xdg(&temp)
        .args([
            "--no-sandbox",
            "run",
            "--session-id",
            "saved-once",
            "another task",
        ])
        .output()
        .expect("merry run should run");
    assert_eq!(
        reused.status.code(),
        Some(2),
        "a second run must not take over the saved session id"
    );
    assert_eq!(
        std::fs::read(&state_path).expect("saved state should still be readable"),
        saved,
        "the refused run must leave the saved session byte-identical"
    );

    let resumed = merry_without_openai_env_and_xdg(&temp)
        .args([
            "--no-sandbox",
            "run",
            "--resume",
            "saved-once",
            "another task",
        ])
        .output()
        .expect("merry run should run");
    let resumed_stderr = String::from_utf8(resumed.stderr).expect("stderr should be utf-8");
    assert!(
        !resumed_stderr.contains("already has saved state"),
        "--resume is the supported way to continue a saved session: {resumed_stderr}"
    );
}
