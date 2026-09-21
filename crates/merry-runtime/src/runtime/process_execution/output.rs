use crate::{
    ArtifactContent, ProcessActionIntent, ProcessExitStatus, ProcessOutputEnvelope,
    ProcessPermissionProfileId, ProcessRunnerOutput, permission::PermissionAdmissionReview,
    process::shell_process_input, tool::ToolResultContent,
};
use merry_core::{ArtifactKind, ArtifactRef};

const TRUNCATION_GUIDANCE: &str =
    "Output is incomplete; inspect a narrower range before drawing conclusions.";
const NON_UTF8_GUIDANCE: &str = "Non-UTF-8 bytes are shown as �; inspect the source with xxd or a format-aware tool for exact bytes.";
const OUTPUT_RECOVERY_GUIDANCE: &str = "Do not repeat a side-effecting command to recover output.";

pub(super) fn result_content(
    intent: &ProcessActionIntent,
    output: &ProcessRunnerOutput,
    permission_profile_id: ProcessPermissionProfileId,
    input_artifact: Option<&ArtifactRef>,
    permission_review: Option<&PermissionAdmissionReview>,
) -> ToolResultContent {
    ToolResultContent::from(artifact_content(
        intent,
        output,
        permission_profile_id,
        input_artifact,
        permission_review,
    ))
    .with_model(ArtifactContent::text(model_text(output)))
}

fn model_text(output: &ProcessRunnerOutput) -> String {
    let stdout_utf8 = output.stdout_is_utf8();
    let stderr_utf8 = output.stderr_is_utf8();
    let mut text = match output.status() {
        ProcessExitStatus::Exited(code) => format!("exit {code}"),
        ProcessExitStatus::Cancelled => "cancelled".to_owned(),
        ProcessExitStatus::FailedToStart => "not started".to_owned(),
        ProcessExitStatus::DomainFailed => "failed before normal exit".to_owned(),
    };
    append_stream(
        &mut text,
        "stdout",
        output.stdout_text(),
        output.stdout_truncated(),
        stdout_utf8,
    );
    append_stream(
        &mut text,
        "stderr",
        output.stderr_text(),
        output.stderr_truncated(),
        stderr_utf8,
    );
    let mut guidance = Vec::with_capacity(4);
    let non_utf8 = !stdout_utf8 || !stderr_utf8;
    let truncated = output.stdout_truncated() || output.stderr_truncated();
    if non_utf8 {
        guidance.push(NON_UTF8_GUIDANCE);
    }
    if truncated {
        guidance.push(TRUNCATION_GUIDANCE);
    }
    if non_utf8 || truncated {
        guidance.push(OUTPUT_RECOVERY_GUIDANCE);
    }
    if let Some(recovery) = recovery_guidance(output.status()) {
        guidance.push(recovery);
    }
    if !guidance.is_empty() {
        append_section(&mut text, "guidance", &guidance.join(" "));
    }
    text
}

fn recovery_guidance(status: ProcessExitStatus) -> Option<&'static str> {
    match status {
        ProcessExitStatus::Cancelled | ProcessExitStatus::DomainFailed => {
            Some("Execution may have had side effects; verify its state before retrying.")
        }
        ProcessExitStatus::FailedToStart => {
            Some("The command did not start; check the error before retrying.")
        }
        ProcessExitStatus::Exited(_) => None,
    }
}

fn append_stream(text: &mut String, name: &str, content: &str, truncated: bool, utf8: bool) {
    if content.is_empty() && !truncated {
        return;
    }
    match (truncated, utf8) {
        (false, true) => append_section(text, name, content),
        (true, true) => append_section(text, &format!("{name} (truncated)"), content),
        (false, false) => append_section(text, &format!("{name} (non-UTF-8)"), content),
        (true, false) => append_section(text, &format!("{name} (truncated, non-UTF-8)"), content),
    }
}

fn append_section(text: &mut String, name: &str, content: &str) {
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(name);
    text.push_str(":\n");
    text.push_str(content);
}

fn artifact_content(
    intent: &ProcessActionIntent,
    output: &ProcessRunnerOutput,
    permission_profile_id: ProcessPermissionProfileId,
    input_artifact: Option<&ArtifactRef>,
    permission_review: Option<&PermissionAdmissionReview>,
) -> ArtifactContent {
    let shell_input = shell_process_input(intent);
    let intent_payload = if let Some(shell_input) = shell_input {
        serde_json::json!({
            "summary": intent.summary(),
            "command": shell_input.script(),
            "cwd": intent.cwd(),
        })
    } else {
        serde_json::json!({
            "summary": intent.summary(),
            "argv": intent.argv(),
            "cwd": intent.cwd(),
        })
    };

    let mut payload = serde_json::json!(ProcessOutputEnvelope::new(
        output,
        permission_profile_id,
        permission_review,
    ));
    payload["intent"] = intent_payload;

    if let Some(guidance) = recovery_guidance(output.status()) {
        payload["guidance"] = serde_json::json!({
            "kind": "process_action_recovery",
            "message": guidance,
        });
    }

    if output.stdout_truncated() || output.stderr_truncated() {
        let key = if output.ok() {
            "guidance"
        } else {
            "output_guidance"
        };
        payload[key] = serde_json::json!({
            "kind": "process_output_truncated",
            "message": format!("{TRUNCATION_GUIDANCE} {OUTPUT_RECOVERY_GUIDANCE}"),
            "stdout_truncated": output.stdout_truncated(),
            "stderr_truncated": output.stderr_truncated(),
        });
    }

    if let Some(input_artifact) = input_artifact {
        payload["input_artifact"] = artifact_ref_json(input_artifact);
    } else if let Some(shell_input) = shell_input {
        payload["input_evidence"] = serde_json::json!({
            "kind": "shell_command_script",
            "shell": shell_input.shell(),
            "flag": shell_input.flag(),
            "script": shell_input.script(),
            "script_bytes": shell_input.script_bytes(),
            "script_fingerprint": shell_input.script_fingerprint(),
        });
    }

    ArtifactContent::json(payload.to_string())
}

fn artifact_ref_json(artifact: &ArtifactRef) -> serde_json::Value {
    let kind = match artifact.kind() {
        ArtifactKind::Text => "text",
        ArtifactKind::Json => "json",
        ArtifactKind::Binary => "binary",
        ArtifactKind::Image => "image",
        ArtifactKind::Other => "other",
    };
    serde_json::json!({ "id": artifact.id().as_str(), "kind": kind })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ProcessActionIntent, ProcessEnvPolicy};

    fn intent() -> ProcessActionIntent {
        ProcessActionIntent::new(
            vec!["test".to_owned()],
            None,
            ProcessEnvPolicy::empty(),
            None,
            4096,
            4096,
        )
        .expect("valid intent")
    }

    #[test]
    fn model_result_preserves_output_without_repeating_execution_input() {
        let output = ProcessRunnerOutput::new(
            &intent(),
            ProcessExitStatus::Exited(0),
            "  first\r\nsecond\n",
            false,
            "",
            false,
        )
        .expect("output");
        assert_eq!(model_text(&output), "exit 0\nstdout:\n  first\r\nsecond\n");
        let empty = ProcessRunnerOutput::new(
            &intent(),
            ProcessExitStatus::Exited(0),
            "",
            false,
            "",
            false,
        )
        .expect("output");
        assert_eq!(model_text(&empty), "exit 0");
    }

    #[test]
    fn nonzero_exit_keeps_both_streams_without_diagnosing_a_sandbox_failure() {
        let output = ProcessRunnerOutput::new(
            &intent(),
            ProcessExitStatus::Exited(2),
            "partial",
            false,
            "invalid syntax\n",
            false,
        )
        .expect("output");
        assert_eq!(
            model_text(&output),
            "exit 2\nstdout:\npartial\nstderr:\ninvalid syntax\n"
        );
    }

    #[test]
    fn truncation_identifies_each_incomplete_stream_even_without_captured_text() {
        for (stdout_truncated, stderr_truncated) in [(true, false), (false, true), (true, true)] {
            let output = ProcessRunnerOutput::new(
                &intent(),
                ProcessExitStatus::Exited(1),
                "",
                stdout_truncated,
                "",
                stderr_truncated,
            )
            .expect("output");
            let text = model_text(&output);
            assert_eq!(text.contains("stdout (truncated)"), stdout_truncated);
            assert_eq!(text.contains("stderr (truncated)"), stderr_truncated);
            assert!(text.contains("Do not repeat a side-effecting command"));
        }
    }

    #[test]
    fn non_utf8_streams_are_text_with_explicit_lossy_display_guidance() {
        for (stdout, stderr) in [(vec![0xff], vec![]), (vec![], vec![0xff])] {
            let output = ProcessRunnerOutput::from_bytes(
                &intent(),
                ProcessExitStatus::Exited(0),
                stdout,
                false,
                stderr,
                false,
            )
            .expect("binary output");
            let stream = if output.stdout_is_utf8() {
                "stderr"
            } else {
                "stdout"
            };
            assert_eq!(
                model_text(&output),
                format!(
                    "exit 0\n{stream} (non-UTF-8):\n�\nguidance:\n{NON_UTF8_GUIDANCE} {OUTPUT_RECOVERY_GUIDANCE}"
                )
            );
        }
    }

    #[test]
    fn exceptional_completion_distinguishes_not_started_from_possible_side_effects() {
        for status in [
            ProcessExitStatus::FailedToStart,
            ProcessExitStatus::Cancelled,
            ProcessExitStatus::DomainFailed,
        ] {
            let output = ProcessRunnerOutput::new(&intent(), status, "", false, "error", false)
                .expect("output");
            let text = model_text(&output);
            assert!(text.contains("stderr:\nerror"));
            match status {
                ProcessExitStatus::FailedToStart => assert!(text.contains("did not start")),
                _ => assert!(text.contains("verify its state before retrying")),
            }
        }
    }
}
