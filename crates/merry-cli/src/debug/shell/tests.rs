use super::*;
use crate::sandbox::os;
use crate::testing::FakeProcessRunner;
use merry_core::ToolCallResultStatus;
use std::{ffi::OsStr, sync::Arc};

#[test]
fn process_action_intent_uses_exact_cli_argv_and_empty_env() {
    let intent = match process_action_intent(vec!["rustc".to_owned(), "--version".to_owned()]) {
        Ok(intent) => intent,
        Err(_) => panic!("shell process intent should be valid"),
    };

    assert_eq!(intent.argv(), ["rustc", "--version"]);
    assert_eq!(intent.cwd(), Some("."));
    assert_eq!(intent.env_policy(), ProcessEnvPolicy::empty());
    assert!(intent.stdin_text().is_none());
    assert_eq!(intent.stdout_limit_bytes(), MAX_PROCESS_OUTPUT_LIMIT_BYTES);
    assert_eq!(intent.stderr_limit_bytes(), MAX_PROCESS_OUTPUT_LIMIT_BYTES);
}

#[test]
fn runtime_admission_requires_accept_handoff_and_exact_sandbox_markers() {
    assert_eq!(
        runtime_admission(
            true,
            Some(SandboxChildHandoff::CliBwrapV1),
            Some(SandboxRuntimeProfile::CliBwrapV1),
            Some(OsStr::new("1")),
            Some(OsStr::new("1")),
        ),
        Some(AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1())
    );

    for (accept, handoff, profile, sandbox, version) in [
        (
            false,
            Some(SandboxChildHandoff::CliBwrapV1),
            Some(SandboxRuntimeProfile::CliBwrapV1),
            Some(os("1")),
            Some(os("1")),
        ),
        (
            true,
            Some(SandboxChildHandoff::CliBwrapV1),
            None,
            Some(os("1")),
            Some(os("1")),
        ),
        (true, None, None, None, None),
        (
            false,
            Some(SandboxChildHandoff::CliBwrapV1),
            None,
            None,
            None,
        ),
        (
            true,
            None,
            Some(SandboxRuntimeProfile::CliBwrapV1),
            Some(os("1")),
            Some(os("1")),
        ),
        (
            false,
            None,
            Some(SandboxRuntimeProfile::CliBwrapV1),
            Some(os("1")),
            Some(os("1")),
        ),
        (
            true,
            Some(SandboxChildHandoff::CliBwrapV1),
            Some(SandboxRuntimeProfile::CliBwrapV1),
            None,
            Some(os("1")),
        ),
        (
            true,
            Some(SandboxChildHandoff::CliBwrapV1),
            Some(SandboxRuntimeProfile::CliBwrapV1),
            Some(os("1")),
            None,
        ),
        (
            true,
            Some(SandboxChildHandoff::CliBwrapV1),
            Some(SandboxRuntimeProfile::CliBwrapV1),
            Some(os("0")),
            Some(os("1")),
        ),
        (
            true,
            Some(SandboxChildHandoff::CliBwrapV1),
            Some(SandboxRuntimeProfile::CliBwrapV1),
            Some(os("1")),
            Some(os("2")),
        ),
        (
            true,
            Some(SandboxChildHandoff::CliBwrapV1),
            Some(SandboxRuntimeProfile::CliBwrapV1),
            Some(os("true")),
            Some(os("1")),
        ),
        (
            true,
            Some(SandboxChildHandoff::CliBwrapV1),
            Some(SandboxRuntimeProfile::CliBwrapV1),
            Some(os("1")),
            Some(os("")),
        ),
    ] {
        assert_eq!(
            runtime_admission(
                accept,
                handoff,
                profile,
                sandbox.as_deref(),
                version.as_deref(),
            ),
            None
        );
    }
}

#[tokio::test]
async fn helper_simulated_sandbox_runs_local_workspace_effect_with_fake_runner() {
    let intent = process_action_intent(vec![
        "cargo".to_owned(),
        "test".to_owned(),
        "-p".to_owned(),
        "merry-runtime".to_owned(),
    ])
    .unwrap_or_else(|_| panic!("shell process intent should be valid"));
    let admission = runtime_admission(
        true,
        Some(SandboxChildHandoff::CliBwrapV1),
        Some(SandboxRuntimeProfile::CliBwrapV1),
        Some(OsStr::new("1")),
        Some(OsStr::new("1")),
    );
    let runner = FakeProcessRunner::succeeding("simulated cargo success\n");
    let mut output = Vec::new();

    run_to_writer(
        intent,
        admission,
        Arc::new(runner.clone()),
        false,
        &mut output,
    )
    .await
    .unwrap_or_else(|_| panic!("accepted local workspace shell command should resolve"));

    assert_eq!(runner.call_count(), 1);
    assert_eq!(
        runner.observed_argv(),
        vec![vec!["cargo", "test", "-p", "merry-runtime"]]
    );
    let text = String::from_utf8(output).expect("output should be utf-8");
    assert_eq!(text, "simulated cargo success\n");
}

#[tokio::test]
async fn helper_simulated_sandbox_marker_still_denies_forbidden_command() {
    let intent = process_action_intent(vec![
        "sh".to_owned(),
        "-c".to_owned(),
        "echo bad".to_owned(),
    ])
    .unwrap_or_else(|_| panic!("shell process intent should be valid"));
    let admission = runtime_admission(
        true,
        Some(SandboxChildHandoff::CliBwrapV1),
        Some(SandboxRuntimeProfile::CliBwrapV1),
        Some(OsStr::new("1")),
        Some(OsStr::new("1")),
    );
    let runner = FakeProcessRunner::succeeding("bad\n");
    let mut output = Vec::new();

    run_to_writer(
        intent,
        admission,
        Arc::new(runner.clone()),
        true,
        &mut output,
    )
    .await
    .unwrap_or_else(|_| panic!("forbidden command should resolve as a policy denial"));

    assert_eq!(runner.call_count(), 0);
    let text = String::from_utf8(output).expect("output should be utf-8");
    let events = parse_runtime_events(&text);
    let resolved = resolved_tool_result(&events);
    assert_eq!(resolved.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        resolved
            .diagnostic()
            .expect("denied result should include a diagnostic")
            .code(),
        "action_policy_denied"
    );
}

fn parse_runtime_events(text: &str) -> Vec<RuntimeEvent> {
    assert!(
        text.ends_with('\n'),
        "runtime JSONL should end with newline"
    );
    text.lines()
        .map(|line| serde_json::from_str::<RuntimeEvent>(line).expect("line should be JSON"))
        .collect()
}

fn resolved_tool_result(events: &[RuntimeEvent]) -> &merry_core::ToolCallResult {
    events
        .iter()
        .find_map(|event| match &event.kind {
            RuntimeEventKind::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .expect("shell command should resolve a tool call")
}
