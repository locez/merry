use crate::tui::input::TextInput;
use merry_core::{QueuedInputsView, ToolCallId};
use std::time::Duration;
use tokio::time::Instant;

const TOOL_PREVIEW_MAX_LINES: usize = 5;
const LIVE_OUTPUT_MAX_LINES: usize = 8;
const TOOL_PREVIEW_MAX_LINE_CHARS: usize = 180;

/// A bounded display preview; complete tool output remains in the runtime transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolOutputPreview {
    pub(crate) lines: Vec<String>,
    pub(crate) truncated: bool,
}

impl ToolOutputPreview {
    /// Keeps five nonempty lines of at most 180 Unicode scalar values each.
    /// Records character, line, or upstream truncation without retaining full output.
    pub(crate) fn new<'a>(lines: impl Iterator<Item = &'a str>, source_truncated: bool) -> Self {
        let mut output_lines = lines.filter(|line| !line.trim().is_empty());
        let mut lines = Vec::new();
        let mut truncated = source_truncated;
        for line in output_lines.by_ref().take(TOOL_PREVIEW_MAX_LINES) {
            let mut characters = line.chars();
            lines.push(
                characters
                    .by_ref()
                    .take(TOOL_PREVIEW_MAX_LINE_CHARS)
                    .collect(),
            );
            truncated |= characters.next().is_some();
        }
        Self {
            lines,
            truncated: truncated || output_lines.next().is_some(),
        }
    }
}

/// Distinguishes cancellation from failures without a nonzero exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandFailure {
    Cancelled,
    Failed,
}

/// Bounded rolling tail of live output while a command is still running.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LiveCommandOutput {
    lines: Vec<String>,
    partial: String,
    older_lines_dropped: bool,
}

impl LiveCommandOutput {
    pub(crate) fn append(&mut self, delta: &str) {
        for ch in delta.chars() {
            if ch == '\n' || ch == '\r' {
                self.flush_partial();
            } else if (!ch.is_control() || ch == '\t')
                && self.partial.chars().count() < TOOL_PREVIEW_MAX_LINE_CHARS
            {
                self.partial.push(ch);
            }
        }
    }

    pub(crate) fn has_output(&self) -> bool {
        !self.lines.is_empty() || !self.partial.trim().is_empty()
    }

    /// Returns `(latest_lines, older_lines_omitted)` in chronological order so
    /// the newest output always stays at the bottom (matching live Thinking).
    pub(crate) fn latest_tail(&self) -> (Vec<String>, bool) {
        let mut lines = self.lines.clone();
        let mut omitted = self.older_lines_dropped;
        if !self.partial.trim().is_empty() {
            if lines.len() >= LIVE_OUTPUT_MAX_LINES {
                lines.remove(0);
                omitted = true;
            }
            lines.push(self.partial.clone());
        }
        (lines, omitted)
    }

    fn flush_partial(&mut self) {
        let line = std::mem::take(&mut self.partial);
        if line.trim().is_empty() {
            return;
        }
        if self.lines.len() >= LIVE_OUTPUT_MAX_LINES {
            self.lines.remove(0);
            self.older_lines_dropped = true;
        }
        self.lines.push(line);
    }
}

/// Projects command lifecycle events without owning process execution or runtime state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommandView {
    Running {
        detail: String,
        started_at: Instant,
        live_preview: LiveCommandOutput,
        /// Presentation fold. Starts collapsed and opens on click when live output exists.
        expanded: bool,
    },
    Finished {
        detail: String,
        exit_code: Option<i64>,
        failure: Option<CommandFailure>,
        preview: ToolOutputPreview,
        command: String,
        cwd: String,
        call_id: ToolCallId,
        elapsed: Option<Duration>,
        /// Presentation fold. Finished commands start closed and open on click.
        expanded: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueuePreviewItem {
    pub(crate) id: u64,
    pub(crate) text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct QueuePreviewState {
    pub(crate) items: Vec<QueuePreviewItem>,
    pub(crate) paused: bool,
}

impl QueuePreviewState {
    pub(super) fn from_inputs(inputs: QueuedInputsView) -> Self {
        Self {
            items: inputs
                .items
                .into_iter()
                .map(|item| QueuePreviewItem {
                    id: item.id,
                    text: item.text,
                })
                .collect(),
            paused: inputs.paused,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Active inline editor for one pending queue item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingQueueEditor {
    pub(crate) id: u64,
    pub(crate) input: TextInput,
    pub(crate) paused_before_edit: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum PatchLineKind {
    Context,
    Add,
    Remove,
}

/// File operation a completed `apply_patch` change reported.
///
/// Envelopes written before the field existed only described updates, so
/// [`PatchOperationView::Update`] is the default for replayed sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PatchOperationView {
    Add,
    #[default]
    Update,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct PatchLineView {
    pub(crate) kind: PatchLineKind,
    pub(crate) old_line: Option<usize>,
    pub(crate) new_line: Option<usize>,
    pub(crate) text: String,
}

#[allow(dead_code)]
impl PatchLineView {
    pub(crate) fn context(text: impl Into<String>, line: Option<usize>) -> Self {
        Self {
            kind: PatchLineKind::Context,
            old_line: line,
            new_line: line,
            text: text.into(),
        }
    }

    pub(crate) fn add(text: impl Into<String>, new_line: Option<usize>) -> Self {
        Self {
            kind: PatchLineKind::Add,
            old_line: None,
            new_line,
            text: text.into(),
        }
    }

    pub(crate) fn remove(text: impl Into<String>, old_line: Option<usize>) -> Self {
        Self {
            kind: PatchLineKind::Remove,
            old_line,
            new_line: None,
            text: text.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct PatchChangeView {
    pub(crate) path: String,
    pub(crate) operation: PatchOperationView,
    pub(crate) added: usize,
    pub(crate) removed: usize,
    pub(crate) hunks: usize,
    pub(crate) lines_before: Option<usize>,
    pub(crate) lines_after: Option<usize>,
    /// Byte counts are part of every recorded envelope, unlike the line counts
    /// that older sessions predate.
    pub(crate) bytes_before: usize,
    pub(crate) bytes_after: usize,
    pub(crate) lines: Vec<PatchLineView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum TimelineItem {
    User {
        text: String,
    },
    Assistant {
        text: String,
    },
    /// Live model reasoning. `elapsed` is set when the turn leaves thinking.
    Thinking {
        text: String,
        started_at: Instant,
        elapsed: Option<Duration>,
        expanded: bool,
    },
    Muted {
        title: String,
        detail: String,
    },
    Command {
        view: CommandView,
    },
    /// Active non-process tool call (`read_file`, `apply_patch`, `MCP`, etc.).
    /// Shares the same spinner, soft lavender-pink flow shimmer, and single-line
    /// collapse behavior as running shell commands.
    ToolRunning {
        title: String,
        detail: String,
        started_at: Instant,
        expanded: bool,
    },
    Read {
        title: String,
        preview: ToolOutputPreview,
        /// Presentation fold. Starts collapsed to one line and opens on click.
        expanded: bool,
    },
    LocalCommand {
        title: String,
        body: String,
    },
    Expanded {
        title: String,
        body: String,
        /// Presentation fold. Starts collapsed to one line and opens on click.
        expanded: bool,
    },
    Diagnostic {
        title: String,
        body: String,
    },
    Patch {
        changes: Vec<PatchChangeView>,
        /// Presentation fold. Starts collapsed to one line per file and opens on click to reveal the colored diff.
        expanded: bool,
    },
    /// Clickable subagent summary card linked to the interactive Subagent Takeover View.
    Subagent {
        agent_id: String,
        task_id: String,
    },
}
