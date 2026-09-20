use crate::{ProcessExitStatus, ProcessRunnerOutput};

pub(super) fn model_text(output: &ProcessRunnerOutput) -> Option<String> {
    if !output.stdout_is_utf8() || !output.stderr_is_utf8() {
        return None;
    }
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
    );
    append_stream(
        &mut text,
        "stderr",
        output.stderr_text(),
        output.stderr_truncated(),
    );
    if output.stdout_truncated() || output.stderr_truncated() {
        append_section(
            &mut text,
            "guidance",
            "Output is incomplete; inspect a narrower range before drawing conclusions. Do not repeat a side-effecting command just to recover output.",
        );
    }
    match output.status() {
        ProcessExitStatus::Cancelled | ProcessExitStatus::DomainFailed => {
            append_section(
                &mut text,
                "guidance",
                "Execution may have had side effects; verify its state before retrying.",
            );
        }
        ProcessExitStatus::FailedToStart => {
            append_section(
                &mut text,
                "guidance",
                "The command did not start; check the error before retrying.",
            );
        }
        ProcessExitStatus::Exited(_) => {}
    }
    Some(text)
}

fn append_stream(text: &mut String, name: &str, content: &str, truncated: bool) {
    if content.is_empty() && !truncated {
        return;
    }
    if truncated {
        append_section(text, &format!("{name} (truncated)"), content);
    } else {
        append_section(text, name, content);
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
        assert_eq!(
            model_text(&output).as_deref(),
            Some("exit 0\nstdout:\n  first\r\nsecond\n")
        );
        let empty = ProcessRunnerOutput::new(
            &intent(),
            ProcessExitStatus::Exited(0),
            "",
            false,
            "",
            false,
        )
        .expect("output");
        assert_eq!(model_text(&empty).as_deref(), Some("exit 0"));
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
            model_text(&output).as_deref(),
            Some("exit 2\nstdout:\npartial\nstderr:\ninvalid syntax\n")
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
            let text = model_text(&output).expect("text output");
            assert_eq!(text.contains("stdout (truncated)"), stdout_truncated);
            assert_eq!(text.contains("stderr (truncated)"), stderr_truncated);
            assert!(text.contains("Do not repeat a side-effecting command"));
        }
    }

    #[test]
    fn non_utf8_streams_keep_the_full_lossless_result_representation() {
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
            assert_eq!(model_text(&output), None);
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
            let text = model_text(&output).expect("model body");
            assert!(text.contains("stderr:\nerror"));
            match status {
                ProcessExitStatus::FailedToStart => assert!(text.contains("did not start")),
                _ => assert!(text.contains("verify its state before retrying")),
            }
        }
    }
}
