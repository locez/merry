use super::{input::TextInput, keymap::Keymap, theme::TuiTheme};
use merry_core::{InteractiveRunState, QueuedInputView, SessionUsage};
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

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum TimelineItem {
    Assistant { text: String },
    Muted { title: String, detail: String },
    Expanded { title: String, body: String },
    Diagnostic { title: String, body: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct TuiState {
    workspace_root: PathBuf,
    model_label: String,
    keymap: Keymap,
    theme: TuiTheme,
    input: TextInput,
    queue_preview: QueuePreviewState,
    timeline: Vec<TimelineItem>,
    run_state: InteractiveRunState,
    usage: Option<SessionUsage>,
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
            queue_preview: QueuePreviewState::from_preview(QueuePreview::empty()),
            timeline: Vec::new(),
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
            .map(|usage| format!("{} tok", usage.total.total_tokens()))
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
