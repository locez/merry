use crate::tui::{state::TuiState, text_wrap::semantic_style, theme::SemanticColor};
use ratatui::{
    style::Modifier,
    text::{Line, Span},
};

pub(crate) const MAX_CLIPBOARD_BYTES: usize = 1_048_576;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyTextError {
    TooLarge,
}

pub(crate) struct CopyTextBuilder {
    text: String,
}

impl CopyTextBuilder {
    pub(crate) fn new() -> Self {
        Self {
            text: String::new(),
        }
    }

    pub(crate) fn push_str(&mut self, chunk: &str) -> Result<(), CopyTextError> {
        let next_len = self
            .text
            .len()
            .checked_add(chunk.len())
            .ok_or(CopyTextError::TooLarge)?;
        if next_len > MAX_CLIPBOARD_BYTES {
            return Err(CopyTextError::TooLarge);
        }
        self.text.push_str(chunk);
        Ok(())
    }

    pub(crate) fn finish(self) -> String {
        self.text
    }
}

pub(crate) fn validate_copy_text(text: String) -> Result<String, CopyTextError> {
    if text.len() > MAX_CLIPBOARD_BYTES {
        Err(CopyTextError::TooLarge)
    } else {
        Ok(text)
    }
}

/// Code owns its parsed text; replies refer to the existing timeline instead of duplicating it.
pub(crate) enum CopyContent {
    Text(String),
    AssistantMessage(usize),
}

/// Associates an actual copy control with its undecorated source and logical row.
pub(crate) struct CopyTarget {
    pub(crate) line_index: usize,
    pub(crate) column: u16,
    pub(crate) width: u16,
    pub(crate) content: CopyContent,
}

impl CopyTarget {
    pub(crate) fn new(line_index: usize, width: u16, content: CopyContent) -> Self {
        Self {
            line_index,
            column: 0,
            width,
            content,
        }
    }
}

/// Renders a complete copy control, or omits it when even the compact label cannot fit.
pub(crate) fn copy_header(
    state: &TuiState,
    label: &'static str,
    width: u16,
) -> Option<(Line<'static>, u16)> {
    let label = if usize::from(width) >= label.len() {
        label
    } else if width >= 6 {
        "[Copy]"
    } else {
        return None;
    };
    let line = Line::from(Span::styled(
        label,
        semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
    ));
    Some((line, u16::try_from(label.len()).unwrap_or(u16::MAX)))
}

#[cfg(test)]
mod tests {
    use super::{CopyTextBuilder, MAX_CLIPBOARD_BYTES};

    #[test]
    fn copy_builder_rejects_the_first_chunk_over_the_limit() {
        let mut builder = CopyTextBuilder::new();
        builder
            .push_str(&"x".repeat(MAX_CLIPBOARD_BYTES - 1))
            .expect("payload below the limit should be accepted");
        builder
            .push_str("x")
            .expect("payload at the limit should be accepted");
        assert_eq!(builder.push_str("x"), Err(super::CopyTextError::TooLarge));
        assert_eq!(builder.finish().len(), MAX_CLIPBOARD_BYTES);
    }
}
