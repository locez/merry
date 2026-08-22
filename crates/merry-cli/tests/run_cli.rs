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
