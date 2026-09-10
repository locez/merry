use crate::tui::{
    copy_controls::{CopyTextBuilder, CopyTextError},
    state::TimelineAnchor,
    transcript::{SelectionPolicy, TranscriptRow},
};
use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    style::Modifier,
    widgets::{Paragraph, Widget, Wrap},
};
use std::ops::Range;
use unicode_width::UnicodeWidthStr;

const EDGE_SCROLL_ROWS: usize = 3;
const COPY_CHUNK_ROWS: u16 = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MouseInput {
    Down(Position),
    Drag(Position),
    Up(Position),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SelectionPoint {
    row: usize,
    column: u16,
}

impl SelectionPoint {
    fn new(row: usize, column: u16) -> Self {
        Self { row, column }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollDirection {
    Up,
    Down,
}

/// Freezes styled transcript lines, rendering only the rows needed for display or copying.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectionSnapshot {
    rows: Vec<TranscriptRow>,
    row_starts: Vec<usize>,
    item_rows: Vec<usize>,
}

impl SelectionSnapshot {
    fn new(
        rows: Vec<TranscriptRow>,
        item_starts: Vec<usize>,
        row_starts: Vec<usize>,
    ) -> Option<Self> {
        if row_starts.len() != rows.len().saturating_add(1) {
            return None;
        }
        let item_rows = item_starts
            .into_iter()
            .map(|index| row_starts.get(index).copied())
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            rows,
            row_starts,
            item_rows,
        })
    }

    fn total_rows(&self) -> usize {
        self.row_starts.last().copied().unwrap_or(0)
    }

    fn render_rows(&self, start: usize, area: Rect) -> Buffer {
        let mut buffer = Buffer::empty(area);
        let first_line = self
            .row_starts
            .partition_point(|row| *row <= start)
            .saturating_sub(1);
        if first_line >= self.rows.len() || area.is_empty() {
            return buffer;
        }
        let end = start.saturating_add(usize::from(area.height));
        let last_line = self
            .row_starts
            .partition_point(|row| *row < end)
            .min(self.rows.len());
        let scroll =
            u16::try_from(start.saturating_sub(self.row_starts[first_line])).unwrap_or(u16::MAX);
        Paragraph::new(
            self.rows[first_line..last_line]
                .iter()
                .map(|row| row.display.clone())
                .collect::<Vec<_>>(),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0))
        .render(area, &mut buffer);
        buffer
    }

    fn anchor_at(&self, row: usize) -> Option<TimelineAnchor> {
        let index = self
            .item_rows
            .partition_point(|start| *start <= row)
            .saturating_sub(1);
        self.item_rows
            .get(index)
            .map(|start| TimelineAnchor::new(index, row.saturating_sub(*start)))
    }

    fn row_index_at(&self, visual_row: usize) -> Option<usize> {
        let index = self
            .row_starts
            .partition_point(|start| *start <= visual_row)
            .saturating_sub(1);
        (index < self.rows.len()).then_some(index)
    }
}

/// Tracks a drag in document rows rather than screen coordinates, including off-screen text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TextSelection {
    snapshot: SelectionSnapshot,
    area: Rect,
    scroll: usize,
    anchor: SelectionPoint,
    cursor: SelectionPoint,
    pointer: Position,
    scroll_direction: Option<ScrollDirection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelectionViewport {
    pub(crate) anchor: TimelineAnchor,
    pub(crate) offset: usize,
}

impl TextSelection {
    /// Takes the timeline renderer's bounded logical lines and current wrapped scroll origin.
    pub(crate) fn new(
        rows: Vec<TranscriptRow>,
        item_starts: Vec<usize>,
        row_starts: Vec<usize>,
        area: Rect,
        scroll: usize,
        pointer: Position,
    ) -> Option<Self> {
        if area.is_empty() || !area.contains(pointer) {
            return None;
        }
        let snapshot = SelectionSnapshot::new(rows, item_starts, row_starts)?;
        let anchor = SelectionPoint::new(
            scroll.saturating_add(usize::from(pointer.y - area.y)),
            pointer.x - area.x,
        );
        Some(Self {
            snapshot,
            area,
            scroll,
            anchor,
            cursor: anchor,
            pointer,
            scroll_direction: None,
        })
    }

    pub(crate) fn area(&self) -> Rect {
        self.area
    }

    pub(crate) fn drag_to(&mut self, position: Position) {
        self.pointer = position;
        self.cursor = self.document_position(position);
        self.scroll_direction = if position.y <= self.area.y {
            Some(ScrollDirection::Up)
        } else if position.y >= self.area.bottom().saturating_sub(1) {
            Some(ScrollDirection::Down)
        } else {
            None
        };
    }

    pub(crate) fn is_autoscrolling(&self) -> bool {
        self.next_scroll() != self.scroll
    }

    /// Advances one refresh tick and reports the corresponding live-timeline reading position.
    pub(crate) fn autoscroll(&mut self) -> Option<SelectionViewport> {
        let next = self.next_scroll();
        if next == self.scroll {
            return None;
        }
        self.scroll = next;
        self.cursor = self.document_position(self.pointer);
        self.snapshot
            .anchor_at(next)
            .map(|anchor| SelectionViewport {
                anchor,
                offset: self.max_scroll().saturating_sub(next),
            })
    }

    pub(crate) fn release(mut self, position: Position) -> Result<Option<String>, CopyTextError> {
        self.cursor = self.document_position(position);
        self.text()
    }

    fn text(&self) -> Result<Option<String>, CopyTextError> {
        if self.anchor == self.cursor {
            return Ok(None);
        }
        let (start, end) = self.ordered_positions();
        let mut text = CopyTextBuilder::new();
        let mut copied_row = false;
        let mut chunk_start = start.row;
        while chunk_start <= end.row {
            let height = u16::try_from(end.row.saturating_sub(chunk_start))
                .unwrap_or(u16::MAX)
                .saturating_add(1)
                .min(COPY_CHUNK_ROWS);
            let buffer = self
                .snapshot
                .render_rows(chunk_start, Rect::new(0, 0, self.area.width, height));
            for row in 0..height {
                let document_row = chunk_start + usize::from(row);
                let columns = self.row_columns(document_row, start, end);
                let Some(source_row) = self.snapshot.row_index_at(document_row) else {
                    continue;
                };
                let selection = self.snapshot.rows[source_row].selection;
                let columns = match selection {
                    SelectionPolicy::Keep => columns,
                    SelectionPolicy::Skip => continue,
                    SelectionPolicy::StripPrefix(prefix) => columns.start.max(prefix)..columns.end,
                };
                if columns.start >= columns.end {
                    continue;
                }
                let mut row_text = String::new();
                let mut column = 0;
                while column < self.area.width {
                    let symbol = buffer[(column, row)].symbol();
                    let next = next_column(column, symbol, self.area.width);
                    if !symbol.is_empty() && column < columns.end && next > columns.start {
                        row_text.push_str(symbol);
                    }
                    column = next;
                }
                if columns.end == self.area.width {
                    let trimmed_len = row_text.trim_end_matches(' ').len();
                    row_text.truncate(trimmed_len);
                }
                if copied_row {
                    text.push_str("\n")?;
                }
                text.push_str(&row_text)?;
                copied_row = true;
            }
            chunk_start = chunk_start.saturating_add(usize::from(height));
        }
        if copied_row {
            Ok(Some(text.finish()))
        } else {
            Ok(None)
        }
    }

    pub(crate) fn render(&self, buffer: &mut Buffer) {
        let mut visible = self.snapshot.render_rows(self.scroll, self.area);
        let (start, end) = self.ordered_positions();
        for row in self.area.y..self.area.bottom() {
            let document_row = self.scroll + usize::from(row - self.area.y);
            if self.anchor != self.cursor && (start.row..=end.row).contains(&document_row) {
                let columns = self.row_columns(document_row, start, end);
                let mut column = 0;
                while column < self.area.width {
                    let symbol = visible[(self.area.x + column, row)].symbol();
                    let next = next_column(column, symbol, self.area.width);
                    if column < columns.end && next > columns.start {
                        for selected_column in column..next {
                            visible[(self.area.x + selected_column, row)]
                                .set_style(Modifier::REVERSED);
                        }
                    }
                    column = next;
                }
            }
            for column in self.area.x..self.area.right() {
                buffer[(column, row)] = visible[(column, row)].clone();
            }
        }
    }

    fn max_scroll(&self) -> usize {
        self.snapshot
            .total_rows()
            .saturating_sub(usize::from(self.area.height))
    }

    fn next_scroll(&self) -> usize {
        match self.scroll_direction {
            Some(ScrollDirection::Up) => self.scroll.saturating_sub(EDGE_SCROLL_ROWS),
            Some(ScrollDirection::Down) if self.scroll < self.max_scroll() => self
                .scroll
                .saturating_add(EDGE_SCROLL_ROWS)
                .min(self.max_scroll()),
            _ => self.scroll,
        }
    }

    fn document_position(&self, position: Position) -> SelectionPoint {
        SelectionPoint::new(
            self.scroll
                + usize::from(
                    position
                        .y
                        .saturating_sub(self.area.y)
                        .min(self.area.height - 1),
                ),
            position
                .x
                .saturating_sub(self.area.x)
                .min(self.area.width - 1),
        )
    }

    fn ordered_positions(&self) -> (SelectionPoint, SelectionPoint) {
        if self.anchor <= self.cursor {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }

    fn row_columns(&self, row: usize, start: SelectionPoint, end: SelectionPoint) -> Range<u16> {
        let first = if row == start.row { start.column } else { 0 };
        let last = if row == end.row {
            end.column.saturating_add(1)
        } else {
            self.area.width
        };
        first..last.min(self.area.width)
    }
}

fn next_column(column: u16, symbol: &str, width: u16) -> u16 {
    let symbol_width = u16::try_from(UnicodeWidthStr::width(symbol))
        .unwrap_or(u16::MAX)
        .max(1);
    column.saturating_add(symbol_width).min(width)
}
