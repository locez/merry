use ratatui::text::Line;

/// Describes which part of a rendered row belongs in mouse-selected text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelectionPolicy {
    Keep,
    Skip,
    StripPrefix(u16),
}

impl SelectionPolicy {
    pub(crate) fn with_prefix(self, prefix_width: u16) -> Self {
        match self {
            Self::Keep if prefix_width == 0 => Self::Keep,
            Self::Keep => Self::StripPrefix(prefix_width),
            Self::Skip => Self::Skip,
            Self::StripPrefix(width) => Self::StripPrefix(width.saturating_add(prefix_width)),
        }
    }
}

/// A rendered transcript row plus its source-selection semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TranscriptRow {
    pub(crate) display: Line<'static>,
    pub(crate) selection: SelectionPolicy,
}

impl TranscriptRow {
    pub(crate) fn new(display: Line<'static>, selection: SelectionPolicy) -> Self {
        Self { display, selection }
    }
}
