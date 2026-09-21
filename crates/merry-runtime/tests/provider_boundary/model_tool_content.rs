use crate::support::{
    events::{event_kind_names, pending_tool_call, resolved_tool_result},
    models::{
        ScriptedModelProvider, completed_outputs_event, completed_text_event, model_name,
        model_tool_call, model_tool_call_with_args,
    },
    runtime::{collect_step, runtime_with_registered_tool, session_id},
    tools::{ScriptedToolExecutor, ToolExecutorResponse},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use merry_core::ToolName;
use merry_llm::{FinishReason, ModelOutput, ModelRequest};
use merry_runtime::{
    ArtifactContent, ProcessActionIntent, ProcessExitStatus, ProcessOutputEnvelope, ProcessRunner,
    ProcessRunnerContext, ProcessRunnerError, ProcessRunnerFuture, ProcessRunnerOutput, Runtime,
    ToolExecutionContext, ToolExecutionOutcome, process_command_tool,
};
use serde_json::{Map, Value, json};
use std::sync::Arc;

#[tokio::test]
async fn provider_replays_only_model_body_while_artifact_remains_exact() {
    let full = "display-only details ".repeat(300);
    let body = "found: exact result\n";
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("used result"))],
        vec![Ok(completed_text_event("still using result"))],
    ]);
    let executor = ScriptedToolExecutor::new(ToolExecutorResponse::Outcome(
        ToolExecutionOutcome::succeeded_text(&full).with_model_text(body),
    ));
    let runtime = runtime_with_registered_tool("model-body-replay", provider.clone(), executor);
    let events = collect_step(&runtime, "lookup").await;
    let pending = pending_tool_call(&events);
    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("execute");
    assert_eq!(
        event_kind_names(&events),
        ["ArtifactRecorded", "ArtifactRecorded", "ToolCallResolved"]
    );
    let result = resolved_tool_result(&events);
    assert_eq!(
        runtime
            .read_artifact_content(result.artifact().id())
            .await
            .expect("full artifact"),
        ArtifactContent::text(&full)
    );
    collect_step(&runtime, "continue").await;
    collect_step(&runtime, "continue again").await;
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    for request in &requests[1..] {
        let result = request
            .continuations()
            .first()
            .expect("continuation")
            .result();
        assert_eq!(result.content().as_text(), Some(body));
        assert!(result.content().as_str().len() * 10 < full.len());
        assert_eq!(request.tools(), requests[0].tools());
        assert_eq!(
            request.stable_prefix_hash(),
            requests[0].stable_prefix_hash()
        );
    }
}

struct OutputProcessRunner {
    status: ProcessExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    truncated: bool,
}

impl OutputProcessRunner {
    fn new(status: ProcessExitStatus, stdout: &[u8], stderr: &[u8], truncated: bool) -> Self {
        Self {
            status,
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
            truncated,
        }
    }
}

impl ProcessRunner for OutputProcessRunner {
    fn run<'a>(
        &'a self,
        intent: ProcessActionIntent,
        context: ProcessRunnerContext,
    ) -> ProcessRunnerFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ProcessRunnerError::Cancelled);
            }
            ProcessRunnerOutput::from_bytes(
                &intent,
                self.status,
                self.stdout.clone(),
                self.truncated,
                self.stderr.clone(),
                self.truncated,
            )
            .map_err(|error| ProcessRunnerError::infrastructure(error.to_string()))
        })
    }
}

async fn process_requests(runner: OutputProcessRunner) -> (ArtifactContent, Vec<ModelRequest>) {
    let call = model_tool_call_with_args(
        "call-process",
        "run_process",
        Map::from_iter([("command".to_owned(), json!("cat fixture.bin"))]),
    );
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("inspected output"))],
        vec![Ok(completed_text_event("output remains available"))],
    ]);
    let runtime = Runtime::builder(session_id("process-result-body"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .register_tool(
            process_command_tool(ToolName::new("run_process").expect("name"), "Run a command")
                .expect("process tool"),
        )
        .allow_read_only_shell_process_actions(Arc::new(runner))
        .build()
        .expect("runtime");
    let events = collect_step(&runtime, "inspect the file").await;
    let pending = pending_tool_call(&events);
    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("execute");
    let result = resolved_tool_result(&events);
    let full = runtime
        .read_artifact_content(result.artifact().id())
        .await
        .expect("full artifact");
    collect_step(&runtime, "continue").await;
    collect_step(&runtime, "continue again").await;
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    for request in &requests[1..] {
        assert_eq!(request.tools(), requests[0].tools());
        assert_eq!(
            request.stable_prefix_hash(),
            requests[0].stable_prefix_hash()
        );
    }
    (full, requests)
}

#[tokio::test]
async fn process_text_replays_without_rewriting_whitespace_controls_or_structured_output() {
    let stdout = "\t{\"header\":\"7f454c46\",\"text\":\"中文�\"}\r\n\0\n";
    let stderr = "  diagnostic\r\n\n";
    let (full, requests) = process_requests(OutputProcessRunner::new(
        ProcessExitStatus::Exited(2),
        stdout.as_bytes(),
        stderr.as_bytes(),
        false,
    ))
    .await;
    let payload: Value = serde_json::from_str(full.as_text().expect("full JSON")).expect("JSON");
    assert_eq!(payload["stdout"]["text"], stdout);
    assert_eq!(payload["stderr"]["text"], stderr);
    let expected = format!("exit 2\nstdout:\n{stdout}stderr:\n{stderr}");
    for request in &requests[1..] {
        let result = request
            .continuations()
            .first()
            .expect("continuation")
            .result();
        assert_eq!(result.content().as_text(), Some(expected.as_str()));
    }
}

#[tokio::test]
async fn binary_process_results_keep_lossless_artifacts_and_textual_model_bodies() {
    let statuses = [
        (ProcessExitStatus::Exited(0), None),
        (ProcessExitStatus::Exited(2), None),
        (
            ProcessExitStatus::Cancelled,
            Some("Execution may have had side effects; verify its state before retrying."),
        ),
        (
            ProcessExitStatus::DomainFailed,
            Some("Execution may have had side effects; verify its state before retrying."),
        ),
        (
            ProcessExitStatus::FailedToStart,
            Some("The command did not start; check the error before retrying."),
        ),
    ];
    let streams: &[(&[u8], &[u8])] = &[
        (b"\xff\0a", b"diagnostic\n"),
        (b"read file\n", b"\xe4\xb8"),
        (b"\xff\0a", b"\xe4\xb8"),
    ];
    for (status, recovery) in statuses {
        for &(stdout, stderr) in streams {
            for truncated in [false, true] {
                let (full, requests) =
                    process_requests(OutputProcessRunner::new(status, stdout, stderr, truncated))
                        .await;
                let payload: Value = serde_json::from_str(full.as_text().expect("full JSON"))
                    .expect("JSON artifact");
                let envelope: ProcessOutputEnvelope<'_> =
                    serde_json::from_str(full.as_text().expect("full JSON"))
                        .expect("runtime artifact is readable by the shared consumer contract");
                assert_eq!(envelope.exit_code(), status.exit_code().map(i64::from));
                assert_eq!(envelope.stdout().text(), String::from_utf8_lossy(stdout));
                assert_eq!(envelope.stderr().text(), String::from_utf8_lossy(stderr));
                for (name, bytes) in [("stdout", stdout), ("stderr", stderr)] {
                    assert_eq!(payload[name]["bytes"], bytes.len());
                    assert_eq!(payload[name]["truncated"], truncated);
                    match std::str::from_utf8(bytes) {
                        Ok(text) => assert_eq!(payload[name]["text"], text),
                        Err(_) => {
                            assert_eq!(payload[name]["utf8"], false);
                            let encoded = payload[name]["bytes_base64"]
                                .as_str()
                                .expect("lossless bytes");
                            assert_eq!(BASE64.decode(encoded).expect("base64"), bytes);
                        }
                    }
                }
                for request in &requests[1..] {
                    let result = request
                        .continuations()
                        .first()
                        .expect("continuation")
                        .result();
                    let text = result
                        .content()
                        .as_text()
                        .expect("text result, not the full artifact");
                    let status_text = match status {
                        ProcessExitStatus::Exited(code) => format!("exit {code}\n"),
                        ProcessExitStatus::Cancelled => "cancelled\n".to_owned(),
                        ProcessExitStatus::DomainFailed => "failed before normal exit\n".to_owned(),
                        ProcessExitStatus::FailedToStart => "not started\n".to_owned(),
                    };
                    assert!(text.starts_with(&status_text));
                    assert!(!text.contains("bytes_base64"));
                    assert!(!text.contains("permission_profile_id"));
                    assert!(text.contains("Non-UTF-8 bytes are shown as �"));
                    assert!(text.contains("xxd or a format-aware tool"));
                    assert_eq!(text.matches("guidance:\n").count(), 1);
                    assert_eq!(
                        text.matches("Do not repeat a side-effecting command")
                            .count(),
                        1
                    );
                    for (name, bytes) in [("stdout", stdout), ("stderr", stderr)] {
                        let label = match (truncated, std::str::from_utf8(bytes).is_ok()) {
                            (false, true) => name.to_owned(),
                            (true, true) => format!("{name} (truncated)"),
                            (false, false) => format!("{name} (non-UTF-8)"),
                            (true, false) => format!("{name} (truncated, non-UTF-8)"),
                        };
                        assert!(
                            text.contains(&format!("{label}:\n{}", String::from_utf8_lossy(bytes)))
                        );
                    }
                    assert_eq!(text.contains("Output is incomplete"), truncated);
                    if truncated {
                        let key = if status == ProcessExitStatus::Exited(0) {
                            "guidance"
                        } else {
                            "output_guidance"
                        };
                        let warning = payload[key]["message"]
                            .as_str()
                            .expect("truncation warning");
                        assert!(warning.contains("Output is incomplete"));
                        assert!(warning.contains("Do not repeat a side-effecting command"));
                        assert!(text.contains(warning));
                    }
                    if let Some(recovery) = recovery {
                        assert_eq!(payload["guidance"]["message"], recovery);
                        assert!(text.contains(recovery));
                    } else if !truncated || status != ProcessExitStatus::Exited(0) {
                        assert!(payload.get("guidance").is_none());
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn small_and_large_binary_streams_never_replay_the_json_envelope() {
    for size in [32, 4096] {
        let mut bytes = vec![0; size];
        bytes[..5].copy_from_slice(b"\x7fELF\xff");
        for stream in ["stdout", "stderr"] {
            let (stdout, stderr) = if stream == "stdout" {
                (bytes.as_slice(), &[][..])
            } else {
                (&[][..], bytes.as_slice())
            };
            let (full, requests) = process_requests(OutputProcessRunner::new(
                ProcessExitStatus::Exited(0),
                stdout,
                stderr,
                false,
            ))
            .await;
            let payload: Value =
                serde_json::from_str(full.as_text().expect("full JSON")).expect("JSON artifact");
            let encoded = payload[stream]["bytes_base64"]
                .as_str()
                .expect("lossless bytes");
            assert_eq!(BASE64.decode(encoded).expect("base64"), bytes);
            let expected = format!(
                "exit 0\n{stream} (non-UTF-8):\n{}\nguidance:\nNon-UTF-8 bytes are shown as �; inspect the source with xxd or a format-aware tool for exact bytes. Do not repeat a side-effecting command to recover output.",
                String::from_utf8_lossy(&bytes),
            );
            for request in &requests[1..] {
                let result = request
                    .continuations()
                    .first()
                    .expect("continuation")
                    .result();
                assert_eq!(result.content().as_text(), Some(expected.as_str()));
                assert!(result.content().as_str().len() < bytes.len() + 256);
            }
        }
    }
}
