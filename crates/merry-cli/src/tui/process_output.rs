use crate::tui::state::ToolOutputPreview;
use merry_runtime::{ArtifactContent, ProcessOutputEnvelope, ProcessOutputStream};

/// A normalized captured process stream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CapturedStream {
    pub(crate) text: String,
    pub(crate) truncated: bool,
    pub(crate) utf8: Option<bool>,
}

impl From<ProcessOutputStream<'_>> for CapturedStream {
    fn from(stream: ProcessOutputStream<'_>) -> Self {
        Self {
            truncated: stream.truncated(),
            utf8: stream.utf8(),
            text: stream.into_text(),
        }
    }
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
        Ok(
            match serde_json::from_str::<ProcessOutputEnvelope<'_>>(&text) {
                Ok(output) => {
                    let (stdout, stderr) = output.into_streams();
                    Self::Process {
                        stdout: stdout.into(),
                        stderr: stderr.into(),
                    }
                }
                Err(_) => Self::Text(text),
            },
        )
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

pub(crate) fn process_output_preview(output: &str) -> Option<ToolOutputPreview> {
    let output = serde_json::from_str::<ProcessOutputEnvelope<'_>>(output).ok()?;
    let stdout = output.stdout();
    let stderr = output.stderr();
    Some(ToolOutputPreview::new(
        stdout.text().lines().chain(stderr.text().lines()),
        stdout.truncated() || stderr.truncated(),
    ))
}

pub(crate) fn process_exit_code(output: &str) -> Option<i64> {
    serde_json::from_str::<ProcessOutputEnvelope<'_>>(output)
        .ok()?
        .exit_code()
}
