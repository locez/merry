use merry_core::{QueuedInputLane, QueuedInputView};

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
    pub(super) fn from_preview(preview: QueuePreview) -> Self {
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

    pub(crate) fn is_empty(&self) -> bool {
        self.next.is_empty() && self.suspended.is_empty() && self.backlog.is_empty()
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
    LocalCommand { title: String, body: String },
    Expanded { title: String, body: String },
    Diagnostic { title: String, body: String },
    Patch { changes: Vec<PatchChangeView> },
}
