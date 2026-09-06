//! Composer sizing, input, completions, and queued-input presentation.

use crate::tui::{
    render::{HEADER_HEIGHT, STATUS_HEIGHT, bordered_inner},
    state::TuiState,
    text_wrap::{semantic_style, truncate_chars},
    theme::SemanticColor,
};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
};

pub(super) const QUEUE_PREVIEW_HEIGHT: u16 = 5;

pub(super) const MAX_COMPLETION_PREVIEW_HEIGHT: u16 = 6;

pub(super) const MAX_INPUT_VISIBLE_ROWS: usize = 5;

pub(super) const MIN_TIMELINE_HEIGHT: u16 = 3;

pub(super) const MIN_INPUT_HEIGHT: u16 = 3;

pub(crate) fn pane_heights_for_area(state: &TuiState, area: Rect) -> PaneHeights {
    pane_heights(state, area.height)
}

pub(super) fn render_input(
    frame: &mut Frame<'_>,
    state: &TuiState,
    region: Rect,
    input_height: u16,
) {
    let input_inner = bordered_inner(region);
    let max_input_rows = usize::from(input_height.saturating_sub(2)).max(1);
    let input_viewport = state.input_viewport_rows(usize::from(input_inner.width), max_input_rows);
    let input_lines = styled_input_lines(
        state,
        &input_viewport.text,
        &input_viewport.image_placeholders,
    );

    frame.render_widget(
        Paragraph::new(input_lines)
            .style(semantic_style(state, SemanticColor::Assistant))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Plain)
                    .title(Line::from(Span::styled(
                        " M ",
                        semantic_style(state, SemanticColor::Status).add_modifier(Modifier::BOLD),
                    )))
                    .border_style(semantic_style(state, SemanticColor::Focus)),
            ),
        region,
    );
    set_input_cursor(
        frame,
        input_inner,
        input_viewport.cursor_column,
        input_viewport.cursor_row,
    );
}

pub(super) fn styled_input_lines(
    state: &TuiState,
    text: &str,
    image_placeholders: &[String],
) -> Vec<Line<'static>> {
    text.split('\n')
        .map(|line| styled_input_line(state, line, image_placeholders))
        .collect()
}

pub(super) fn styled_input_line(
    state: &TuiState,
    line: &str,
    image_placeholders: &[String],
) -> Line<'static> {
    let normal = semantic_style(state, SemanticColor::Assistant);
    let image = semantic_style(state, SemanticColor::Status).add_modifier(Modifier::BOLD);
    let mut spans = Vec::new();
    let mut cursor = 0;

    while cursor < line.len() {
        let Some((start, placeholder)) = image_placeholders
            .iter()
            .filter_map(|placeholder| {
                line[cursor..]
                    .find(placeholder)
                    .map(|offset| (cursor + offset, placeholder.as_str()))
            })
            .min_by_key(|(start, _)| *start)
        else {
            spans.push(Span::styled(line[cursor..].to_owned(), normal));
            break;
        };

        if start > cursor {
            spans.push(Span::styled(line[cursor..start].to_owned(), normal));
        }
        spans.push(Span::styled(placeholder.to_owned(), image));
        cursor = start + placeholder.len();
    }

    if spans.is_empty() {
        spans.push(Span::styled(String::new(), normal));
    }
    Line::from(spans)
}

pub(super) fn set_input_cursor(
    frame: &mut Frame<'_>,
    region: Rect,
    cursor_column: usize,
    cursor_row: usize,
) {
    if region.width == 0 || region.height == 0 {
        return;
    }
    let cursor_column = u16::try_from(cursor_column).unwrap_or(u16::MAX);
    let cursor_row = u16::try_from(cursor_row).unwrap_or(u16::MAX);
    let max_x = region.x.saturating_add(region.width.saturating_sub(1));
    let max_y = region.y.saturating_add(region.height.saturating_sub(1));
    frame.set_cursor_position(Position {
        x: region.x.saturating_add(cursor_column).min(max_x),
        y: region.y.saturating_add(cursor_row).min(max_y),
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneHeights {
    pub(crate) queue: u16,
    pub(crate) completion: u16,
    pub(crate) input: u16,
}

pub(super) fn pane_heights(state: &TuiState, total_height: u16) -> PaneHeights {
    let desired_queue = desired_queue_preview_height(state);
    let desired_completion = desired_completion_preview_height(state);
    let desired_input = desired_input_region_height(state, MAX_INPUT_VISIBLE_ROWS);
    let desired_bottom = desired_queue
        .saturating_add(desired_completion)
        .saturating_add(desired_input)
        .saturating_add(STATUS_HEIGHT)
        .saturating_add(HEADER_HEIGHT);

    if total_height >= desired_bottom.saturating_add(MIN_TIMELINE_HEIGHT) {
        return PaneHeights {
            queue: desired_queue,
            completion: desired_completion,
            input: desired_input,
        };
    }

    let reserved = HEADER_HEIGHT
        .saturating_add(STATUS_HEIGHT)
        .saturating_add(MIN_TIMELINE_HEIGHT);
    let mut remaining = total_height.saturating_sub(reserved);
    let input = desired_input
        .min(remaining)
        .max(MIN_INPUT_HEIGHT.min(remaining));
    remaining = remaining.saturating_sub(input);

    let completion = desired_completion.min(remaining);
    remaining = remaining.saturating_sub(completion);

    let queue = desired_queue.min(remaining);
    PaneHeights {
        queue,
        completion,
        input,
    }
}

pub(super) fn desired_queue_preview_height(state: &TuiState) -> u16 {
    if state.has_queue_preview_items() {
        QUEUE_PREVIEW_HEIGHT
    } else {
        0
    }
}

pub(super) fn desired_completion_preview_height(state: &TuiState) -> u16 {
    state
        .completion_menu()
        .map(|menu| {
            u16::try_from(menu.items().len())
                .unwrap_or(MAX_COMPLETION_PREVIEW_HEIGHT)
                .min(MAX_COMPLETION_PREVIEW_HEIGHT)
        })
        .unwrap_or(0)
}

pub(super) fn desired_input_region_height(state: &TuiState, max_rows: usize) -> u16 {
    let visible_rows = state.input_visible_rows(max_rows);
    u16::try_from(visible_rows)
        .unwrap_or(u16::MAX)
        .saturating_add(2)
}

pub(super) fn queue_lines(state: &TuiState, region: Rect) -> Vec<Line<'static>> {
    let queue = state.queue_preview();
    vec![
        queue_lane(state, "Next", &queue.next, region.width),
        queue_lane(state, "Suspended", &queue.suspended, region.width),
        queue_lane(state, "Backlog", &queue.backlog, region.width),
    ]
}

pub(super) fn completion_lines(state: &TuiState, region: Rect) -> Vec<Line<'static>> {
    let Some(menu) = state.completion_menu() else {
        return Vec::new();
    };
    menu.items()
        .iter()
        .take(usize::from(region.height))
        .enumerate()
        .map(|(index, item)| {
            let selected = index == menu.selected_index();
            let base_style = if selected {
                semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD)
            } else {
                semantic_style(state, SemanticColor::Muted)
            };
            let marker = if selected { ">" } else { " " };
            let label_text = format!("{marker} ");
            let content_width =
                usize::from(region.width).saturating_sub(label_text.chars().count());
            let detail_width = if item.detail().is_some_and(|detail| !detail.is_empty()) {
                content_width / 2
            } else {
                0
            };
            let detail_text = item
                .detail()
                .filter(|detail| !detail.is_empty() && detail_width > 2)
                .map(|detail| format!("  {}", truncate_chars(detail, detail_width - 2)))
                .unwrap_or_default();
            let value_width = content_width.saturating_sub(detail_text.chars().count());
            Line::from(vec![
                Span::styled(label_text, base_style),
                Span::styled(truncate_chars(item.value(), value_width), base_style),
                Span::styled(detail_text, semantic_style(state, SemanticColor::Muted)),
            ])
        })
        .collect()
}

pub(super) fn queue_lane(
    state: &TuiState,
    label: &'static str,
    items: &[crate::tui::state::QueuePreviewItem],
    region_width: u16,
) -> Line<'static> {
    let label_text = format!("{label:<10} ");
    let content_width = usize::from(region_width).saturating_sub(label_text.chars().count());
    let content = if items.is_empty() {
        "--".to_owned()
    } else {
        items
            .iter()
            .enumerate()
            .map(|(index, item)| format!("{}. {}", index + 1, item.text))
            .collect::<Vec<_>>()
            .join(" | ")
    };
    Line::from(vec![
        Span::styled(
            label_text,
            semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            truncate_chars(&content, content_width),
            semantic_style(state, SemanticColor::Muted),
        ),
    ])
}
