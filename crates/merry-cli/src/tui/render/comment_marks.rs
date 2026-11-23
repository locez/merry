//! Highlights quoted text in the timeline and resolves clicks on those marks.
//!
//! A quote is stored as an item plus the Nth match of its text with newlines
//! removed. Wrapped rows join with newlines when copied, so the match stays on
//! the same words after the terminal width changes.

use super::sticky::{TimelineScrollSplit, split_timeline_content, timeline_scroll_split};
use super::timeline_cache::TimelineLayoutSnapshot;
use super::timeline_layout::{timeline_content_region, timeline_layout, timeline_scroll_start};
use crate::tui::{
    state::{CommentHighlight, TuiState},
    text_wrap::semantic_style,
    theme::SemanticColor,
    transcript::SelectionPolicy,
};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Position, Rect},
    style::{Modifier, Style},
    widgets::{Paragraph, Widget, Wrap},
};
use std::ops::Range;
use unicode_width::UnicodeWidthStr;

const CHUNK_ROWS: u16 = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommentMarkHit {
    pub(crate) index: usize,
    pub(crate) in_sticky_header: bool,
}

#[derive(Debug, Clone)]
struct Symbol {
    byte: usize,
    len: usize,
    visual_row: usize,
    start: u16,
    end: u16,
}

struct ItemStream {
    symbols: Vec<Symbol>,
    haystack: String,
    width: u16,
}

struct CellSpan {
    visual_row: usize,
    start: u16,
    end: u16,
}

struct PlacedMark {
    index: Option<usize>,
    pending: bool,
    cells: Vec<CellSpan>,
    number: Option<usize>,
    badge: Option<CellSpan>,
}

pub(super) struct CommentOverlay {
    marks: Vec<PlacedMark>,
}

pub(super) fn comment_overlay(
    state: &TuiState,
    timeline: &TimelineLayoutSnapshot,
    width: u16,
) -> CommentOverlay {
    CommentOverlay {
        marks: if state.comment_highlights().is_empty() {
            Vec::new()
        } else {
            placed_marks(state, timeline, width)
        },
    }
}

pub(super) fn paint_comment_overlay(
    frame: &mut Frame<'_>,
    state: &TuiState,
    overlay: &CommentOverlay,
    split: &TimelineScrollSplit,
    header: Rect,
    body: Rect,
) {
    if overlay.marks.is_empty() {
        return;
    }
    let buffer = frame.buffer_mut();
    for mark in &overlay.marks {
        let style = mark_style(state, mark.pending);
        for cell in &mark.cells {
            paint_span(buffer, body, split.body_scroll, cell, style);
            if let Some(origin) = split.pinned_origin {
                paint_span(buffer, sticky_rect(header, split), origin, cell, style);
            }
        }
        paint_badge(buffer, state, body, split.body_scroll, mark);
        if let Some(origin) = split.pinned_origin {
            paint_badge(buffer, state, sticky_rect(header, split), origin, mark);
        }
    }
}

pub(crate) fn comment_mark_at(
    state: &TuiState,
    region: Rect,
    position: Position,
) -> Option<CommentMarkHit> {
    let content = timeline_content_region(region);
    if content.is_empty() || state.comment_highlights().is_empty() {
        return None;
    }
    let timeline = timeline_layout(state, content);
    let scroll = timeline_scroll_start(state, &timeline, content);
    let split = timeline_scroll_split(state, &timeline, content.height, scroll);
    let (header, body) = split_timeline_content(content, split.header_rows);
    let (visual_row, column, in_sticky_header) = document_cell(&split, header, body, position)?;
    let marks = placed_marks(state, &timeline, content.width);
    for mark in marks.iter().rev() {
        let hit =
            mark.cells.iter().any(|cell| {
                cell.visual_row == visual_row && column >= cell.start && column < cell.end
            }) || badge_hit(mark, visual_row, column);
        if hit {
            return mark.index.map(|index| CommentMarkHit {
                index,
                in_sticky_header,
            });
        }
    }
    None
}

pub(crate) fn quote_occurrence(
    state: &TuiState,
    region: Rect,
    item_index: usize,
    quote: &str,
    visual_row: usize,
    column: u16,
) -> usize {
    let content = timeline_content_region(region);
    if content.width == 0 {
        return 0;
    }
    let timeline = timeline_layout(state, content);
    let Some(stream) = item_stream(&timeline, item_index, content.width) else {
        return 0;
    };
    let ranges = match_ranges(&stream.haystack, &needle(quote));
    occurrence_at(&stream, &ranges, visual_row, column)
}

fn placed_marks(
    state: &TuiState,
    timeline: &TimelineLayoutSnapshot,
    width: u16,
) -> Vec<PlacedMark> {
    let highlights = state.comment_highlights();
    let mut streams: Vec<(usize, ItemStream)> = Vec::new();
    let mut marks = Vec::new();
    for highlight in highlights {
        if !streams
            .iter()
            .any(|(index, _)| *index == highlight.item_index)
            && let Some(stream) = item_stream(timeline, highlight.item_index, width)
        {
            streams.push((highlight.item_index, stream));
        }
        let Some((_, stream)) = streams
            .iter()
            .find(|(index, _)| *index == highlight.item_index)
        else {
            continue;
        };
        if let Some(mark) = place_mark(stream, &highlight) {
            marks.push(mark);
        }
    }
    marks
}

fn place_mark(stream: &ItemStream, highlight: &CommentHighlight) -> Option<PlacedMark> {
    let ranges = match_ranges(&stream.haystack, &needle(&highlight.quote));
    let range = ranges.get(highlight.occurrence)?.clone();
    let cells = spans_for(stream, &range);
    if cells.is_empty() {
        return None;
    }
    let badge = badge_span(stream, &cells, highlight.number);
    Some(PlacedMark {
        index: highlight.comment_index,
        pending: highlight.pending,
        cells,
        number: highlight.number,
        badge,
    })
}

fn item_stream(
    timeline: &TimelineLayoutSnapshot,
    item_index: usize,
    width: u16,
) -> Option<ItemStream> {
    if width == 0 {
        return None;
    }
    let item = timeline.item_layout(item_index)?;
    let visual_start = *timeline.item_visual_starts.get(item_index)?;
    let height = item.visual_height();
    if height == 0 {
        return None;
    }
    let lines = item
        .rows
        .iter()
        .map(|row| row.display.clone())
        .collect::<Vec<_>>();
    let mut symbols = Vec::new();
    let mut haystack = String::new();
    let mut offset = 0_usize;
    while offset < height {
        let rows = height.saturating_sub(offset);
        let rows_u16 = u16::try_from(rows).unwrap_or(u16::MAX).min(CHUNK_ROWS);
        let scroll = u16::try_from(offset).ok()?;
        let area = Rect::new(0, 0, width, rows_u16);
        let mut buffer = Buffer::empty(area);
        Paragraph::new(lines.clone())
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .render(area, &mut buffer);
        for row in 0..rows_u16 {
            let local_visual = offset.saturating_add(usize::from(row));
            let visual_row = visual_start.saturating_add(local_visual);
            let Some(logical) = item.logical_at_local_visual(local_visual) else {
                continue;
            };
            let Some(source) = item.rows.get(logical) else {
                continue;
            };
            append_row(
                &mut symbols,
                &mut haystack,
                &buffer,
                row,
                width,
                visual_row,
                source.selection,
            );
        }
        offset = offset.saturating_add(usize::from(rows_u16));
    }
    Some(ItemStream {
        symbols,
        haystack,
        width,
    })
}

fn append_row(
    symbols: &mut Vec<Symbol>,
    haystack: &mut String,
    buffer: &Buffer,
    row: u16,
    width: u16,
    visual_row: usize,
    selection: SelectionPolicy,
) {
    let columns = match selection {
        SelectionPolicy::Keep => 0..width,
        SelectionPolicy::Skip => return,
        SelectionPolicy::StripPrefix(prefix) => prefix..width,
    };
    if columns.start >= columns.end {
        return;
    }
    let mut gathered = Vec::new();
    let mut column = 0;
    while column < width {
        let symbol = buffer[(column, row)].symbol();
        let next = next_column(column, symbol, width);
        if !symbol.is_empty() && column < columns.end && next > columns.start {
            gathered.push((column, next, symbol.to_owned()));
        }
        column = next;
    }
    if columns.end == width {
        while gathered.last().is_some_and(|(_, _, symbol)| symbol == " ") {
            gathered.pop();
        }
    }
    for (start, end, symbol) in gathered {
        let byte = haystack.len();
        haystack.push_str(&symbol);
        symbols.push(Symbol {
            byte,
            len: symbol.len(),
            visual_row,
            start,
            end,
        });
    }
}

fn spans_for(stream: &ItemStream, range: &Range<usize>) -> Vec<CellSpan> {
    let mut cells: Vec<CellSpan> = Vec::new();
    for symbol in &stream.symbols {
        if symbol.byte >= range.end || symbol.byte.saturating_add(symbol.len) <= range.start {
            continue;
        }
        if let Some(last) = cells.last_mut()
            && last.visual_row == symbol.visual_row
            && last.end == symbol.start
        {
            last.end = symbol.end;
        } else {
            cells.push(CellSpan {
                visual_row: symbol.visual_row,
                start: symbol.start,
                end: symbol.end,
            });
        }
    }
    cells
}

fn occurrence_at(
    stream: &ItemStream,
    ranges: &[Range<usize>],
    visual_row: usize,
    column: u16,
) -> usize {
    if ranges.is_empty() {
        return 0;
    }
    if let Some(index) = ranges.iter().position(|range| {
        stream.symbols.iter().any(|symbol| {
            symbol.byte < range.end
                && symbol.byte.saturating_add(symbol.len) > range.start
                && symbol.visual_row == visual_row
                && column >= symbol.start
                && column < symbol.end
        })
    }) {
        return index;
    }
    let mut after = None;
    let mut before = None;
    for (index, range) in ranges.iter().enumerate() {
        let Some(first) = stream
            .symbols
            .iter()
            .find(|symbol| symbol.byte >= range.start && symbol.byte < range.end)
        else {
            continue;
        };
        let starts_after = first.visual_row > visual_row
            || (first.visual_row == visual_row && first.start >= column);
        if starts_after {
            after.get_or_insert(index);
        } else {
            before = Some(index);
        }
    }
    after.or(before).unwrap_or(0)
}

fn match_ranges(haystack: &str, needle: &str) -> Vec<Range<usize>> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut ranges = Vec::new();
    let mut from = 0;
    while let Some(found) = haystack[from..].find(needle) {
        let start = from.saturating_add(found);
        let end = start.saturating_add(needle.len());
        ranges.push(start..end);
        from = end;
    }
    ranges
}

fn needle(quote: &str) -> String {
    quote.replace('\n', "")
}

fn badge_hit(mark: &PlacedMark, visual_row: usize, column: u16) -> bool {
    mark.badge.as_ref().is_some_and(|span| {
        span.visual_row == visual_row && column >= span.start && column < span.end
    })
}

/// Number sits in the blank cells after a quote that ends the visible line.
fn badge_span(stream: &ItemStream, cells: &[CellSpan], number: Option<usize>) -> Option<CellSpan> {
    let number = number?;
    let last = cells.last()?;
    let content_end = stream
        .symbols
        .iter()
        .filter(|symbol| symbol.visual_row == last.visual_row)
        .map(|symbol| symbol.end)
        .max()
        .unwrap_or(last.end);
    if last.end < content_end || last.end >= stream.width {
        return None;
    }
    let label = badge_label(number, stream.width.saturating_sub(last.end));
    let text_width = u16::try_from(UnicodeWidthStr::width(label.as_str())).ok()?;
    if text_width == 0 {
        return None;
    }
    Some(CellSpan {
        visual_row: last.visual_row,
        start: last.end,
        end: last.end.saturating_add(text_width),
    })
}

fn paint_span(buffer: &mut Buffer, area: Rect, origin: usize, cell: &CellSpan, style: Style) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(local) = cell.visual_row.checked_sub(origin) else {
        return;
    };
    if local >= usize::from(area.height) {
        return;
    }
    let y = area
        .y
        .saturating_add(u16::try_from(local).unwrap_or(u16::MAX));
    if y >= area.bottom() {
        return;
    }
    let mut column = cell.start;
    while column < cell.end {
        let x = area.x.saturating_add(column);
        if x >= area.right() {
            break;
        }
        buffer[(x, y)].set_style(style);
        column = column.saturating_add(1);
    }
}

fn paint_badge(
    buffer: &mut Buffer,
    state: &TuiState,
    area: Rect,
    origin: usize,
    mark: &PlacedMark,
) {
    let Some(span) = &mark.badge else {
        return;
    };
    let Some(local) = span.visual_row.checked_sub(origin) else {
        return;
    };
    if local >= usize::from(area.height) || area.width == 0 || span.start >= area.width {
        return;
    }
    let y = area
        .y
        .saturating_add(u16::try_from(local).unwrap_or(u16::MAX));
    let Some(number) = mark.number else {
        return;
    };
    let room = area.width.saturating_sub(span.start);
    let text = badge_label(number, room);
    let style = mark_style(state, mark.pending).add_modifier(Modifier::BOLD);
    let mut column = span.start;
    for character in text.chars() {
        if column >= area.width {
            break;
        }
        let x = area.x.saturating_add(column);
        if x >= area.right() || y >= area.bottom() {
            break;
        }
        buffer[(x, y)].set_symbol(&character.to_string());
        buffer[(x, y)].set_style(style);
        column = column.saturating_add(1);
    }
}

fn badge_label(number: usize, room: u16) -> String {
    let text = number.to_string();
    let width = UnicodeWidthStr::width(text.as_str());
    if width <= usize::from(room) {
        text
    } else if room >= 1 {
        "#".to_owned()
    } else {
        String::new()
    }
}

fn mark_style(state: &TuiState, pending: bool) -> Style {
    if pending {
        semantic_style(state, SemanticColor::Focus)
            .add_modifier(Modifier::UNDERLINED)
            .add_modifier(Modifier::BOLD)
    } else if let Some(color) = state.theme().color(SemanticColor::Selection) {
        Style::default()
            .bg(color)
            .add_modifier(Modifier::UNDERLINED)
    } else {
        Style::default().add_modifier(Modifier::UNDERLINED)
    }
}

fn document_cell(
    split: &TimelineScrollSplit,
    header: Rect,
    body: Rect,
    position: Position,
) -> Option<(usize, u16, bool)> {
    if body.contains(position) {
        return Some((
            split
                .body_scroll
                .saturating_add(usize::from(position.y.saturating_sub(body.y))),
            position.x.saturating_sub(body.x),
            false,
        ));
    }
    let sticky = sticky_rect(header, split);
    if let Some(origin) = split.pinned_origin
        && sticky.contains(position)
    {
        return Some((
            origin.saturating_add(usize::from(position.y.saturating_sub(sticky.y))),
            position.x.saturating_sub(sticky.x),
            true,
        ));
    }
    None
}

fn sticky_rect(header: Rect, split: &TimelineScrollSplit) -> Rect {
    Rect {
        x: header.x,
        y: header.y,
        width: header.width,
        height: split.content_rows.min(header.height),
    }
}

fn next_column(column: u16, symbol: &str, width: u16) -> u16 {
    let symbol_width = u16::try_from(UnicodeWidthStr::width(symbol))
        .unwrap_or(u16::MAX)
        .max(1);
    column.saturating_add(symbol_width).min(width)
}
