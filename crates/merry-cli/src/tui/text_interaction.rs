use crate::tui::{
    copy_controls::{CopyTextBuilder, CopyTextError},
    render::timeline_cache::TimelineLayoutSnapshot,
    state::TimelineAnchor,
    transcript::SelectionPolicy,
};
use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect, Size},
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

/// Freezes immutable `Arc`-backed per-item layouts so starting a selection is
/// O(N_items) pointer copies and renders only visible rows on drag/copy.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectionSnapshot(TimelineLayoutSnapshot);

impl SelectionSnapshot {
    fn total_rows(&self) -> usize {
        self.0.total_rows()
    }

    fn render_rows(&self, start: usize, area: Rect) -> Buffer {
        let mut buffer = Buffer::empty(area);
        if area.is_empty() {
            return buffer;
        }
        let viewport = self.0.viewport_slice(start, area.height);
        if viewport.lines.is_empty() {
            return buffer;
        }
        Paragraph::new(viewport.lines)
            .wrap(Wrap { trim: false })
            .scroll((viewport.scroll, 0))
            .render(area, &mut buffer);
        buffer
    }

    fn anchor_at(&self, row: usize) -> Option<TimelineAnchor> {
        self.0.anchor_at(row)
    }

    fn selection_policy_at(&self, visual_row: usize) -> Option<SelectionPolicy> {
        self.0.selection_policy_at(visual_row)
    }
}

/// Plain text captured from a drag, plus where in the timeline it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CapturedText {
    pub(crate) text: String,
    pub(crate) item_index: usize,
    pub(crate) visual_row: usize,
    pub(crate) column: u16,
}

/// Tracks a drag in document rows rather than screen coordinates, including off-screen text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TextSelection {
    snapshot: SelectionSnapshot,
    area: Rect,
    terminal_size: Size,
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
    /// Captures the cached per-item layout snapshot and current wrapped scroll origin.
    pub(crate) fn new(
        snapshot: TimelineLayoutSnapshot,
        area: Rect,
        terminal_size: Size,
        scroll: usize,
        pointer: Position,
    ) -> Option<Self> {
        if area.is_empty() || !area.contains(pointer) {
            return None;
        }
        let anchor = SelectionPoint::new(
            scroll.saturating_add(usize::from(pointer.y - area.y)),
            pointer.x - area.x,
        );
        Some(Self {
            snapshot: SelectionSnapshot(snapshot),
            area,
            terminal_size,
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

    pub(crate) fn terminal_size(&self) -> Size {
        self.terminal_size
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

    pub(crate) fn release(
        mut self,
        position: Position,
    ) -> Result<Option<CapturedText>, CopyTextError> {
        self.cursor = self.document_position(position);
        let Some(text) = self.text()? else {
            return Ok(None);
        };
        let (start, _) = self.ordered_positions();
        let item_index = self
            .snapshot
            .anchor_at(start.row)
            .map(|anchor| anchor.item_index)
            .unwrap_or(0);
        Ok(Some(CapturedText {
            text,
            item_index,
            visual_row: start.row,
            column: start.column,
        }))
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
                let Some(selection) = self.snapshot.selection_policy_at(document_row) else {
                    continue;
                };
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
