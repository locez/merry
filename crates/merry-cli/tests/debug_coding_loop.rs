mod support;

use serde_json::Value;
use support::{merry_without_openai_env, repo_root};

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
fn debug_permission_network_smoke_requires_with_sandbox() {
    let output = merry_without_openai_env()
        .args(["debug", "permission-network-smoke"])
        .output()
        .expect("merry debug permission-network-smoke should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "usage errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("--with-sandbox"));
    assert!(stderr.contains("permission-network-smoke"));
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
fn debug_coding_loop_subagent_live_smoke_requires_with_sandbox_before_config_or_network() {
    let output = merry_without_openai_env()
        .args(["debug", "coding-loop-subagent-live-smoke"])
        .output()
        .expect("merry debug coding-loop-subagent-live-smoke should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "usage errors should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("--with-sandbox"));
    assert!(stderr.contains("coding-loop-subagent-live-smoke"));
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

// Real-bwrap smokes are host-shell checks. Inside Codex or another outer
// sandbox, nested bwrap mount/namespace setup can fail for expected
// environment reasons; reproduce from an unsandboxed shell before debugging as
// a product regression.
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
fn debug_permission_network_smoke_runs_inside_real_bwrap_when_opted_in() {
    let mut command = merry_without_openai_env();
    let output = command
        .current_dir(repo_root())
        .args(["--with-sandbox", "debug", "permission-network-smoke"])
        .output()
        .expect("merry --with-sandbox debug permission-network-smoke should run");

    assert!(
        output.status.success(),
        "permission network smoke should exit successfully: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "permission network smoke should not write stderr"
    );
    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    let mut lines = stdout.lines();
    assert_eq!(lines.next(), Some("permission-network-smoke: ok"));
    let events = lines
        .map(|line| serde_json::from_str::<Value>(line).expect("line should be JSON"))
        .collect::<Vec<_>>();
    assert!(
        events.iter().any(|event| {
            event.pointer("/payload/type").and_then(Value::as_str) == Some("tool_call_pending")
                && event.pointer("/payload/call/name").and_then(Value::as_str)
                    == Some("request_permissions")
                && event
                    .pointer("/payload/call/arguments/requested/network")
                    .and_then(Value::as_bool)
                    == Some(true)
        }),
        "stdout should include the network permission request tool call"
    );
    assert!(
        events.iter().any(|event| {
            event.pointer("/type").and_then(Value::as_str) == Some("process_artifact_preview")
                && event.pointer("/status").and_then(Value::as_str) == Some("succeeded")
                && event
                    .pointer("/content")
                    .and_then(Value::as_str)
                    .is_some_and(|content| {
                        content.contains("\"kind\":\"process_action\"")
                            && content.contains("\"argv\":[\"getent\",\"hosts\",\"example.com\"]")
                    })
        }),
        "stdout should include the approved successful process artifact preview"
    );
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
    let mut lines = stdout.lines();
    assert_eq!(lines.next(), Some("coding-loop-task-live-smoke: ok"));
    let events = lines
        .map(|line| serde_json::from_str::<Value>(line).expect("line should be JSON"))
        .collect::<Vec<_>>();
    assert!(
        events.iter().any(|event| {
            event.pointer("/payload/type").and_then(Value::as_str) == Some("tool_call_pending")
                && event.pointer("/payload/call/name").and_then(Value::as_str)
                    == Some("workspace_patch")
                && event
                    .pointer("/payload/call/arguments/patch")
                    .and_then(Value::as_str)
                    .is_some_and(|patch| patch.contains("*** Begin Workspace Patch"))
        }),
        "stdout should include runtime events with live patch tool arguments"
    );
}

#[test]
#[ignore = "requires Linux bubblewrap, network access, and XDG OpenAI config"]
fn debug_coding_loop_subagent_live_smoke_runs_inside_real_bwrap_when_opted_in() {
    let mut command = merry_without_openai_env();
    let output = command
        .current_dir(repo_root())
        .args(["--with-sandbox", "debug", "coding-loop-subagent-live-smoke"])
        .output()
        .expect("merry --with-sandbox debug coding-loop-subagent-live-smoke should run");

    assert!(
        output.status.success(),
        "live coding-loop subagent smoke should exit successfully: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "subagent live smoke should not write stderr"
    );
    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    let mut lines = stdout.lines();
    assert_eq!(lines.next(), Some("coding-loop-subagent-live-smoke: ok"));
    let events = lines
        .map(|line| serde_json::from_str::<Value>(line).expect("line should be JSON"))
        .collect::<Vec<_>>();
    assert!(
        events.iter().any(|event| {
            event.pointer("/payload/type").and_then(Value::as_str) == Some("tool_call_pending")
                && event.pointer("/payload/call/name").and_then(Value::as_str)
                    == Some("spawn_subagents")
        }),
        "stdout should include the parent spawn_subagents tool call"
    );
    assert!(
        events.iter().any(|event| {
            event.pointer("/payload/type").and_then(Value::as_str) == Some("tool_call_pending")
                && event.pointer("/payload/call/name").and_then(Value::as_str)
                    == Some("wait_subagents")
        }),
        "stdout should include the parent wait_subagents tool call"
    );
    assert!(
        events.iter().any(|event| {
            event.pointer("/type").and_then(Value::as_str) == Some("subagent_live_smoke_fixture")
                && event.pointer("/target_matched").and_then(Value::as_bool) == Some(true)
        }),
        "stdout should include fixture proof that the child-edited file reached the target"
    );
}
