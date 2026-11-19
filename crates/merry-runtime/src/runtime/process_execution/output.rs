use crate::{
    ProcessActionIntent, ProcessExitStatus, ProcessOutputEnvelope, ProcessPermissionProfileId,
    ProcessRunnerOutput, permission::PermissionAdmissionReview, process::shell_process_input,
    tool::ToolResultContent,
};
use merry_core::ToolOutput;
use serde::Serialize;

const TRUNCATION_GUIDANCE: &str =
    "Output is incomplete; inspect a narrower range before drawing conclusions.";
const NON_UTF8_GUIDANCE: &str = "Non-UTF-8 bytes are shown as �; inspect the source with xxd or a format-aware tool for exact bytes.";
const OUTPUT_RECOVERY_GUIDANCE: &str = "Do not repeat a side-effecting command to recover output.";
const STILL_RUNNING_GUIDANCE: &str = "The process is still running, so this turn can continue. Ending this tool call does not stop it. Use read_background_process with this id for later output and stop_background_process to stop it. Merry keeps it until the session ends or that stop.";

pub(super) fn result_content(
    intent: &ProcessActionIntent,
    output: &ProcessRunnerOutput,
    permission_profile_id: ProcessPermissionProfileId,
    permission_review: Option<&PermissionAdmissionReview>,
) -> ToolResultContent {
    ToolResultContent::from(full_output(
        intent,
        output,
        permission_profile_id,
        permission_review,
    ))
    .with_model(ToolOutput::text(model_text(output)))
}

fn model_text(output: &ProcessRunnerOutput) -> String {
    let stdout_utf8 = output.stdout_is_utf8();
    let stderr_utf8 = output.stderr_is_utf8();
    let mut text = match output.status() {
        ProcessExitStatus::Exited(code) => format!("exit {code}"),
        ProcessExitStatus::Cancelled => "cancelled".to_owned(),
        ProcessExitStatus::FailedToStart => "not started".to_owned(),
        ProcessExitStatus::DomainFailed => "failed before normal exit".to_owned(),
        ProcessExitStatus::Running => {
            match (output.running_pid(), output.background_process_id()) {
                (Some(pid), Some(id)) => format!("still running pid {pid} id {id}"),
                (Some(pid), None) => format!("still running pid {pid}"),
                (None, Some(id)) => format!("still running id {id}"),
                (None, None) => "still running".to_owned(),
            }
        }
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
        ProcessExitStatus::Running => Some(STILL_RUNNING_GUIDANCE),
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

fn full_output(
    intent: &ProcessActionIntent,
    output: &ProcessRunnerOutput,
    permission_profile_id: ProcessPermissionProfileId,
    permission_review: Option<&PermissionAdmissionReview>,
) -> ToolOutput {
    let shell_input = shell_process_input(intent);
    let recovery = recovery_guidance(output.status());
    let truncated = output.stdout_truncated() || output.stderr_truncated();
    let truncation = truncated.then(|| ProcessTruncationRecord {
        kind: "process_output_truncated",
        message: format!("{TRUNCATION_GUIDANCE} {OUTPUT_RECOVERY_GUIDANCE}"),
        stdout_truncated: output.stdout_truncated(),
        stderr_truncated: output.stderr_truncated(),
    });
    // A still-running process can carry recovery text and a truncation note.
    // Recovery keeps the `guidance` field; truncation moves beside it.
    let (guidance, output_guidance) = match (recovery, truncation) {
        (Some(message), truncation) => (
            Some(ProcessGuidanceRecord::Recovery {
                kind: "process_action_recovery",
                message,
            }),
            truncation,
        ),
        (None, Some(truncation)) => (Some(ProcessGuidanceRecord::Truncated(truncation)), None),
        (None, None) => (None, None),
    };
    let document = ProcessActionDocument {
        envelope: ProcessOutputEnvelope::new(output, permission_profile_id, permission_review),
        intent: match shell_input {
            Some(shell_input) => ProcessIntentRecord {
                summary: intent.summary(),
                command: Some(shell_input.script()),
                argv: None,
                cwd: intent.cwd(),
            },
            None => ProcessIntentRecord {
                summary: intent.summary(),
                command: None,
                argv: Some(intent.argv()),
                cwd: intent.cwd(),
            },
        },
        guidance,
        output_guidance,
        input_evidence: shell_input.map(|shell_input| ShellInputEvidence {
            kind: "shell_command_script",
            shell: shell_input.shell(),
            flag: shell_input.flag(),
            script: shell_input.script(),
            script_bytes: shell_input.script_bytes(),
            script_fingerprint: shell_input.script_fingerprint(),
        }),
    };
    match serde_json::to_string(&document) {
        Ok(json) => ToolOutput::json(json),
        Err(_) => ToolOutput::json(
            r#"{"ok":false,"kind":"process_action","error":"process result could not be encoded"}"#,
        ),
    }
}

#[derive(Serialize)]
struct ProcessActionDocument<'a> {
    #[serde(flatten)]
    envelope: ProcessOutputEnvelope<'a>,
    intent: ProcessIntentRecord<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    guidance: Option<ProcessGuidanceRecord<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_guidance: Option<ProcessTruncationRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_evidence: Option<ShellInputEvidence<'a>>,
}

#[derive(Serialize)]
struct ProcessIntentRecord<'a> {
    summary: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    argv: Option<&'a [String]>,
    cwd: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum ProcessGuidanceRecord<'a> {
    Recovery {
        kind: &'static str,
        message: &'a str,
    },
    Truncated(ProcessTruncationRecord),
}

#[derive(Serialize)]
struct ProcessTruncationRecord {
    kind: &'static str,
    message: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

#[derive(Serialize)]
struct ShellInputEvidence<'a> {
    kind: &'static str,
    shell: &'a str,
    flag: &'a str,
    script: &'a str,
    script_bytes: usize,
    script_fingerprint: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BackgroundProcessId, ProcessActionIntent, ProcessEnvPolicy};

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
    fn running_process_names_its_pid_and_keeps_the_turn_free() {
        let output = ProcessRunnerOutput::new(
            &intent(),
            ProcessExitStatus::Running,
            "listening\n",
            false,
            "",
            false,
        )
        .expect("output")
        .with_running_pid(42)
        .with_background_process_id(
            BackgroundProcessId::new("bg-1").expect("background process id"),
        );
        assert!(output.ok());
        assert_eq!(output.running_pid(), Some(42));
        assert_eq!(
            output
                .background_process_id()
                .map(BackgroundProcessId::as_str),
            Some("bg-1")
        );
        assert_eq!(
            model_text(&output),
            format!(
                "still running pid 42 id bg-1\nstdout:\nlistening\nguidance:\n{STILL_RUNNING_GUIDANCE}"
            )
        );
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
                ProcessExitStatus::Running => {
                    assert!(text.contains("still running"));
                    assert!(text.contains("does not stop it"));
                }
                _ => assert!(text.contains("verify its state before retrying")),
            }
        }
    }
}
