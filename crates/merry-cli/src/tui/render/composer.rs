//! Composer sizing, surface-styled input (Issue #70), completions, and queued-input presentation.

use crate::tui::{
    keymap::KeyAction,
    render::{HEADER_HEIGHT, STATUS_HEIGHT},
    state::{CommandView, TimelineItem, TuiState},
    text_wrap::{semantic_style, truncate_chars},
    theme::SemanticColor,
};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};
use unicode_width::UnicodeWidthStr;

pub(super) const MAX_COMPLETION_PREVIEW_HEIGHT: u16 = 8;

pub(super) const MAX_INPUT_VISIBLE_ROWS: usize = 5;

pub(super) const MIN_TIMELINE_HEIGHT: u16 = 3;

pub(super) const MIN_INPUT_HEIGHT: u16 = 3;

/// Left indent + prompt (`❯ `) prefix width + right padding.
/// At standard widths (`>= 40`), text starts at col 4 — vertically aligned with
/// the `🎀` mark on the top hairline (` ── 🎀 ───`).
fn composer_horizontal_insets(width: u16) -> (u16, u16, u16) {
    // Returns (left_indent, prompt_width, right_pad).
    if width >= 40 {
        // cols 0..2: 2-space indent, cols 2..4: "❯ ", col 4: text start
        (2, 2, 2)
    } else if width >= 16 {
        // col 0: 1-space indent, cols 1..3: "❯ ", col 3: text start
        (1, 2, 1)
    } else {
        (0, 1, 0)
    }
}

/// Keep the composer compact (top hairline + editable rows + bottom hairline)
/// so it never forms a hollow 5-row cavern above the status bar.
fn composer_uses_vertical_breathing(_total_height: u16) -> bool {
    false
}

/// Single source of truth for the editable text viewport inside `region`.
pub(crate) fn composer_text_rect_for_region(region: Rect, total_height: u16) -> Rect {
    composer_text_rect(region, composer_uses_vertical_breathing(total_height))
}

pub(crate) fn composer_text_rect(region: Rect, vertical_breathing: bool) -> Rect {
    if region.width == 0 || region.height < 3 {
        return Rect::default();
    }
    let (left_margin, prefix_width, right_pad) = composer_horizontal_insets(region.width);
    let left = left_margin.saturating_add(prefix_width);
    let inner_width = region.width.saturating_sub(left.saturating_add(right_pad));
    let v_pad = if vertical_breathing && region.height >= 5 {
        1
    } else {
        0
    };
    let top_offset = 1u16.saturating_add(v_pad);
    let bottom_reserved = 1u16.saturating_add(v_pad);
    let inner_height = region
        .height
        .saturating_sub(top_offset.saturating_add(bottom_reserved));
    Rect {
        x: region.x.saturating_add(left),
        y: region.y.saturating_add(top_offset),
        width: inner_width,
        height: inner_height,
    }
}

pub(crate) fn composer_inner_columns(total_width: u16) -> usize {
    let (left_margin, prefix_width, right_pad) = composer_horizontal_insets(total_width);
    usize::from(
        total_width
            .saturating_sub(
                left_margin
                    .saturating_add(prefix_width)
                    .saturating_add(right_pad),
            )
            .max(1),
    )
}

pub(crate) fn pane_heights_for_area(state: &TuiState, area: Rect) -> PaneHeights {
    pane_heights(state, area.height, area.width)
}

pub(super) fn render_input(
    frame: &mut Frame<'_>,
    state: &TuiState,
    region: Rect,
    total_height: u16,
) {
    if region.width == 0 || region.height == 0 {
        return;
    }
    let breathing = composer_uses_vertical_breathing(total_height);
    let text_rect = composer_text_rect(region, breathing);
    let surface_bg = state.theme().bg_color(SemanticColor::SurfaceSubtle);
    let surface_style = surface_bg.map_or_else(Style::default, |bg| Style::default().bg(bg));
    let border_style = semantic_style(state, SemanticColor::Border);
    let prompt_color = if state.is_commenting() || state.plan().is_plan_mode_on() {
        SemanticColor::Warning
    } else {
        SemanticColor::Focus
    };
    let prompt_style = semantic_style(state, prompt_color)
        .add_modifier(Modifier::BOLD)
        .patch(surface_style);

    // 1. Top hairline with embedded composer title / mode badge.
    render_top_hairline(
        frame,
        state,
        Rect::new(region.x, region.y, region.width, 1),
        border_style,
    );

    // 2. Surface band with clean `❯` prompt and input rows.
    if region.height >= 3 {
        let band = Rect {
            x: region.x,
            y: region.y.saturating_add(1),
            width: region.width,
            height: region.height.saturating_sub(2),
        };
        frame.render_widget(Block::default().style(surface_style), band);

        let (_, prefix_width, _) = composer_horizontal_insets(region.width);

        if !text_rect.is_empty() {
            let max_input_rows = usize::from(text_rect.height).max(1);
            // Render prompt glyph `❯` directly before the first editable text row.
            if prefix_width >= 2 && text_rect.x >= 2 {
                let prompt_x = text_rect.x.saturating_sub(2);
                let cell = &mut frame.buffer_mut()[(prompt_x, text_rect.y)];
                cell.set_symbol("❯");
                cell.set_style(prompt_style);
            }

            if let Some(search) = state.history_search() {
                if search.query().is_empty() {
                    render_composer_placeholder(
                        frame,
                        state,
                        text_rect,
                        "Type to filter history…",
                        surface_style,
                    );
                } else {
                    let query_viewport = search
                        .query_input()
                        .viewport_rows(usize::from(text_rect.width), max_input_rows);
                    let query_lines = styled_input_lines(
                        state,
                        &query_viewport.text,
                        &query_viewport.image_placeholders,
                        &query_viewport.row_selections,
                        surface_style,
                    );
                    frame.render_widget(
                        Paragraph::new(query_lines).style(
                            semantic_style(state, SemanticColor::Assistant).patch(surface_style),
                        ),
                        text_rect,
                    );
                    if !state.is_editing_pending() {
                        set_input_cursor(
                            frame,
                            text_rect,
                            query_viewport.cursor_column,
                            query_viewport.cursor_row,
                        );
                    }
                }
            } else if state.plain_input_text() == Some("") {
                let hint = if state.is_commenting() {
                    "Add comment…"
                } else if state.plan().is_plan_mode_on() {
                    "Plan Mode (read-only) — describe what to plan…"
                } else {
                    "Message Merry…"
                };
                render_composer_placeholder(frame, state, text_rect, hint, surface_style);
            } else {
                let input_viewport =
                    state.input_viewport_rows(usize::from(text_rect.width), max_input_rows);
                let input_lines = styled_input_lines(
                    state,
                    &input_viewport.text,
                    &input_viewport.image_placeholders,
                    &input_viewport.row_selections,
                    surface_style,
                );
                frame.render_widget(
                    Paragraph::new(input_lines).style(
                        semantic_style(state, SemanticColor::Assistant).patch(surface_style),
                    ),
                    text_rect,
                );
                if !state.is_editing_pending() {
                    set_input_cursor(
                        frame,
                        text_rect,
                        input_viewport.cursor_column,
                        input_viewport.cursor_row,
                    );
                }
            }
        }
    }

    // 3. Bottom hairline separating composer from the status bar.
    if region.height >= 2 {
        let bottom_y = region.bottom().saturating_sub(1);
        let margin = usize::from(if region.width >= 40 { 1u16 } else { 0u16 });
        let rule_len = usize::from(region.width).saturating_sub(margin.saturating_mul(2));
        let line = Line::from(vec![
            Span::raw(" ".repeat(margin)),
            Span::styled("─".repeat(rule_len), border_style),
        ]);
        frame.render_widget(
            Paragraph::new(line),
            Rect::new(region.x, bottom_y, region.width, 1),
        );
    }
}

fn render_composer_placeholder(
    frame: &mut Frame<'_>,
    state: &TuiState,
    text_rect: Rect,
    text: &str,
    surface_style: Style,
) {
    let placeholder = truncate_chars(text, usize::from(text_rect.width));
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            placeholder,
            semantic_style(state, SemanticColor::Muted)
                .add_modifier(Modifier::DIM)
                .patch(surface_style),
        )))
        .style(surface_style),
        text_rect,
    );
    if !state.is_editing_pending() {
        set_input_cursor(frame, text_rect, 0, 0);
    }
}

fn render_top_hairline(frame: &mut Frame<'_>, state: &TuiState, area: Rect, border_style: Style) {
    if area.is_empty() {
        return;
    }
    let width = usize::from(area.width);
    let margin = usize::from(if area.width >= 40 { 1u16 } else { 0u16 });
    let title = state.composer_title(area.width);
    let trimmed = title.trim();
    let title_style = semantic_style(state, SemanticColor::Status).add_modifier(Modifier::BOLD);
    let hint_style = semantic_style(state, SemanticColor::Muted);

    let review_hint = if state.is_timeline_detached() {
        let key = state.keymap().binding_label_for(KeyAction::FollowLatest);
        let label = if state.timeline_has_updates() {
            "New content"
        } else {
            "Reviewing"
        };
        let suffix = key.map(|k| format!(" · {k} latest")).unwrap_or_default();
        Some(format!("{label}{suffix}"))
    } else {
        None
    };
    let output_hint = (state.overlay().is_none()
        && state.timeline().iter().rev().any(|item| {
            matches!(
                item,
                TimelineItem::Command {
                    view: CommandView::Finished { .. }
                }
            )
        }))
    .then(|| {
        state
            .keymap()
            .binding_label_for(KeyAction::OpenCommandDetails)
            .map(|key| format!("{key} output"))
    })
    .flatten();

    let mut spans = Vec::new();
    let mut used = 0usize;
    if margin > 0 && width > margin * 2 {
        spans.push(Span::raw(" ".repeat(margin)));
        used += margin;
    }
    if width >= used + 4 + UnicodeWidthStr::width(trimmed) + margin {
        spans.push(Span::styled("── ", border_style));
        spans.push(Span::styled(trimmed.to_owned(), title_style));
        spans.push(Span::styled(" ", border_style));
        used += 4 + UnicodeWidthStr::width(trimmed);
    } else if !trimmed.is_empty() && width >= used + UnicodeWidthStr::width(trimmed) {
        spans.push(Span::styled(trimmed.to_owned(), title_style));
        used += UnicodeWidthStr::width(trimmed);
    }

    let tail = width.saturating_sub(used.saturating_add(margin));
    let history_hint = state
        .history_search_active()
        .then_some("↑↓ select · Enter use · Esc cancel".to_owned());
    let chosen_hint = if let Some(hint) = history_hint {
        (UnicodeWidthStr::width(hint.as_str()) + 2 <= tail).then_some(hint)
    } else {
        match (review_hint, output_hint) {
            (Some(review), Some(output)) => {
                let combined = format!("{review} · {output}");
                if UnicodeWidthStr::width(combined.as_str()) + 2 <= tail {
                    Some(combined)
                } else if UnicodeWidthStr::width(review.as_str()) + 2 <= tail {
                    Some(review)
                } else {
                    None
                }
            }
            (Some(review), None) => {
                (UnicodeWidthStr::width(review.as_str()) + 2 <= tail).then_some(review)
            }
            (None, Some(output)) => {
                (UnicodeWidthStr::width(output.as_str()) + 2 <= tail).then_some(output)
            }
            (None, None) => None,
        }
    };

    if let Some(hint) = chosen_hint {
        let hint_w = UnicodeWidthStr::width(hint.as_str());
        let trailing_dashes = if tail >= hint_w + 4 { 2 } else { 0 };
        let mid = tail.saturating_sub(hint_w + 2 + trailing_dashes);
        if mid > 0 {
            spans.push(Span::styled("─".repeat(mid), border_style));
        }
        spans.push(Span::styled(" ", border_style));
        spans.push(Span::styled(hint, hint_style));
        spans.push(Span::styled(" ", border_style));
        if trailing_dashes > 0 {
            spans.push(Span::styled("─".repeat(trailing_dashes), border_style));
        }
    } else if tail > 0 {
        spans.push(Span::styled("─".repeat(tail), border_style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub(crate) fn styled_input_lines(
    state: &TuiState,
    text: &str,
    image_placeholders: &[String],
    row_selections: &[Option<std::ops::Range<usize>>],
    surface_style: Style,
) -> Vec<Line<'static>> {
    text.split('\n')
        .enumerate()
        .map(|(index, line)| {
            let selection = row_selections.get(index).and_then(|sel| sel.as_ref());
            styled_input_line(state, line, image_placeholders, selection, surface_style)
        })
        .collect()
}

pub(super) fn styled_input_line(
    state: &TuiState,
    line: &str,
    image_placeholders: &[String],
    selection: Option<&std::ops::Range<usize>>,
    surface_style: Style,
) -> Line<'static> {
    let normal = semantic_style(state, SemanticColor::Assistant).patch(surface_style);
    let image = semantic_style(state, SemanticColor::Status)
        .add_modifier(Modifier::BOLD)
        .patch(surface_style);
    let selected_patch = match state.theme().bg_color(SemanticColor::SelectionBackground) {
        Some(bg) => Style::default().bg(bg),
        None => Style::default().add_modifier(Modifier::REVERSED),
    };
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
            push_selection_aware_spans(
                &mut spans,
                line,
                cursor..line.len(),
                normal,
                selected_patch,
                selection,
            );
            break;
        };

        if start > cursor {
            push_selection_aware_spans(
                &mut spans,
                line,
                cursor..start,
                normal,
                selected_patch,
                selection,
            );
        }
        push_selection_aware_spans(
            &mut spans,
            line,
            start..start + placeholder.len(),
            image,
            selected_patch,
            selection,
        );
        cursor = start + placeholder.len();
    }

    if spans.is_empty() {
        spans.push(Span::styled(String::new(), normal));
    }
    Line::from(spans)
}

fn push_selection_aware_spans(
    spans: &mut Vec<Span<'static>>,
    line: &str,
    range: std::ops::Range<usize>,
    base_style: Style,
    selected_patch: Style,
    selection: Option<&std::ops::Range<usize>>,
) {
    if range.start >= range.end {
        return;
    }
    let Some(sel) = selection else {
        spans.push(Span::styled(line[range].to_owned(), base_style));
        return;
    };
    let ov_start = range.start.max(sel.start);
    let ov_end = range.end.min(sel.end);
    if ov_start >= ov_end {
        spans.push(Span::styled(line[range].to_owned(), base_style));
        return;
    }
    if range.start < ov_start {
        spans.push(Span::styled(
            line[range.start..ov_start].to_owned(),
            base_style,
        ));
    }
    spans.push(Span::styled(
        line[ov_start..ov_end].to_owned(),
        base_style.patch(selected_patch),
    ));
    if ov_end < range.end {
        spans.push(Span::styled(line[ov_end..range.end].to_owned(), base_style));
    }
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

pub(super) fn pane_heights(
    state: &TuiState,
    total_height: u16,
    terminal_width: u16,
) -> PaneHeights {
    let desired_queue = desired_queue_preview_height(state);
    let desired_completion = desired_completion_preview_height(state);
    let inner_width = composer_inner_columns(terminal_width);
    let breathing = composer_uses_vertical_breathing(total_height);
    let desired_input =
        desired_input_region_height(state, MAX_INPUT_VISIBLE_ROWS, inner_width, breathing);
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
    let compact_desired =
        desired_input_region_height(state, MAX_INPUT_VISIBLE_ROWS, inner_width, false);
    let input = compact_desired
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
    super::pending_queue::desired_pending_queue_height(state)
}

pub(super) fn desired_completion_preview_height(state: &TuiState) -> u16 {
    if let Some(search) = state.history_search() {
        return u16::try_from(search.matches().len())
            .unwrap_or(MAX_COMPLETION_PREVIEW_HEIGHT)
            .clamp(1, MAX_COMPLETION_PREVIEW_HEIGHT);
    }
    state
        .completion_menu()
        .map(|menu| {
            u16::try_from(menu.items().len())
                .unwrap_or(MAX_COMPLETION_PREVIEW_HEIGHT)
                .min(MAX_COMPLETION_PREVIEW_HEIGHT)
        })
        .unwrap_or(0)
}

pub(super) fn desired_input_region_height(
    state: &TuiState,
    max_rows: usize,
    inner_width: usize,
    vertical_breathing: bool,
) -> u16 {
    let visible_rows = if let Some(search) = state.history_search() {
        search
            .query_input()
            .viewport_rows(inner_width, max_rows)
            .visible_rows
    } else {
        state
            .input_viewport_rows(inner_width, max_rows)
            .visible_rows
    };
    let chrome = if vertical_breathing { 4 } else { 2 };
    u16::try_from(visible_rows)
        .unwrap_or(u16::MAX)
        .saturating_add(chrome)
}

pub(super) fn completion_lines(state: &TuiState, region: Rect) -> Vec<Line<'static>> {
    if state.history_search_active() {
        return history_search_lines(state, region);
    }
    let Some(menu) = state.completion_menu() else {
        return Vec::new();
    };
    let surface_bg = state.theme().bg_color(SemanticColor::SurfaceSubtle);
    let selected_bg = state.theme().bg_color(SemanticColor::SelectionBackground);
    let (start, end) = menu.visible_window(usize::from(region.height));
    let total_items = menu.items().len();
    let show_counter = total_items > (end - start);
    let pad = if region.width >= 40 { "  " } else { "" };
    let label_width = UnicodeWidthStr::width(pad) + 2;
    let content_width = usize::from(region.width).saturating_sub(label_width);
    let has_any_detail = menu
        .items()
        .iter()
        .any(|item| item.detail().is_some_and(|detail| !detail.is_empty()));
    // Measure across the entire menu so the description column never shifts as
    // the user scrolls through items of varying name lengths.
    let value_col_width = if has_any_detail {
        menu.items()
            .iter()
            .map(|item| {
                UnicodeWidthStr::width(item.value())
                    + item.suffix().map_or(0, UnicodeWidthStr::width)
            })
            .max()
            .unwrap_or(12)
            .min(content_width / 2)
    } else {
        content_width
    };

    menu.items()[start..end]
        .iter()
        .enumerate()
        .map(|(offset, item)| {
            let index = start + offset;
            let selected = index == menu.selected_index();
            let row_bg = if selected { selected_bg } else { surface_bg };
            let with_bg = |style: Style| row_bg.map_or(style, |bg| style.bg(bg));
            let base_style = with_bg(if selected {
                semantic_style(state, SemanticColor::Assistant).add_modifier(Modifier::BOLD)
            } else {
                semantic_style(state, SemanticColor::TextSecondary)
            });
            let highlight_style =
                with_bg(semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD));
            let suffix_style = with_bg(semantic_style(state, SemanticColor::Muted));
            let detail_style = with_bg(semantic_style(state, SemanticColor::Muted));
            let marker = if selected { "> " } else { "  " };
            let label_text = format!("{pad}{marker}");

            let counter_text = if selected && show_counter {
                format!(" {}/{total_items}", index + 1)
            } else {
                String::new()
            };
            let counter_width = UnicodeWidthStr::width(counter_text.as_str());
            let row_content_width = content_width.saturating_sub(counter_width);

            let col1_budget = if has_any_detail {
                value_col_width.min(row_content_width)
            } else {
                row_content_width
            };
            let value_text = truncate_chars(item.value(), col1_budget);
            let value_width = UnicodeWidthStr::width(value_text.as_str());
            let suffix_budget = col1_budget.saturating_sub(value_width);
            let suffix_text = item
                .suffix()
                .filter(|suffix| !suffix.is_empty() && suffix_budget > 0)
                .map(|suffix| truncate_chars(suffix, suffix_budget))
                .unwrap_or_default();
            let col1_used = value_width + UnicodeWidthStr::width(suffix_text.as_str());

            let detail_pad =
                if has_any_detail && item.detail().is_some_and(|detail| !detail.is_empty()) {
                    col1_budget.saturating_sub(col1_used) + 2
                } else {
                    0
                };
            let detail_budget = row_content_width
                .saturating_sub(col1_used)
                .saturating_sub(detail_pad);
            let detail_text = item
                .detail()
                .filter(|detail| !detail.is_empty() && detail_budget > 0)
                .map(|detail| {
                    format!(
                        "{}{}",
                        " ".repeat(detail_pad),
                        truncate_chars(detail, detail_budget)
                    )
                })
                .unwrap_or_default();

            let used = UnicodeWidthStr::width(label_text.as_str())
                + col1_used
                + UnicodeWidthStr::width(detail_text.as_str())
                + counter_width;
            let trailing = " ".repeat(usize::from(region.width).saturating_sub(used));

            let mut spans = vec![Span::styled(label_text, highlight_style)];
            crate::tui::skills_overlay::push_position_highlighted_spans(
                &mut spans,
                &value_text,
                item.match_positions(),
                base_style,
                highlight_style,
            );
            if !suffix_text.is_empty() {
                spans.push(Span::styled(suffix_text, suffix_style));
            }
            spans.push(Span::styled(detail_text, detail_style));
            spans.push(Span::styled(trailing, detail_style));
            if !counter_text.is_empty() {
                spans.push(Span::styled(counter_text, detail_style));
            }
            Line::from(spans)
        })
        .collect()
}

fn history_search_lines(state: &TuiState, region: Rect) -> Vec<Line<'static>> {
    let Some(search) = state.history_search() else {
        return Vec::new();
    };
    let surface_bg = state.theme().bg_color(SemanticColor::SurfaceSubtle);
    let selected_bg = state.theme().bg_color(SemanticColor::SelectionBackground);
    let pad = if region.width >= 40 { "  " } else { "" };
    let total_width = usize::from(region.width);

    if search.matches().is_empty() {
        let style = surface_bg.map_or_else(
            || semantic_style(state, SemanticColor::Muted).add_modifier(Modifier::DIM),
            |bg| {
                semantic_style(state, SemanticColor::Muted)
                    .add_modifier(Modifier::DIM)
                    .bg(bg)
            },
        );
        let text = truncate_chars(&format!("{pad}  No matching history entries"), total_width);
        let trailing =
            " ".repeat(total_width.saturating_sub(UnicodeWidthStr::width(text.as_str())));
        return vec![Line::from(vec![
            Span::styled(text, style),
            Span::styled(trailing, style),
        ])];
    }

    let entries = state.history_search_entries();
    let max_rows = usize::from(region.height).max(1);
    let (start, end) = search.visible_window(max_rows);
    let query_lower = search.query().to_lowercase();

    (start..end)
        .map(|match_pos| {
            let selected = match_pos == search.selected_pos();
            let row_bg = if selected { selected_bg } else { surface_bg };
            let with_bg = |style: Style| row_bg.map_or(style, |bg| style.bg(bg));
            let marker_style = with_bg(if selected {
                semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD)
            } else {
                semantic_style(state, SemanticColor::Muted)
            });
            let text_style = with_bg(if selected {
                semantic_style(state, SemanticColor::Assistant).add_modifier(Modifier::BOLD)
            } else {
                semantic_style(state, SemanticColor::TextSecondary)
            });
            let highlight_style =
                with_bg(semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD));

            let marker = if selected { "> " } else { "  " };
            let prefix = format!("{pad}{marker}");
            let content_width = total_width.saturating_sub(UnicodeWidthStr::width(prefix.as_str()));
            let raw = entries
                .get(search.matches()[match_pos])
                .map(String::as_str)
                .unwrap_or("");
            let collapsed = raw.replace('\n', " ↵ ");
            let shown = truncate_chars(&collapsed, content_width);
            let used =
                UnicodeWidthStr::width(prefix.as_str()) + UnicodeWidthStr::width(shown.as_str());
            let trailing = " ".repeat(total_width.saturating_sub(used));

            let mut spans = vec![Span::styled(prefix, marker_style)];
            push_highlighted_query_spans(
                &mut spans,
                &shown,
                &query_lower,
                text_style,
                highlight_style,
            );
            spans.push(Span::styled(trailing, text_style));
            Line::from(spans)
        })
        .collect()
}

fn push_highlighted_query_spans(
    spans: &mut Vec<Span<'static>>,
    text: &str,
    query_lower: &str,
    normal_style: Style,
    highlight_style: Style,
) {
    if query_lower.is_empty() {
        spans.push(Span::styled(text.to_owned(), normal_style));
        return;
    }
    let lowered = text.to_lowercase();
    // Guard against Unicode case-folding length changes so byte slices stay valid.
    if lowered.len() != text.len() {
        spans.push(Span::styled(text.to_owned(), normal_style));
        return;
    }
    let mut cursor = 0;
    while cursor < text.len() {
        let Some(offset) = lowered[cursor..].find(query_lower) else {
            spans.push(Span::styled(text[cursor..].to_owned(), normal_style));
            break;
        };
        let start = cursor + offset;
        let end = start + query_lower.len();
        if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            spans.push(Span::styled(text[cursor..].to_owned(), normal_style));
            break;
        }
        if start > cursor {
            spans.push(Span::styled(text[cursor..start].to_owned(), normal_style));
        }
        spans.push(Span::styled(text[start..end].to_owned(), highlight_style));
        cursor = end;
    }
}
