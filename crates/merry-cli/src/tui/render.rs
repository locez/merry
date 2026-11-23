//! Coordinates focused renderers over the single TUI state snapshot.

use crate::{
    config::HeaderItemKind,
    tui::{
        click_target::{
            chip_line, status_action_label, status_actions, status_chips, truncate_columns,
        },
        layout::{BottomPaneHeights, cockpit_layout},
        motion::{self, MotionClock},
        overlay_render,
        render::composer::{completion_lines, composer_inner_columns, pane_heights, render_input},
        state::TuiState,
        status::build_header_layout,
        text_wrap::semantic_style,
        theme::SemanticColor,
    },
};
pub(crate) use comment_marks::{comment_mark_at, quote_occurrence};
pub(crate) use composer::{
    composer_text_rect_for_region, pane_heights_for_area, styled_input_lines,
};
#[cfg(test)]
use ratatui::layout::Position;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};
use std::time::Instant;
pub(crate) use timeline::{prepare_timeline_viewport, render_timeline_pane};
#[cfg(test)]
pub(crate) use timeline_layout::timeline_content_region;
pub(crate) use timeline_pointer::{
    TimelineMouseDown, timeline_mouse_down, timeline_task_dock_rect, timeline_text_area,
    timeline_todo_dock_rect,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

mod activity;
mod command_rows;
mod command_style;
mod comment_marks;
mod mcp_style;
pub(crate) use pending_queue::{PendingQueuePointer, pending_queue_pointer_at};
mod pending_queue;
mod sticky;
pub(crate) mod task_dock;

mod composer;

mod timeline;
pub(crate) mod timeline_cache;
mod timeline_layout;
mod timeline_pointer;

pub(crate) const STATUS_HEIGHT: u16 = 1;

const HEADER_HEIGHT: u16 = 1;

/// Captures the visible timeline anchor before drawing or processing subsequent updates.
pub(crate) fn prepare_viewport(state: &mut TuiState, size: ratatui::layout::Size) {
    state.set_composer_columns(composer_inner_columns(size.width));
    let area = Rect::new(0, 0, size.width, size.height);
    super::command_details::prepare_viewport(state, area);
    super::plan_view::prepare_viewport(state, area);
    let heights = pane_heights_for_area(state, area);
    let rects = cockpit_layout(
        area,
        BottomPaneHeights {
            queue: heights.queue,
            completion: heights.completion,
            input: heights.input,
            status: STATUS_HEIGHT,
        },
    );
    let selection_matches_size = state.overlay().is_none()
        && state
            .text_selection()
            .is_some_and(|selection| selection.terminal_size() == size);
    if !selection_matches_size {
        state.validate_text_selection_area(timeline_pointer::timeline_text_area(
            state,
            rects.timeline,
        ));
    }
    if state.text_selection().is_none() {
        timeline::prepare_timeline_viewport(state, rects.timeline);
    }
}

pub(crate) fn render(frame: &mut Frame<'_>, state: &TuiState) {
    let pane_heights = pane_heights(state, frame.area().height, frame.area().width);
    let rects = cockpit_layout(
        frame.area(),
        BottomPaneHeights {
            queue: pane_heights.queue,
            completion: pane_heights.completion,
            input: pane_heights.input,
            status: STATUS_HEIGHT,
        },
    );

    if let Some(bg) = state.theme().bg_color(SemanticColor::Background) {
        frame.render_widget(
            Block::default().style(Style::default().bg(bg)),
            frame.area(),
        );
    }

    render_header(frame, state, rects.header);
    if rects.timeline.width > 0 && rects.timeline.height > 0 {
        render_timeline_pane(frame, state, rects.timeline);
    }

    if let Some(queue_region) = rects.queue {
        pending_queue::render_pending_queue(frame, state, queue_region);
    }
    if pane_heights.completion > 0 {
        frame.render_widget(
            Paragraph::new(completion_lines(state, rects.completion)),
            rects.completion,
        );
    }
    render_input(frame, state, rects.input, frame.area().height);
    render_status(frame, state, rects.status);
    overlay_render::render_overlay(frame, state);
}

#[cfg(test)]
pub(crate) fn render_to_text(state: &TuiState, width: u16, height: u16) -> String {
    let buffer = render_to_buffer(state, width, height);
    let area = buffer.area;
    let mut text = String::new();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

#[cfg(test)]
pub(crate) fn render_to_buffer(
    state: &TuiState,
    width: u16,
    height: u16,
) -> ratatui::buffer::Buffer {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal should build");
    terminal
        .draw(|frame| render(frame, state))
        .expect("test render should draw");

    terminal.backend().buffer().clone()
}

#[cfg(test)]
pub(crate) fn render_to_buffer_and_cursor(
    state: &TuiState,
    width: u16,
    height: u16,
) -> (ratatui::buffer::Buffer, Position) {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal should build");
    terminal
        .draw(|frame| render(frame, state))
        .expect("test render should draw");

    (
        terminal.backend().buffer().clone(),
        terminal.backend().cursor_position(),
    )
}

fn render_header(frame: &mut Frame<'_>, state: &TuiState, region: Rect) {
    if region.is_empty() {
        return;
    }
    let model = state.model_display_label();
    let rate = state.output_rate_label();
    let layout = build_header_layout(
        state.workspace_root(),
        &model,
        state.usage(),
        &rate,
        state.is_active_run(),
        state.header_items(),
        region.width,
    );
    let bg_style = header_background_style(state);
    let sep_style = semantic_style(state, SemanticColor::Muted).patch(bg_style);
    let mut spans = Vec::new();
    if layout.left_pad > 0 {
        spans.push(Span::styled(
            " ".repeat(usize::from(layout.left_pad)),
            bg_style,
        ));
    }
    spans.push(Span::styled(
        crate::tui::status::HEADER_BRAND_MARK,
        semantic_style(state, SemanticColor::Status)
            .add_modifier(Modifier::BOLD)
            .patch(bg_style),
    ));
    let mut in_telemetry = false;
    for segment in layout.segments {
        let is_telemetry = matches!(
            segment.kind,
            HeaderItemKind::Context
                | HeaderItemKind::Cache
                | HeaderItemKind::Rate
                | HeaderItemKind::Tokens
        );
        if is_telemetry && in_telemetry {
            spans.push(Span::styled(" · ", sep_style));
        } else {
            spans.push(Span::styled("  ", bg_style));
        }
        if is_telemetry {
            in_telemetry = true;
        }
        let mut style = semantic_style(state, segment.color).patch(bg_style);
        if segment.bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if segment.underlined {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        if segment.dim {
            style = style.add_modifier(Modifier::DIM);
        }
        spans.push(Span::styled(segment.text, style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(bg_style), region);
}

fn header_background_style(state: &TuiState) -> Style {
    state
        .theme()
        .bg_color(SemanticColor::HeaderBackground)
        .map_or_else(Style::default, |color| Style::default().bg(color))
}

fn status_background_style(state: &TuiState) -> Style {
    state
        .theme()
        .bg_color(SemanticColor::StatusBackground)
        .map_or_else(Style::default, |color| Style::default().bg(color))
}

fn render_status(frame: &mut Frame<'_>, state: &TuiState, region: Rect) {
    if region.is_empty() {
        return;
    }
    let bg_style = status_background_style(state);
    frame.render_widget(Block::default().style(bg_style), region);

    let left_pad = if region.width >= 40 { " " } else { "" };
    if let Some(feedback) = state.clipboard_feedback() {
        let color = if feedback.failed {
            SemanticColor::Error
        } else {
            SemanticColor::Focus
        };
        let text = format!("{left_pad}{}", feedback.message);
        frame.render_widget(
            Paragraph::new(truncate_columns(&text, usize::from(region.width)))
                .style(semantic_style(state, color).patch(bg_style)),
            region,
        );
        return;
    }

    let chips = status_chips(state, region, &status_actions(state));
    let text_limit = chips
        .first()
        .map_or(usize::from(region.width), |(_, chip)| {
            usize::from(chip.x.saturating_sub(region.x).saturating_sub(1))
        });
    let text_width = u16::try_from(text_limit).unwrap_or(0).min(region.width);
    if text_width > 0 {
        let clock = MotionClock::now();
        let mut spans = Vec::new();
        if !left_pad.is_empty() {
            spans.push(Span::styled(left_pad, bg_style));
        }
        if state.is_active_run() && state.history_search_status().is_none() {
            spans.extend(motion::brand_status_spans(
                state.active_phase_label(),
                state.theme(),
                clock,
                state.animations_enabled(),
            ));
            if let Some(elapsed) = state.active_run_elapsed_label(Instant::now()) {
                spans.push(Span::styled(
                    format!(" · {elapsed}"),
                    semantic_style(state, SemanticColor::TextSecondary).patch(bg_style),
                ));
            }
        } else {
            spans.extend(motion::brand_idle_spans(state.theme()));
            let text_style = if state.is_commenting() {
                semantic_style(state, SemanticColor::Warning)
                    .add_modifier(Modifier::BOLD)
                    .patch(bg_style)
            } else {
                semantic_style(state, SemanticColor::TextSecondary).patch(bg_style)
            };
            spans.push(Span::styled(state.interaction_status_text(), text_style));
        }
        let fitted = truncate_status_spans(spans, usize::from(text_width));
        frame.render_widget(
            Paragraph::new(Line::from(fitted)).style(bg_style),
            Rect {
                width: text_width,
                ..region
            },
        );
    }
    if let Some((_, first)) = chips.first() {
        let labels = chips
            .iter()
            .map(|(action, _)| status_action_label(state, action))
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(chip_line(state, &labels)).style(bg_style),
            Rect {
                x: first.x,
                y: region.y,
                width: region.right().saturating_sub(first.x),
                height: 1,
            },
        );
    }
}

fn truncate_status_spans(spans: Vec<Span<'static>>, max_width: usize) -> Vec<Span<'static>> {
    let mut kept = Vec::new();
    let mut used = 0usize;
    for span in spans {
        let width = UnicodeWidthStr::width(span.content.as_ref());
        if used.saturating_add(width) <= max_width {
            used = used.saturating_add(width);
            kept.push(span);
            continue;
        }
        let room = max_width.saturating_sub(used);
        if room > 0 {
            let mut out = String::new();
            let mut w = 0usize;
            let limit = room.saturating_sub(1);
            for ch in span.content.chars() {
                let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                if w + cw > limit {
                    break;
                }
                w += cw;
                out.push(ch);
            }
            if room >= 1 {
                out.push('…');
            }
            kept.push(Span::styled(out, span.style));
        }
        break;
    }
    kept
}
