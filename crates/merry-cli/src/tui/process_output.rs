use crate::tui::state::ProcessOutputPreview;
use merry_runtime::ArtifactContent;
use serde::Deserialize;
use serde_json::Value;

/// A normalized captured process stream.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct CapturedStream {
    #[serde(default)]
    pub(crate) text: String,
    #[serde(default)]
    pub(crate) truncated: bool,
    #[serde(default)]
    pub(crate) utf8: Option<bool>,
}

#[derive(Deserialize)]
#[serde(tag = "kind")]
enum ProcessOutputWire {
    #[serde(rename = "process_action")]
    Process {
        #[serde(default)]
        stdout: CapturedStream,
        #[serde(default)]
        stderr: CapturedStream,
    },
}

/// Normalized captured text. Unknown artifact formats remain inspectable as raw text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CapturedOutput {
    Process {
        stdout: CapturedStream,
        stderr: CapturedStream,
    },
    Text(String),
}

impl CapturedOutput {
    pub(crate) fn from_artifact(content: ArtifactContent) -> Result<Self, String> {
        let text = match content {
            ArtifactContent::Text { content } | ArtifactContent::Json { content } => content,
            _ => {
                return Err(
                    "This artifact contains binary data, not a text command output.".into(),
                );
            }
        };
        Ok(match serde_json::from_str::<ProcessOutputWire>(&text) {
            Ok(ProcessOutputWire::Process { stdout, stderr }) => Self::Process { stdout, stderr },
            Err(_) => Self::Text(text),
        })
    }

    /// Copies captured text, preserving whitespace and separating stdout from stderr.
    pub(crate) fn copy_text(&self) -> String {
        match self {
            Self::Process { stdout, stderr } => {
                let mut text = stdout.text.clone();
                if !text.is_empty() && !text.ends_with('\n') && !stderr.text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&stderr.text);
                text
            }
            Self::Text(text) => text.clone(),
        }
    }
}

pub(crate) fn process_output_preview(output: &str) -> Option<ProcessOutputPreview> {
    let value = serde_json::from_str::<Value>(output).ok()?;
    if value.get("kind").and_then(Value::as_str) != Some("process_action") {
        return None;
    }
    let stdout = value
        .pointer("/stdout/text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let stderr = value
        .pointer("/stderr/text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let truncated = value.pointer("/stdout/truncated").and_then(Value::as_bool) == Some(true)
        || value.pointer("/stderr/truncated").and_then(Value::as_bool) == Some(true);
    Some(ProcessOutputPreview::new(stdout, stderr, truncated))
}

pub(crate) fn process_exit_code(output: &str) -> Option<i64> {
    let value = serde_json::from_str::<Value>(output).ok()?;
    process_exit_code_from_value(&value)
}

pub(crate) fn process_exit_code_from_value(value: &Value) -> Option<i64> {
    if value.get("kind").and_then(Value::as_str) != Some("process_action") {
        return None;
    }
    value.get("status").and_then(Value::as_i64).or_else(|| {
        value
            .pointer("/status/kind")
            .and_then(Value::as_str)
            .filter(|kind| *kind == "exited")
            .and_then(|_| value.pointer("/status/code").and_then(Value::as_i64))
    })
}
