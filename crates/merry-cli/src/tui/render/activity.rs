//! Live thinking blocks and the gutter wave shared with running commands.
//!
//! The wave matches Grok's `sin²` rail: one cycle is about a third of a second
//! when the tick advances like a 60fps frame clock. Merry repaints an active
//! run every 100ms, so the tick is wall-clock milliseconds divided by 16.

use crate::tui::{
    state::{CommandView, TimelineItem, TuiState},
    text_wrap::{StyledTextPart, semantic_style, wrap_styled_parts},
    theme::SemanticColor,
};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const THINKING_PREVIEW_LINES: usize = 3;
const WAVE_SPEED: f32 = 0.15;
const WAVE_ROWS: u16 = 8;
const ACCENT_BAR: &str = "┃";

/// Geometry shared by the activity rail and the thinking fold hit test.
pub(super) struct ActivityLayout<'a> {
    pub(super) item_starts: &'a [usize],
    pub(super) row_starts: &'a [usize],
    pub(super) blank_lines: &'a [bool],
}

/// Header plus the live tail, or a one-line duration once thinking has finished.
pub(super) fn thinking_lines(
    state: &TuiState,
    text: &str,
    elapsed: Option<Duration>,
    expanded: bool,
    region_width: u16,
) -> Vec<Line<'static>> {
    let header = thinking_header(state, elapsed, region_width);
    if elapsed.is_some() && !expanded {
        return vec![header];
    }
    let limit = elapsed.is_none().then_some(THINKING_PREVIEW_LINES);
    let mut lines = vec![header];
    lines.extend(thinking_body(state, text, region_width, limit));
    lines
}

pub(super) fn paint_activity(
    frame: &mut Frame<'_>,
    state: &TuiState,
    content: Rect,
    layout: ActivityLayout<'_>,
    scroll: usize,
) {
    if content.x == 0 || content.height == 0 {
        return;
    }
    let tick = activity_tick();
    for rail in activity_rails(state, content, &layout, scroll) {
        let y = content.y.saturating_add(rail.local_y);
        let cell = &mut frame.buffer_mut()[(content.x - 1, y)];
        cell.set_symbol(ACCENT_BAR);
        cell.set_style(rail_style(state, tick, rail.phase_row, rail.animated));
    }
    let fold_style = semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD);
    for fold in thinking_folds(state, content, &layout, scroll) {
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
    layout: &ActivityLayout<'_>,
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
    let logical = logical_line_at_visual(layout.row_starts, visual)?;
    let index = item_index_at_logical(layout.item_starts, logical)?;
    if layout.item_starts.get(index).copied() != Some(logical) {
        return None;
    }
    let open = thinking_open(state.timeline().get(index)?)?;
    let first_visual = layout.row_starts.get(logical).copied()?;
    if visual != first_visual || (open && !in_gutter) {
        return None;
    }
    Some(index)
}

fn thinking_header(state: &TuiState, elapsed: Option<Duration>, width: u16) -> Line<'static> {
    let label = semantic_style(state, SemanticColor::ToolKeyword).add_modifier(Modifier::BOLD);
    let detail = semantic_style(state, SemanticColor::Muted);
    let line = if let Some(elapsed) = elapsed {
        let time = format_thought_duration(elapsed);
        Line::from(vec![
            Span::styled("Thought".to_owned(), label),
            Span::styled(format!(" for {time}"), detail),
        ])
    } else {
        Line::from(Span::styled("Thinking…".to_owned(), label))
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
    let style = semantic_style(state, SemanticColor::Assistant);
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
    let Some(limit) = limit else {
        return wrapped;
    };
    if wrapped.len() <= limit {
        return wrapped;
    }
    let mut lines = vec![Line::from(Span::styled(
        "…".to_owned(),
        semantic_style(state, SemanticColor::Muted),
    ))];
    let skip = wrapped.len().saturating_sub(limit);
    lines.extend(wrapped.into_iter().skip(skip));
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
    animated: bool,
}

struct FoldMark {
    local_y: u16,
    open: bool,
}

fn activity_rails(
    state: &TuiState,
    content: Rect,
    layout: &ActivityLayout<'_>,
    scroll: usize,
) -> Vec<RailRow> {
    let mut rails = Vec::new();
    for (index, item) in state.timeline().iter().enumerate() {
        let Some(animated) = rail_animation(item) else {
            continue;
        };
        let Some((visual_start, visual_end)) = item_visual_range(index, layout) else {
            continue;
        };
        for visual in visual_start..visual_end {
            if visual < scroll {
                continue;
            }
            let local = visual - scroll;
            if local >= usize::from(content.height) {
                break;
            }
            let Ok(local_y) = u16::try_from(local) else {
                continue;
            };
            let Ok(phase_row) = u16::try_from(visual - visual_start) else {
                continue;
            };
            rails.push(RailRow {
                local_y,
                phase_row,
                animated,
            });
        }
    }
    rails
}

fn thinking_folds(
    state: &TuiState,
    content: Rect,
    layout: &ActivityLayout<'_>,
    scroll: usize,
) -> Vec<FoldMark> {
    let mut folds = Vec::new();
    for (index, item) in state.timeline().iter().enumerate() {
        let Some(open) = thinking_open(item) else {
            continue;
        };
        let Some(&logical) = layout.item_starts.get(index) else {
            continue;
        };
        let Some(&visual) = layout.row_starts.get(logical) else {
            continue;
        };
        if visual < scroll {
            continue;
        }
        let local = visual - scroll;
        if local >= usize::from(content.height) {
            continue;
        }
        let Ok(local_y) = u16::try_from(local) else {
            continue;
        };
        folds.push(FoldMark { local_y, open });
    }
    folds
}

/// `Some(true)` animates. `Some(false)` is a static rail on an opened thought.
fn rail_animation(item: &TimelineItem) -> Option<bool> {
    match item {
        TimelineItem::Thinking { elapsed: None, .. } => Some(true),
        TimelineItem::Thinking {
            elapsed: Some(_),
            expanded: true,
            ..
        } => Some(false),
        TimelineItem::Command {
            view: CommandView::Running { .. },
        } => Some(true),
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

fn item_visual_range(index: usize, layout: &ActivityLayout<'_>) -> Option<(usize, usize)> {
    let start_logical = *layout.item_starts.get(index)?;
    let mut end_logical = layout
        .item_starts
        .get(index + 1)
        .copied()
        .unwrap_or(layout.blank_lines.len());
    while end_logical > start_logical
        && layout
            .blank_lines
            .get(end_logical - 1)
            .copied()
            .unwrap_or(true)
    {
        end_logical -= 1;
    }
    if end_logical <= start_logical {
        return None;
    }
    let visual_start = *layout.row_starts.get(start_logical)?;
    let visual_end = *layout.row_starts.get(end_logical)?;
    (visual_start < visual_end).then_some((visual_start, visual_end))
}

fn rail_style(state: &TuiState, tick: u64, row: u16, animated: bool) -> Style {
    let accent = state
        .theme()
        .color(SemanticColor::ToolKeyword)
        .unwrap_or(Color::LightCyan);
    if !animated {
        return Style::default().fg(accent);
    }
    let Some(accent_rgb) = color_rgb(accent) else {
        return Style::default().fg(accent);
    };
    let background = state
        .theme()
        .color(SemanticColor::CodeBackground)
        .unwrap_or(Color::Black);
    let Some(background_rgb) = color_rgb(background) else {
        return Style::default().fg(accent);
    };
    let brightness = wave_brightness(tick, row);
    Style::default().fg(Color::Rgb(
        blend_channel(background_rgb.0, accent_rgb.0, brightness),
        blend_channel(background_rgb.1, accent_rgb.1, brightness),
        blend_channel(background_rgb.2, accent_rgb.2, brightness),
    ))
}

fn activity_tick() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0);
    u64::try_from(millis / 16).unwrap_or(u64::MAX)
}

/// `sin²(tick * 0.15 + (row / wave_rows) * 2π)`, in `0.0..=1.0`.
fn wave_brightness(tick: u64, row: u16) -> f32 {
    let rows_per_wave = f32::from(WAVE_ROWS.max(1));
    let phase = (f32::from(row) / rows_per_wave) * 2.0 * std::f32::consts::PI;
    let sample = (tick as f32).mul_add(WAVE_SPEED, phase).sin();
    sample * sample
}

fn blend_channel(from: u8, to: u8, brightness: f32) -> u8 {
    let brightness = brightness.clamp(0.0, 1.0);
    let mixed = f32::from(from).mul_add(1.0 - brightness, f32::from(to) * brightness);
    let rounded = mixed.round().clamp(0.0, 255.0);
    if rounded <= 0.0 {
        0
    } else if rounded >= 255.0 {
        255
    } else {
        // `rounded` is inside 1..255, so the narrowing cast preserves the value.
        let value = rounded as u16;
        u8::try_from(value).unwrap_or(255)
    }
}

fn color_rgb(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Rgb(red, green, blue) => Some((red, green, blue)),
        Color::Black => Some((0, 0, 0)),
        Color::Red => Some((205, 0, 0)),
        Color::Green => Some((0, 205, 0)),
        Color::Yellow => Some((205, 205, 0)),
        Color::Blue => Some((0, 0, 238)),
        Color::Magenta => Some((205, 0, 205)),
        Color::Cyan => Some((0, 205, 205)),
        Color::Gray => Some((229, 229, 229)),
        Color::DarkGray => Some((127, 127, 127)),
        Color::LightRed => Some((255, 0, 0)),
        Color::LightGreen => Some((0, 255, 0)),
        Color::LightYellow => Some((255, 255, 0)),
        Color::LightBlue => Some((92, 92, 255)),
        Color::LightMagenta => Some((255, 0, 255)),
        Color::LightCyan => Some((0, 255, 255)),
        Color::White => Some((255, 255, 255)),
        Color::Indexed(_) | Color::Reset => None,
    }
}

fn logical_line_at_visual(row_starts: &[usize], visual: usize) -> Option<usize> {
    let end = *row_starts.last()?;
    if visual >= end {
        return None;
    }
    Some(row_starts.partition_point(|start| *start <= visual) - 1)
}

fn item_index_at_logical(item_starts: &[usize], logical: usize) -> Option<usize> {
    if item_starts.is_empty() || logical < item_starts[0] {
        return None;
    }
    Some(item_starts.partition_point(|start| *start <= logical) - 1)
}

#[cfg(test)]
mod tests {
    use super::wave_brightness;

    #[test]
    fn wave_brightness_follows_the_grok_phase() {
        assert!(wave_brightness(0, 0) < 0.01);
        assert!((wave_brightness(0, 2) - 1.0).abs() < 0.02);
        assert!(wave_brightness(0, 4) < 0.02);
    }
}
