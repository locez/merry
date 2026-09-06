//! Coordinates focused renderers over the single TUI state snapshot.

use crate::tui::{
    layout::{BottomPaneHeights, cockpit_layout},
    overlay_render, plan_render,
    render::{
        composer::{completion_lines, pane_heights, queue_lines, render_input},
        timeline::render_timeline_pane,
    },
    state::TuiState,
    text_wrap::semantic_style,
    theme::{SemanticColor, dim_color},
};
pub(crate) use composer::pane_heights_for_area;
#[cfg(test)]
use ratatui::layout::Position;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};

mod command_style;

mod composer;

mod timeline;

pub(crate) const STATUS_HEIGHT: u16 = 1;

const HEADER_HEIGHT: u16 = 1;

pub(crate) fn render(frame: &mut Frame<'_>, state: &TuiState) {
    let pane_heights = pane_heights(state, frame.area().height);
    let rects = cockpit_layout(
        frame.area(),
        BottomPaneHeights {
            queue: pane_heights.queue,
            completion: pane_heights.completion,
            input: pane_heights.input,
            status: STATUS_HEIGHT,
        },
        state.plan().is_open(),
        state.plan().is_focused(),
    );

    render_header(frame, state, rects.header);
    if rects.timeline.width > 0 && rects.timeline.height > 0 {
        render_timeline_pane(frame, state, rects.timeline);
    }
    if let Some(region) = rects.plan {
        plan_render::render_plan(frame, state, region);
    }

    if let Some(queue_region) = rects.queue {
        frame.render_widget(
            Paragraph::new(queue_lines(state, queue_region)).block(
                Block::default()
                    .title("queue")
                    .border_style(semantic_style(state, SemanticColor::Muted)),
            ),
            queue_region,
        );
    }
    if pane_heights.completion > 0 {
        frame.render_widget(
            Paragraph::new(completion_lines(state, rects.completion)),
            rects.completion,
        );
    }
    render_input(frame, state, rects.input, pane_heights.input);
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

fn bordered_inner(region: Rect) -> Rect {
    Rect {
        x: region.x.saturating_add(1),
        y: region.y.saturating_add(1),
        width: region.width.saturating_sub(2),
        height: region.height.saturating_sub(2),
    }
}

fn render_header(frame: &mut Frame<'_>, state: &TuiState, region: Rect) {
    let [workspace, model, usage] = state.header_status_parts(region.width);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "merry",
                semantic_style(state, SemanticColor::Status).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ", Style::default()),
            Span::styled(workspace, semantic_style(state, SemanticColor::Command)),
            Span::styled("  ", Style::default()),
            Span::styled(
                model,
                semantic_style(state, SemanticColor::ToolKeyword).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ", Style::default()),
            Span::styled(
                usage,
                semantic_style(state, SemanticColor::Assistant).add_modifier(Modifier::DIM),
            ),
        ]))
        .style(header_background_style(state)),
        region,
    );
}

fn header_background_style(state: &TuiState) -> Style {
    state
        .theme()
        .color(SemanticColor::Status)
        .map(dim_color)
        .map_or_else(Style::default, |color| Style::default().bg(color))
}

fn render_status(frame: &mut Frame<'_>, state: &TuiState, region: Rect) {
    let interaction_style = if state.is_active_run() {
        semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD)
    } else {
        semantic_style(state, SemanticColor::Muted)
    };
    frame.render_widget(
        Paragraph::new(state.interaction_status_text()).style(interaction_style),
        region,
    );
}
