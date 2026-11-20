//! Live thinking blocks and their animated vertical gutter rail.
//!
//! Independent single-line commands use horizontal shimmer instead of a
//! vertical gutter bar; only multi-line Thinking blocks paint a left rail.

use super::timeline_cache::TimelineLayoutSnapshot;
use crate::tui::{
    motion::{self, MotionClock},
    state::{TimelineItem, TuiState},
    text_wrap::{StyledTextPart, semantic_style, wrap_styled_parts},
    theme::SemanticColor,
};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use std::time::Duration;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const THINKING_PREVIEW_LINES: usize = 3;

/// Header plus the live tail, or a one-line duration once thinking has finished.
pub(super) fn thinking_lines(
    state: &TuiState,
    text: &str,
    started_at: tokio::time::Instant,
    elapsed: Option<Duration>,
    expanded: bool,
    region_width: u16,
) -> Vec<Line<'static>> {
    let header = thinking_header(state, started_at, elapsed, region_width);
    if elapsed.is_some() && !expanded {
        return vec![header];
    }
    let limit = elapsed.is_none().then_some(THINKING_PREVIEW_LINES);
    let max_reading = state.max_reading_width();
    let body_width = if max_reading == 0 {
        region_width
    } else {
        region_width.min(max_reading)
    }
    .saturating_sub(2)
    .max(1);
    let mut lines = vec![header];
    lines.extend(thinking_body(state, text, body_width, limit));
    lines
}

pub(super) fn paint_activity(
    frame: &mut Frame<'_>,
    state: &TuiState,
    content: Rect,
    timeline: &TimelineLayoutSnapshot,
    scroll: usize,
) {
    if content.x == 0 || content.height == 0 {
        return;
    }
    let clock = MotionClock::now();
    for rail in activity_rails(state, content, timeline, scroll) {
        let y = content.y.saturating_add(rail.local_y);
        let (symbol, style) =
            rail_cell(state, clock, rail.phase_row, rail.total_rows, rail.animated);
        let cell = &mut frame.buffer_mut()[(content.x - 1, y)];
        cell.set_symbol(symbol);
        cell.set_style(style);
    }
    let fold_style = semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD);
    for fold in thinking_folds(state, content, timeline, scroll) {
        let y = content.y.saturating_add(fold.local_y);
        let cell = &mut frame.buffer_mut()[(content.x - 1, y)];
        cell.set_symbol(if fold.open { "▾" } else { "▸" });
        cell.set_style(fold_style);
    }
}

/// Opens a finished thinking header. A running block stays open, so it is not a hit.
pub(super) fn thinking_fold_hit(
    state: &TuiState,
    content: Rect,
    timeline: &TimelineLayoutSnapshot,
    scroll: usize,
    position: Position,
) -> Option<usize> {
    if content.height == 0 || position.y < content.y || position.y >= content.bottom() {
        return None;
    }
    let in_gutter = content.x > 0 && position.x + 1 == content.x;
    let in_content = content.contains(position);
    if !in_gutter && !in_content {
        return None;
    }
    let visual = scroll.saturating_add(usize::from(position.y - content.y));
    let index = timeline.item_at_visual(visual)?;
    let open = thinking_open(state.timeline().get(index)?)?;
    let first_visual = timeline.item_visual_starts.get(index).copied()?;
    if visual != first_visual || (open && !in_gutter) {
        return None;
    }
    Some(index)
}

fn thinking_header(
    state: &TuiState,
    started_at: tokio::time::Instant,
    elapsed: Option<Duration>,
    width: u16,
) -> Line<'static> {
    let label = semantic_style(state, SemanticColor::ToolKeyword).add_modifier(Modifier::BOLD);
    let detail = semantic_style(state, SemanticColor::Muted);
    let line = if let Some(elapsed) = elapsed {
        let time = format_thought_duration(elapsed);
        Line::from(vec![
            Span::styled("Thought".to_owned(), label),
            Span::styled(format!(" for {time}"), detail),
        ])
    } else {
        let running_elapsed = started_at.elapsed();
        let clock = MotionClock::now();
        let spinner = motion::spinner_glyph(running_elapsed);
        let mut spans = motion::shimmer_text(
            &format!("{spinner} Thinking…"),
            label,
            motion::SHIMMER_HIGHLIGHT,
            clock,
            state.animations_enabled(),
        );
        if running_elapsed >= Duration::from_millis(100) {
            spans.push(Span::styled(
                format!("  {}", format_thought_duration(running_elapsed)),
                detail,
            ));
        }
        Line::from(spans)
    };
    truncate_line(line, width)
}

fn format_thought_duration(elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64();
    if secs < 60.0 {
        return format!("{secs:.1}s");
    }
    let mins = (secs / 60.0).floor();
    let mins = u32::try_from(mins as u64).unwrap_or(u32::MAX);
    let remaining = secs - f64::from(mins) * 60.0;
    format!("{mins}m{remaining:.0}s")
}

fn thinking_body(
    state: &TuiState,
    text: &str,
    region_width: u16,
    limit: Option<usize>,
) -> Vec<Line<'static>> {
    let style = semantic_style(state, SemanticColor::TextSecondary).add_modifier(Modifier::ITALIC);
    let mut wrapped = Vec::new();
    for paragraph in text.split('\n') {
        let paragraph = paragraph.trim_end_matches('\r');
        if paragraph.is_empty() {
            continue;
        }
        wrapped.extend(wrap_styled_parts(
            vec![StyledTextPart {
                text: paragraph.to_owned(),
                style,
                atomic: false,
            }],
            region_width,
        ));
    }
    let indent = |mut line: Line<'static>| {
        line.spans.insert(0, Span::raw("  "));
        line
    };
    let Some(limit) = limit else {
        return wrapped.into_iter().map(indent).collect();
    };
    if wrapped.len() <= limit {
        return wrapped.into_iter().map(indent).collect();
    }
    let mut lines = vec![Line::from(Span::styled(
        "  …".to_owned(),
        semantic_style(state, SemanticColor::Muted),
    ))];
    let skip = wrapped.len().saturating_sub(limit);
    lines.extend(wrapped.into_iter().skip(skip).map(indent));
    lines
}

fn truncate_line(line: Line<'static>, width: u16) -> Line<'static> {
    let max_width = usize::from(width);
    if max_width == 0 {
        return Line::from("");
    }
    if line_width(&line) <= max_width {
        return line;
    }
    let mut kept = Vec::new();
    let mut used = 0_usize;
    for span in line.spans {
        let span_width = UnicodeWidthStr::width(span.content.as_ref());
        if used.saturating_add(span_width) <= max_width {
            used = used.saturating_add(span_width);
            kept.push(span);
            continue;
        }
        let room = max_width.saturating_sub(used);
        let text = truncate_to_width(span.content.as_ref(), room);
        if !text.is_empty() {
            kept.push(Span::styled(text, span.style));
        }
        break;
    }
    Line::from(kept)
}

fn truncate_to_width(text: &str, max_width: usize) -> String {
    let mut width = 0_usize;
    let mut end = 0_usize;
    for (index, character) in text.char_indices() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if width.saturating_add(character_width) > max_width {
            break;
        }
        width = width.saturating_add(character_width);
        end = index.saturating_add(character.len_utf8());
    }
    text[..end].to_owned()
}

fn line_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

struct RailRow {
    local_y: u16,
    phase_row: u16,
    total_rows: u16,
    animated: bool,
}

struct FoldMark {
    local_y: u16,
    open: bool,
}

fn activity_rails(
    state: &TuiState,
    content: Rect,
    timeline: &TimelineLayoutSnapshot,
    scroll: usize,
) -> Vec<RailRow> {
    let mut rails = Vec::new();
    let range_end = scroll.saturating_add(usize::from(content.height));
    let first_item = timeline
        .item_visual_starts
        .partition_point(|start| *start <= scroll)
        .saturating_sub(1);
    for (index, item) in state.timeline().iter().enumerate().skip(first_item) {
        let Some(&item_start) = timeline.item_visual_starts.get(index) else {
            break;
        };
        if item_start >= range_end {
            break;
        }
        let Some(animated) = rail_animation(item) else {
            continue;
        };
        let Some((visual_start, visual_end)) = timeline.item_content_visual_range(index) else {
            continue;
        };
        let total_rows = u16::try_from(visual_end.saturating_sub(visual_start)).unwrap_or(u16::MAX);
        let start = visual_start.max(scroll);
        let end = visual_end.min(range_end);
        for visual in start..end {
            let local = visual - scroll;
            let Ok(local_y) = u16::try_from(local) else {
                continue;
            };
            let Ok(phase_row) = u16::try_from(visual - visual_start) else {
                continue;
            };
            rails.push(RailRow {
                local_y,
                phase_row,
                total_rows,
                animated,
            });
        }
    }
    rails
}

fn thinking_folds(
    state: &TuiState,
    content: Rect,
    timeline: &TimelineLayoutSnapshot,
    scroll: usize,
) -> Vec<FoldMark> {
    let mut folds = Vec::new();
    let range_end = scroll.saturating_add(usize::from(content.height));
    let first_item = timeline
        .item_visual_starts
        .partition_point(|start| *start <= scroll)
        .saturating_sub(1);
    for (index, item) in state.timeline().iter().enumerate().skip(first_item) {
        let Some(&visual) = timeline.item_visual_starts.get(index) else {
            break;
        };
        if visual >= range_end {
            break;
        }
        if visual < scroll {
            continue;
        }
        let Some(open) = thinking_open(item) else {
            continue;
        };
        let Ok(local_y) = u16::try_from(visual - scroll) else {
            continue;
        };
        folds.push(FoldMark { local_y, open });
    }
    folds
}

/// `Some(true)` animates. `Some(false)` is a static rail on an opened thought.
/// Independent commands do not use a vertical rail (they use horizontal shimmer).
fn rail_animation(item: &TimelineItem) -> Option<bool> {
    match item {
        TimelineItem::Thinking { elapsed: None, .. } => Some(true),
        TimelineItem::Thinking {
            elapsed: Some(_),
            expanded: true,
            ..
        } => Some(false),
        _ => None,
    }
}

/// `Some(true)` is an open finished thought. Running thoughts are not foldable.
fn thinking_open(item: &TimelineItem) -> Option<bool> {
    match item {
        TimelineItem::Thinking {
            elapsed: Some(_),
            expanded,
            ..
        } => Some(*expanded),
        _ => None,
    }
}

fn rail_cell(
    state: &TuiState,
    clock: MotionClock,
    row: u16,
    total_rows: u16,
    animated: bool,
) -> (&'static str, Style) {
    let base = state
        .theme()
        .color(SemanticColor::Border)
        .unwrap_or(Color::DarkGray);
    if !animated {
        return ("│", Style::default().fg(base));
    }
    let accent = state
        .theme()
        .color(SemanticColor::Focus)
        .unwrap_or(Color::LightMagenta);
    let trail = state
        .theme()
        .color(SemanticColor::ToolKeyword)
        .unwrap_or(Color::LightCyan);
    motion::flow_rail_cell(
        row,
        total_rows,
        base,
        accent,
        trail,
        clock,
        state.animations_enabled(),
    )
}

#[cfg(test)]
mod tests {
    use super::rail_animation;
    use crate::tui::state::{CommandView, TimelineItem};
    use std::time::Duration;
    use tokio::time::Instant;

    #[test]
    fn vertical_rail_applies_to_thinking_only_not_independent_commands() {
        assert_eq!(
            rail_animation(&TimelineItem::Thinking {
                text: "working".to_owned(),
                started_at: Instant::now(),
                elapsed: None,
                expanded: false,
            }),
            Some(true)
        );
        assert_eq!(
            rail_animation(&TimelineItem::Thinking {
                text: "done".to_owned(),
                started_at: Instant::now(),
                elapsed: Some(Duration::from_secs(1)),
                expanded: true,
            }),
            Some(false)
        );
        assert_eq!(
            rail_animation(&TimelineItem::Command {
                view: CommandView::Running {
                    detail: "cargo test".to_owned(),
                    started_at: Instant::now(),
                    live_preview: crate::tui::state::LiveCommandOutput::default(),
                    expanded: false,
                },
            }),
            None
        );
    }
}
