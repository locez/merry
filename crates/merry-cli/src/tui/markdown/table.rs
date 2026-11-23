//! Markdown table layout, smart column sizing, multi-line cell wrapping, and
//! narrow-terminal stacked-card fallback (never silently truncating cell text).

use crate::tui::{
    links::LineLink,
    state::TuiState,
    text_wrap::{StyledTextPart, WrappedLine, semantic_style, wrap_styled_parts_with_links},
    theme::SemanticColor,
};
use pulldown_cmark::Alignment;
use ratatui::{
    style::Modifier,
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

const LEFT_MARGIN: &str = " ";
const COL_DIVIDER: &str = " │ ";
const RULE_DIVIDER: &str = "─┼─";
const MIN_READABLE_COL_WIDTH: usize = 8;

#[derive(Debug, Default, Clone)]
pub(super) struct TableState {
    pub(super) alignments: Vec<Alignment>,
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
    pub(super) fn new(alignments: Vec<Alignment>) -> Self {
        Self {
            alignments,
            ..Self::default()
        }
    }

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
    alignments: &[Alignment],
    region_width: u16,
) -> Vec<WrappedLine> {
    if rows.is_empty() {
        return Vec::new();
    }

    let column_count = rows.iter().map(|row| row.cells.len()).max().unwrap_or(0);
    if column_count == 0 {
        return Vec::new();
    }

    let max_width = usize::from(region_width).max(1);
    let mut natural_widths = vec![1usize; column_count];
    for row in &rows {
        for (index, cell) in row.cells.iter().enumerate() {
            natural_widths[index] = natural_widths[index].max(styled_parts_width(cell));
        }
    }

    let chrome_width = UnicodeWidthStr::width(LEFT_MARGIN)
        + column_count.saturating_sub(1) * UnicodeWidthStr::width(COL_DIVIDER);
    let min_widths: Vec<usize> = natural_widths
        .iter()
        .map(|&natural| natural.clamp(1, MIN_READABLE_COL_WIDTH))
        .collect();
    let min_required = chrome_width.saturating_add(min_widths.iter().sum::<usize>());

    if column_count > 1 && max_width < min_required {
        return render_stacked_cards(state, &rows, column_count, region_width);
    }

    let available = max_width.saturating_sub(chrome_width).max(column_count);
    let widths = fit_table_widths(&natural_widths, &min_widths, available);
    render_tabular_rows(state, &rows, alignments, &widths)
}

fn render_tabular_rows(
    state: &TuiState,
    rows: &[TableRow],
    alignments: &[Alignment],
    widths: &[usize],
) -> Vec<WrappedLine> {
    let wrapped_rows: Vec<(bool, Vec<Vec<WrappedLine>>)> = rows
        .iter()
        .map(|row| {
            let cells = widths
                .iter()
                .copied()
                .enumerate()
                .map(|(col, width)| {
                    let parts = row.cells.get(col).map(Vec::as_slice).unwrap_or(&[]);
                    wrap_table_cell(state, parts, row.header, width)
                })
                .collect();
            (row.header, cells)
        })
        .collect();

    let has_multiline_body = wrapped_rows
        .iter()
        .filter(|(is_header, _)| !*is_header)
        .any(|(_, cells)| row_visual_height(cells) > 1);

    let mut lines = Vec::new();
    let mut emitted_body_row = false;
    for (is_header, cells) in wrapped_rows {
        if !is_header && has_multiline_body && emitted_body_row {
            lines.push(WrappedLine::plain(table_rule_line(state, widths, true)));
        }
        let height = row_visual_height(&cells);
        for line_idx in 0..height {
            lines.push(compose_visual_row(
                state, &cells, alignments, widths, line_idx,
            ));
        }
        if is_header {
            lines.push(WrappedLine::plain(table_rule_line(state, widths, false)));
        } else {
            emitted_body_row = true;
        }
    }
    lines
}

fn wrap_table_cell(
    state: &TuiState,
    parts: &[StyledTextPart],
    is_header: bool,
    width: usize,
) -> Vec<WrappedLine> {
    if parts.is_empty() {
        return vec![WrappedLine::plain(Line::default())];
    }
    let header_style = semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD);
    let styled_parts: Vec<StyledTextPart> = parts
        .iter()
        .map(|part| {
            let style = if is_header {
                if part.link.is_some() {
                    part.style.add_modifier(Modifier::BOLD)
                } else {
                    header_style
                }
            } else {
                part.style
            };
            StyledTextPart::with_link(part.text.clone(), style, part.atomic, part.link.clone())
        })
        .collect();

    let cell_width = u16::try_from(width).unwrap_or(u16::MAX).max(1);
    let mut wrapped = wrap_styled_parts_with_links(styled_parts, cell_width);
    if wrapped.is_empty() {
        wrapped.push(WrappedLine::plain(Line::default()));
    }
    wrapped
}

fn row_visual_height(cells: &[Vec<WrappedLine>]) -> usize {
    cells.iter().map(Vec::len).max().unwrap_or(1).max(1)
}

fn compose_visual_row(
    state: &TuiState,
    cells: &[Vec<WrappedLine>],
    alignments: &[Alignment],
    widths: &[usize],
    line_idx: usize,
) -> WrappedLine {
    let divider_style = semantic_style(state, SemanticColor::Border);
    let pad_style = semantic_style(state, SemanticColor::Muted);
    let mut spans = vec![Span::styled(LEFT_MARGIN.to_owned(), pad_style)];
    let mut links: Vec<LineLink> = Vec::new();
    let mut col_x = UnicodeWidthStr::width(LEFT_MARGIN);
    let last_col = widths.len().saturating_sub(1);

    for (index, width) in widths.iter().copied().enumerate() {
        if index > 0 {
            spans.push(Span::styled(COL_DIVIDER.to_owned(), divider_style));
            col_x += UnicodeWidthStr::width(COL_DIVIDER);
        }

        let alignment = alignments.get(index).copied().unwrap_or(Alignment::None);
        if let Some(cell_line) = cells.get(index).and_then(|lines| lines.get(line_idx)) {
            let content_width = spans_width(&cell_line.line.spans);
            let slack = width.saturating_sub(content_width);
            let (left_pad, right_pad) = match alignment {
                Alignment::Right => (slack, 0),
                Alignment::Center => (slack / 2, slack - slack / 2),
                Alignment::None | Alignment::Left => (0, slack),
            };
            if left_pad > 0 {
                spans.push(Span::styled(" ".repeat(left_pad), pad_style));
            }
            for link in &cell_line.links {
                links.push(LineLink::new(
                    link.column
                        .saturating_add(u16::try_from(col_x + left_pad).unwrap_or(u16::MAX)),
                    link.width,
                    link.url.clone(),
                ));
            }
            spans.extend(cell_line.line.spans.iter().cloned());
            if right_pad > 0 && index < last_col {
                spans.push(Span::styled(" ".repeat(right_pad), pad_style));
            }
        } else if index < last_col {
            spans.push(Span::styled(" ".repeat(width), pad_style));
        }

        col_x += width;
    }

    WrappedLine::with_links(Line::from(spans), links)
}

fn table_rule_line(state: &TuiState, widths: &[usize], subtle: bool) -> Line<'static> {
    let mut text = String::from(LEFT_MARGIN);
    for (index, width) in widths.iter().copied().enumerate() {
        if index > 0 {
            text.push_str(RULE_DIVIDER);
        }
        text.push_str(&"─".repeat(width.max(1)));
    }
    let mut style = semantic_style(state, SemanticColor::Border);
    if subtle {
        style = style.add_modifier(Modifier::DIM);
    }
    Line::from(Span::styled(text, style))
}

/// Renders wide tables on narrow terminals as stacked key-value cards so every
/// header and cell value remains fully readable without narrow-column wrapping.
fn render_stacked_cards(
    state: &TuiState,
    rows: &[TableRow],
    column_count: usize,
    region_width: u16,
) -> Vec<WrappedLine> {
    let header_row = rows.iter().find(|row| row.header);
    let body_rows: Vec<&TableRow> = rows.iter().filter(|row| !row.header).collect();
    let effective_rows: Vec<&TableRow> = if body_rows.is_empty() {
        rows.iter().collect()
    } else {
        body_rows
    };

    let header_style = semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD);
    let border_style = semantic_style(state, SemanticColor::Border);
    let content_width = region_width.saturating_sub(2).max(1);
    let rule_width = usize::from(region_width.saturating_sub(1).max(1));

    let mut out = Vec::new();
    for (row_idx, row) in effective_rows.into_iter().enumerate() {
        if row_idx > 0 {
            out.push(WrappedLine::plain(Line::from(Span::styled(
                format!("{LEFT_MARGIN}{}", "─".repeat(rule_width)),
                border_style.add_modifier(Modifier::DIM),
            ))));
        }

        for col in 0..column_count {
            let mut parts: Vec<StyledTextPart> = Vec::new();
            if let Some(header) = header_row
                && let Some(header_cell) = header.cells.get(col)
                && !header_cell.is_empty()
            {
                let label = parts_plain_text(header_cell);
                let trimmed = label.trim();
                if !trimmed.is_empty() {
                    parts.push(StyledTextPart::new(
                        format!("{trimmed}: "),
                        header_style,
                        false,
                    ));
                }
            }
            if let Some(cell) = row.cells.get(col) {
                parts.extend(cell.iter().cloned());
            }
            if parts.is_empty() {
                continue;
            }

            let wrapped = wrap_styled_parts_with_links(parts, content_width);
            for (line_idx, mut wrapped_line) in wrapped.into_iter().enumerate() {
                let prefix = if line_idx == 0 { " " } else { "  " };
                wrapped_line
                    .line
                    .spans
                    .insert(0, Span::raw(prefix.to_owned()));
                wrapped_line.shift_links(UnicodeWidthStr::width(prefix));
                out.push(wrapped_line);
            }
        }
    }
    out
}

fn parts_plain_text(parts: &[StyledTextPart]) -> String {
    parts.iter().map(|part| part.text.as_str()).collect()
}

/// Distributes available width across columns: short columns keep their exact
/// natural width, while overflowing columns share the remaining space
/// proportionally above their readable floor.
fn fit_table_widths(natural: &[usize], min_widths: &[usize], available: usize) -> Vec<usize> {
    if natural.is_empty() {
        return Vec::new();
    }
    let natural_total: usize = natural.iter().sum();
    if natural_total <= available {
        return natural.to_vec();
    }

    let mut widths = vec![0usize; natural.len()];
    let mut locked = vec![false; natural.len()];
    let mut remaining = available;

    loop {
        let unlocked_count = locked.iter().filter(|&&is_locked| !is_locked).count();
        if unlocked_count == 0 {
            return widths;
        }
        let fair_share = remaining / unlocked_count;
        let mut locked_any = false;
        for ((&nat, width), is_locked) in natural.iter().zip(&mut widths).zip(&mut locked) {
            if !*is_locked && nat <= fair_share {
                *width = nat;
                *is_locked = true;
                remaining = remaining.saturating_sub(nat);
                locked_any = true;
            }
        }
        if !locked_any {
            break;
        }
    }

    let unlocked_indices: Vec<usize> = (0..natural.len()).filter(|&i| !locked[i]).collect();
    let min_sum: usize = unlocked_indices.iter().map(|&i| min_widths[i]).sum();
    let extra = remaining.saturating_sub(min_sum);
    let excess_total: usize = unlocked_indices
        .iter()
        .map(|&i| natural[i].saturating_sub(min_widths[i]))
        .sum();

    let mut assigned = 0usize;
    for &index in &unlocked_indices {
        let base = min_widths[index];
        let excess = natural[index].saturating_sub(base);
        let bonus = extra
            .saturating_mul(excess)
            .checked_div(excess_total)
            .unwrap_or(0);
        let cap = natural[index];
        let col_width = base.saturating_add(bonus).min(cap);
        widths[index] = col_width;
        assigned = assigned.saturating_add(col_width);
    }

    // Distribute any integer-division remainder one column at a time to the
    // column furthest from its natural width.
    let mut leftover = remaining.saturating_sub(assigned);
    while leftover > 0 {
        let Some(&best) = unlocked_indices
            .iter()
            .filter(|&&i| widths[i] < natural[i])
            .max_by_key(|&&i| natural[i].saturating_sub(widths[i]))
        else {
            break;
        };
        widths[best] += 1;
        leftover -= 1;
    }

    widths
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
