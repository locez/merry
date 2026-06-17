use super::{
    input::{InputHistory, TextInput},
    keymap::Keymap,
    theme::TuiTheme,
};
use merry_core::{InteractiveRunState, QueuedInputLane, QueuedInputView, SessionUsage};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct QueuePreview {
    pub(crate) next: Vec<QueuedInputView>,
    pub(crate) suspended: Vec<QueuedInputView>,
    pub(crate) backlog: Vec<QueuedInputView>,
}

#[allow(dead_code)]
impl QueuePreview {
    pub(crate) fn empty() -> Self {
        Self {
            next: Vec::new(),
            suspended: Vec::new(),
            backlog: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct QueuePreviewItem {
    pub(crate) text: String,
}

#[allow(dead_code)]
impl QueuePreviewItem {
    pub(crate) fn display_text(&self, max_chars: usize) -> String {
        if max_chars <= 3 {
            return ".".repeat(max_chars);
        }
        if self.text.chars().count() <= max_chars {
            return self.text.clone();
        }
        let prefix = self.text.chars().take(max_chars - 3).collect::<String>();
        format!("{prefix}...")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct QueuePreviewState {
    pub(crate) next: Vec<QueuePreviewItem>,
    pub(crate) suspended: Vec<QueuePreviewItem>,
    pub(crate) backlog: Vec<QueuePreviewItem>,
}

impl QueuePreviewState {
    fn from_preview(preview: QueuePreview) -> Self {
        fn convert(items: Vec<QueuedInputView>) -> Vec<QueuePreviewItem> {
            items
                .into_iter()
                .map(|item| QueuePreviewItem { text: item.text })
                .collect()
        }

        Self {
            next: convert(preview.next),
            suspended: convert(preview.suspended),
            backlog: convert(preview.backlog),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum PatchLineKind {
    Context,
    Add,
    Remove,
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
    pub(crate) added: usize,
    pub(crate) removed: usize,
    pub(crate) hunks: usize,
    pub(crate) bytes_before: Option<usize>,
    pub(crate) bytes_after: Option<usize>,
    pub(crate) lines: Vec<PatchLineView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum TimelineItem {
    User { text: String, lane: QueuedInputLane },
    Assistant { text: String },
    Muted { title: String, detail: String },
    Expanded { title: String, body: String },
    Diagnostic { title: String, body: String },
    Patch { changes: Vec<PatchChangeView> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct TuiState {
    workspace_root: PathBuf,
    model_label: String,
    keymap: Keymap,
    theme: TuiTheme,
    input: TextInput,
    input_history: InputHistory,
    queue_preview: QueuePreviewState,
    timeline: Vec<TimelineItem>,
    timeline_scroll_offset: usize,
    pending_local_echoes: Vec<PendingLocalEcho>,
    run_state: InteractiveRunState,
    usage: Option<SessionUsage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingLocalEcho {
    text: String,
    lane: QueuedInputLane,
}

#[allow(dead_code)]
impl TuiState {
    pub(crate) fn new(
        workspace_root: PathBuf,
        model_label: String,
        keymap: Keymap,
        theme: TuiTheme,
    ) -> Self {
        Self {
            workspace_root,
            model_label,
            keymap,
            theme,
            input: TextInput::default(),
            input_history: InputHistory::default(),
            queue_preview: QueuePreviewState::from_preview(QueuePreview::empty()),
            timeline: Vec::new(),
            timeline_scroll_offset: 0,
            pending_local_echoes: Vec::new(),
            run_state: InteractiveRunState::WaitingForInput,
            usage: None,
        }
    }

    pub(crate) fn input_mut(&mut self) -> &mut TextInput {
        &mut self.input
    }

    pub(crate) fn input_text(&self) -> &str {
        self.input.text()
    }

    pub(crate) fn input_viewport(&self, max_width: usize) -> super::input::TextInputViewport {
        self.input.viewport(max_width)
    }

    pub(crate) fn take_input_for_submit(&mut self) -> Option<String> {
        let value = self.input.take_trimmed()?;
        self.input_history.record(&value);
        Some(value)
    }

    pub(crate) fn previous_input_history(&mut self) {
        self.input_history.previous(&mut self.input);
    }

    pub(crate) fn next_input_history(&mut self) {
        self.input_history.next(&mut self.input);
    }

    pub(crate) fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    pub(crate) fn theme(&self) -> &TuiTheme {
        &self.theme
    }

    pub(crate) fn timeline(&self) -> &[TimelineItem] {
        &self.timeline
    }

    pub(crate) fn push_timeline_item(&mut self, item: TimelineItem) {
        self.timeline.push(item);
        self.timeline_scroll_offset = 0;
    }

    pub(crate) fn push_user_timeline_item(&mut self, text: String, lane: QueuedInputLane) {
        self.push_timeline_item(TimelineItem::User { text, lane });
    }

    pub(crate) fn push_local_user_echo(&mut self, text: String, lane: QueuedInputLane) {
        self.pending_local_echoes.push(PendingLocalEcho {
            text: text.clone(),
            lane,
        });
        self.push_user_timeline_item(text, lane);
    }

    pub(crate) fn confirm_or_push_user_input(&mut self, text: String, lane: QueuedInputLane) {
        if let Some(index) = self
            .pending_local_echoes
            .iter()
            .position(|echo| echo.text == text && echo.lane == lane)
        {
            self.pending_local_echoes.remove(index);
            return;
        }

        self.push_user_timeline_item(text, lane);
    }

    pub(crate) fn replace_timeline_item(&mut self, index: usize, item: TimelineItem) {
        if let Some(slot) = self.timeline.get_mut(index) {
            *slot = item;
            self.timeline_scroll_offset = 0;
        }
    }

    pub(crate) fn timeline_scroll_offset(&self) -> usize {
        self.timeline_scroll_offset
    }

    pub(crate) fn scroll_timeline_up(&mut self) {
        self.timeline_scroll_offset = self.timeline_scroll_offset.saturating_add(1);
    }

    pub(crate) fn scroll_timeline_down(&mut self) {
        self.timeline_scroll_offset = self.timeline_scroll_offset.saturating_sub(1);
    }

    pub(crate) fn queue_preview(&self) -> &QueuePreviewState {
        &self.queue_preview
    }

    pub(crate) fn update_queue_preview(&mut self, preview: QueuePreview) {
        self.queue_preview = QueuePreviewState::from_preview(preview);
    }

    pub(crate) fn set_run_state(&mut self, state: InteractiveRunState) {
        self.run_state = state;
    }

    pub(crate) fn set_usage(&mut self, usage: SessionUsage) {
        self.usage = Some(usage);
    }

    pub(crate) fn status_text(&self) -> String {
        let usage = self
            .usage
            .as_ref()
            .map(format_session_usage)
            .unwrap_or_else(|| "usage -".to_owned());
        format!(
            "{:?}  {}  {}  {}",
            self.run_state,
            self.workspace_root.display(),
            self.model_label,
            usage
        )
    }
}

fn format_session_usage(usage: &SessionUsage) -> String {
    format!(
        "last in {} out {} | total {} tok",
        format_token_count(usage.last.input_tokens()),
        format_token_count(usage.last.output_tokens()),
        format_token_count(usage.total.total_tokens())
    )
}

fn format_token_count(tokens: u64) -> String {
    if tokens < 1_000 {
        return tokens.to_string();
    }

    let whole = tokens / 1_000;
    let decimal = (tokens % 1_000) / 100;
    if decimal == 0 {
        format!("{whole}k")
    } else {
        format!("{whole}.{decimal}k")
    }
}
