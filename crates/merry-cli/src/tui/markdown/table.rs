//! Markdown table layout, column width fitting, and cell truncation.

use crate::tui::{
    state::TuiState,
    text_wrap::{StyledTextPart, semantic_style},
    theme::SemanticColor,
};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Debug, Default, Clone)]
pub(super) struct TableState {
    pub(super) rows: Vec<TableRow>,
    current_row: Option<TableRow>,
    current_cell: Option<Vec<StyledTextPart>>,
    pub(super) in_head: bool,
}

#[derive(Debug, Clone)]
pub(super) struct TableRow {
    cells: Vec<Vec<StyledTextPart>>,
    header: bool,
}

impl TableState {
    pub(super) fn start_row(&mut self) {
        self.current_row = Some(TableRow {
            cells: Vec::new(),
            header: self.in_head,
        });
    }

    pub(super) fn finish_row(&mut self) {
        self.finish_cell();
        if let Some(row) = self.current_row.take()
            && !row.cells.is_empty()
        {
            self.rows.push(row);
        }
    }

    pub(super) fn start_cell(&mut self) {
        self.current_cell = Some(Vec::new());
    }

    pub(super) fn finish_cell(&mut self) {
        let Some(cell) = self.current_cell.take() else {
            return;
        };
        if let Some(row) = &mut self.current_row {
            row.cells.push(cell);
        }
    }

    pub(super) fn push_part(&mut self, part: StyledTextPart) {
        if self.current_cell.is_none() {
            self.start_cell();
        }
        if let Some(cell) = &mut self.current_cell {
            cell.push(part);
        }
    }
}

pub(super) fn table_lines(
    state: &TuiState,
    rows: Vec<TableRow>,
    region_width: u16,
) -> Vec<Line<'static>> {
    if rows.is_empty() {
        return Vec::new();
    }

    let column_count = rows.iter().map(|row| row.cells.len()).max().unwrap_or(0);
    if column_count == 0 {
        return Vec::new();
    }

    let mut widths = vec![1; column_count];
    for row in &rows {
        for (index, cell) in row.cells.iter().enumerate() {
            widths[index] = widths[index].max(styled_parts_width(cell));
        }
    }
    fit_table_widths(&mut widths, usize::from(region_width).max(1));

    let mut lines = Vec::new();
    for row in rows {
        lines.push(table_row_line(state, &row, &widths));
        if row.header {
            lines.push(table_separator_line(state, &widths));
        }
    }
    lines
}

fn table_row_line(state: &TuiState, row: &TableRow, widths: &[usize]) -> Line<'static> {
    let mut spans = Vec::new();
    spans.push(Span::styled(
        " ".to_owned(),
        semantic_style(state, SemanticColor::Muted),
    ));
    for (index, width) in widths.iter().copied().enumerate() {
        if index > 0 {
            spans.push(Span::styled(
                "  ".to_owned(),
                semantic_style(state, SemanticColor::Muted),
            ));
        }
        let cell = row.cells.get(index).map(Vec::as_slice).unwrap_or(&[]);
        let cell_style = if row.header {
            semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD)
        } else {
            semantic_style(state, SemanticColor::Assistant)
        };
        let mut cell_spans = if row.header {
            truncate_styled_parts_with_override(cell, width, cell_style)
        } else {
            truncate_styled_parts(cell, width, cell_style)
        };
        let cell_width = spans_width(&cell_spans);
        spans.append(&mut cell_spans);
        if cell_width < width {
            spans.push(Span::styled(
                " ".repeat(width - cell_width),
                semantic_style(state, SemanticColor::Muted),
            ));
        }
    }
    Line::from(spans)
}

fn table_separator_line(state: &TuiState, widths: &[usize]) -> Line<'static> {
    let mut text = String::from(" ");
    for (index, width) in widths.iter().copied().enumerate() {
        if index > 0 {
            text.push_str("  ");
        }
        text.push_str(&"─".repeat(width.max(3)));
    }
    Line::from(Span::styled(
        text,
        semantic_style(state, SemanticColor::Border),
    ))
}

fn fit_table_widths(widths: &mut [usize], max_width: usize) {
    if widths.is_empty() {
        return;
    }
    let separators = widths.len().saturating_sub(1) * 2 + 1;
    let available = max_width.saturating_sub(separators).max(widths.len());
    let mut total: usize = widths.iter().sum();
    while total > available {
        let Some((index, width)) = widths
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, width)| *width > 3)
            .max_by_key(|(_, width)| *width)
        else {
            break;
        };
        widths[index] = width.saturating_sub(1);
        total = total.saturating_sub(1);
    }
}

fn styled_parts_width(parts: &[StyledTextPart]) -> usize {
    parts
        .iter()
        .map(|part| UnicodeWidthStr::width(part.text.as_str()))
        .sum()
}

pub(super) fn spans_width(spans: &[Span<'static>]) -> usize {
    spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

fn truncate_styled_parts(
    parts: &[StyledTextPart],
    max_width: usize,
    fallback_style: Style,
) -> Vec<Span<'static>> {
    truncate_styled_parts_impl(parts, max_width, fallback_style, None)
}

fn truncate_styled_parts_with_override(
    parts: &[StyledTextPart],
    max_width: usize,
    style: Style,
) -> Vec<Span<'static>> {
    truncate_styled_parts_impl(parts, max_width, style, Some(style))
}

fn truncate_styled_parts_impl(
    parts: &[StyledTextPart],
    max_width: usize,
    fallback_style: Style,
    style_override: Option<Style>,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut used = 0;
    for part in parts {
        if used >= max_width {
            break;
        }
        let remaining = max_width - used;
        let text = truncate_to_width(&part.text, remaining);
        if text.is_empty() {
            continue;
        }
        used += UnicodeWidthStr::width(text.as_str());
        spans.push(Span::styled(text, style_override.unwrap_or(part.style)));
    }
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), fallback_style));
    }
    spans
}

fn truncate_to_width(text: &str, max_width: usize) -> String {
    let mut width = 0;
    let mut result = String::new();
    for character in text.chars() {
        let char_width = character.width().unwrap_or(0);
        if width + char_width > max_width {
            break;
        }
        width += char_width;
        result.push(character);
    }
    result
}
