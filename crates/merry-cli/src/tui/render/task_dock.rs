//! Rendering and pointer hit-testing for the persistent Background Tasks &
//! Subagents dock (anchored directly below the top/sticky user box) and the
//! interactive Background Process Output inspector modal.

use crate::tui::{
    click_target::truncate_columns,
    modal_frame::{modal_rects, modal_surface_style, render_modal_frame},
    motion::{self, MotionClock},
    state::TuiState,
    task_dock::{
        BackgroundTaskOverlay, BackgroundTaskPointer, MAX_DOCK_VISIBLE_ROWS, TaskDockEntry,
        TaskDockFilter,
    },
    text_wrap::{semantic_style, truncate_chars},
    theme::SemanticColor,
};
use merry_runtime::BackgroundProcessState;
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, Paragraph, Wrap},
};
use std::time::Duration;
use unicode_width::UnicodeWidthStr;

/// Pointer action resolved from a click inside the inline Tasks & Subagents dock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TaskDockPointer {
    ToggleExpanded,
    SelectFilter(TaskDockFilter),
    InspectBackgroundProcess(String),
    StopBackgroundProcess(String),
    InspectSubagent(String),
}

/// Splits the timeline area beneath any sticky user box into `(dock, body)`.
///
/// Returns an empty `dock` rectangle whenever there are no background processes
/// or subagents so normal sessions incur zero visual overhead.
pub(crate) fn split_dock_region(state: &TuiState, available: Rect) -> (Rect, Rect) {
    let height = dock_height(state, available.height);
    if height == 0 || available.width == 0 {
        return (Rect::default(), available);
    }
    let gap = u16::from(available.height > height + 1);
    let dock = Rect {
        x: available.x,
        y: available.y,
        width: available.width,
        height,
    };
    let reserved = height.saturating_add(gap).min(available.height);
    let body = Rect {
        x: available.x,
        y: available.y.saturating_add(reserved),
        width: available.width,
        height: available.height.saturating_sub(reserved),
    };
    (dock, body)
}

/// Advances the timeline body scroll by the rows consumed by the inline dock
/// while keeping short conversations pinned at row 0 and followed conversations
/// flush with the bottom of `body`.
pub(crate) fn adjust_body_scroll(
    base_body_scroll: usize,
    available_height: u16,
    body_height: u16,
    total_rows: usize,
) -> usize {
    let reserved = usize::from(available_height.saturating_sub(body_height));
    if reserved == 0 {
        return base_body_scroll;
    }
    let max_scroll = total_rows.saturating_sub(usize::from(body_height));
    base_body_scroll.saturating_add(reserved).min(max_scroll)
}

pub(crate) fn dock_visible_rows(state: &TuiState, available_height: u16) -> usize {
    if !state.task_dock().is_expanded() || available_height <= 3 {
        return 0;
    }
    let entries = state.task_dock().entries(state.subagents());
    // Reserve room for the dock header row, bottom divider, and timeline body.
    let budget = usize::from(available_height.saturating_sub(4)).clamp(1, MAX_DOCK_VISIBLE_ROWS);
    entries.len().max(1).min(budget)
}

fn dock_height(state: &TuiState, available_height: u16) -> u16 {
    let counts = state.task_dock().counts(state.subagents());
    if counts.total == 0 || available_height < 2 {
        return 0;
    }
    if !state.task_dock().is_expanded() {
        return 1;
    }
    let rows = dock_visible_rows(state, available_height);
    // 1 header row + visible rows (capped at 5) + 1 subtle bottom divider.
    u16::try_from(rows.saturating_add(2))
        .unwrap_or(u16::MAX)
        .min(available_height.saturating_sub(1).max(1))
}

/// Paints the persistent 1-row collapsed strip or bounded 5-row expanded drawer.
pub(crate) fn render_task_dock(frame: &mut Frame<'_>, state: &TuiState, area: Rect) {
    if area.is_empty() {
        return;
    }
    let counts = state.task_dock().counts(state.subagents());
    if counts.total == 0 {
        return;
    }
    let clock = MotionClock::now();
    let header_area = Rect::new(area.x, area.y, area.width, 1);

    if !state.task_dock().is_expanded() {
        let line = collapsed_dock_line(state, counts, clock, area.width);
        frame.render_widget(Paragraph::new(line), header_area);
        return;
    }

    let entries = state.task_dock().entries(state.subagents());
    let list_rows = usize::from(area.height.saturating_sub(2)).min(MAX_DOCK_VISIBLE_ROWS);
    let scroll = state
        .task_dock()
        .scroll()
        .min(entries.len().saturating_sub(list_rows));
    let header_line = expanded_dock_header_line(
        state,
        counts,
        entries.len(),
        scroll,
        list_rows,
        clock,
        area.width,
    );
    frame.render_widget(Paragraph::new(header_line), header_area);

    let list_y = area.y.saturating_add(1);
    if entries.is_empty() && list_rows > 0 {
        let empty_line = Line::from(Span::styled(
            "  No matching tasks in this filter.".to_owned(),
            semantic_style(state, SemanticColor::Muted),
        ));
        frame.render_widget(
            Paragraph::new(empty_line),
            Rect::new(area.x, list_y, area.width, 1),
        );
    } else {
        for (offset, entry) in entries.iter().skip(scroll).take(list_rows).enumerate() {
            let row_y = list_y.saturating_add(u16::try_from(offset).unwrap_or(0));
            if row_y >= area.bottom().saturating_sub(1) {
                break;
            }
            let line = dock_entry_line(state, entry, clock, area.width);
            frame.render_widget(
                Paragraph::new(line),
                Rect::new(area.x, row_y, area.width, 1),
            );
        }
    }

    if area.height >= 2 {
        let divider_y = area.bottom().saturating_sub(1);
        let divider = Line::from(Span::styled(
            "─".repeat(usize::from(area.width)),
            semantic_style(state, SemanticColor::Border),
        ));
        frame.render_widget(
            Paragraph::new(divider),
            Rect::new(area.x, divider_y, area.width, 1),
        );
    }
}

/// Resolves a mouse click inside `area` into a [`TaskDockPointer`].
pub(crate) fn dock_mouse_down(
    state: &TuiState,
    area: Rect,
    position: Position,
) -> Option<TaskDockPointer> {
    if area.is_empty() || !area.contains(position) {
        return None;
    }
    let counts = state.task_dock().counts(state.subagents());
    if counts.total == 0 {
        return None;
    }
    if !state.task_dock().is_expanded() {
        return Some(TaskDockPointer::ToggleExpanded);
    }
    if position.y == area.y {
        let rel_x = usize::from(position.x.saturating_sub(area.x));
        for (filter, start, end) in expanded_filter_hit_ranges(counts) {
            if (start..end).contains(&rel_x) {
                return Some(TaskDockPointer::SelectFilter(filter));
            }
        }
        return Some(TaskDockPointer::ToggleExpanded);
    }
    // Bottom hairline row is decorative.
    if position.y == area.bottom().saturating_sub(1) {
        return None;
    }
    let row_index = usize::from(position.y.saturating_sub(area.y.saturating_add(1)));
    let entries = state.task_dock().entries(state.subagents());
    let list_rows = usize::from(area.height.saturating_sub(2)).min(MAX_DOCK_VISIBLE_ROWS);
    let scroll = state
        .task_dock()
        .scroll()
        .min(entries.len().saturating_sub(list_rows));
    let entry = entries.get(scroll.saturating_add(row_index))?;
    let rel_x = usize::from(position.x.saturating_sub(area.x));
    let width = usize::from(area.width);
    match entry {
        TaskDockEntry::Background(summary) => {
            let id = summary.id().as_str().to_owned();
            if matches!(summary.state(), BackgroundProcessState::Running { .. })
                && width >= 18
                && rel_x >= width.saturating_sub(6)
            {
                Some(TaskDockPointer::StopBackgroundProcess(id))
            } else {
                Some(TaskDockPointer::InspectBackgroundProcess(id))
            }
        }
        TaskDockEntry::Subagent { agent_id, .. } => {
            Some(TaskDockPointer::InspectSubagent(agent_id.clone()))
        }
    }
}

fn collapsed_dock_line(
    state: &TuiState,
    counts: crate::tui::task_dock::TaskDockCounts,
    clock: MotionClock,
    width: u16,
) -> Line<'static> {
    let max_width = usize::from(width);
    let chevron_style = semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD);
    let muted = semantic_style(state, SemanticColor::Muted);
    let secondary = semantic_style(state, SemanticColor::TextSecondary);
    let badge = format!("[Tasks {} ▸]", counts.total);
    let badge_w = UnicodeWidthStr::width(badge.as_str());

    let mut left_spans = if counts.running > 0 {
        let spinner = motion::spinner_glyph(Duration::from_millis(clock.millis()));
        let mut active_parts = Vec::new();
        if counts.background_running > 0 {
            active_parts.push(format!("{} bg", counts.background_running));
        }
        if counts.subagents_running > 0 {
            let noun = if counts.subagents_running == 1 {
                "agent"
            } else {
                "agents"
            };
            active_parts.push(format!("{} {noun}", counts.subagents_running));
        }
        let headline = if active_parts.len() > 1 {
            format!("{} running ({})", counts.running, active_parts.join(" · "))
        } else {
            format!("{} running", counts.running)
        };
        let mut spans = vec![
            Span::styled("▸ ".to_owned(), chevron_style),
            Span::styled(format!("{spinner} "), chevron_style),
            Span::styled(headline, chevron_style),
        ];

        let entries = state.task_dock().entries(state.subagents());
        let used_prefix: usize = spans.iter().map(Span::width).sum();
        let preview_budget = max_width
            .saturating_sub(used_prefix)
            .saturating_sub(badge_w + 5);
        if preview_budget >= 10
            && let Some(first) = entries.first()
        {
            let preview = compact_entry_preview(first, counts.total);
            spans.push(Span::styled(" · ".to_owned(), muted));
            spans.push(Span::styled(
                truncate_chars(&preview, preview_budget),
                secondary,
            ));
        }
        motion::flow_gradient_spans(
            spans,
            motion::RUNNING_FLOW_STOPS,
            motion::RUNNING_FLOW_WAVELENGTH,
            motion::RUNNING_FLOW_CHARS_PER_SEC,
            0,
            state.theme().color(SemanticColor::Muted),
            clock,
            state.animations_enabled(),
        )
    } else {
        let mut breakdown = Vec::new();
        if counts.background_total > 0 {
            breakdown.push(format!("{} bg", counts.background_total));
        }
        if counts.subagents_total > 0 {
            let noun = if counts.subagents_total == 1 {
                "agent"
            } else {
                "agents"
            };
            breakdown.push(format!("{} {noun}", counts.subagents_total));
        }
        let summary = format!(
            "All {} tasks done ({})",
            counts.total,
            breakdown.join(" · ")
        );
        vec![
            Span::styled("▸ ".to_owned(), chevron_style),
            Span::styled(
                "✓ ".to_owned(),
                semantic_style(state, SemanticColor::Success).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                truncate_chars(&summary, max_width.saturating_sub(badge_w + 6).max(8)),
                secondary,
            ),
        ]
    };

    let used: usize = left_spans.iter().map(Span::width).sum();
    if max_width >= used + badge_w + 2 {
        let gap = max_width.saturating_sub(used + badge_w);
        left_spans.push(Span::raw(" ".repeat(gap)));
        left_spans.push(Span::styled(badge, chevron_style));
    }
    Line::from(left_spans)
}

fn compact_entry_preview(first: &TaskDockEntry, total: usize) -> String {
    let base = match first {
        TaskDockEntry::Background(summary) => {
            let cmd = summary
                .command()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            format!("{}: {}", summary.id().as_str(), cmd)
        }
        TaskDockEntry::Subagent {
            agent_id,
            display_name,
            summary,
            phase,
            ..
        } => {
            let label = display_name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(agent_id.as_str());
            let trimmed = summary.trim();
            if trimmed.is_empty() {
                format!("{label}: {}", phase.label().to_ascii_lowercase())
            } else {
                format!("{label}: {trimmed}")
            }
        }
    };
    if total > 1 {
        format!("{base}  (+{})", total - 1)
    } else {
        base
    }
}

fn expanded_filter_labels(
    counts: crate::tui::task_dock::TaskDockCounts,
) -> Vec<(TaskDockFilter, String)> {
    let mut items = vec![
        (TaskDockFilter::All, format!("[All {}]", counts.total)),
        (
            TaskDockFilter::Running,
            format!("[Running {}]", counts.running),
        ),
    ];
    if counts.background_total > 0 {
        items.push((
            TaskDockFilter::Background,
            format!("[BG {}]", counts.background_total),
        ));
    }
    if counts.subagents_total > 0 {
        items.push((
            TaskDockFilter::Subagents,
            format!("[Agents {}]", counts.subagents_total),
        ));
    }
    items
}

fn expanded_filter_hit_ranges(
    counts: crate::tui::task_dock::TaskDockCounts,
) -> Vec<(TaskDockFilter, usize, usize)> {
    // Prefix: "▾ Tasks  " = 9 cols (width of "▾ " + "Tasks  ").
    let mut x = 9usize;
    let mut ranges = Vec::new();
    for (filter, label) in expanded_filter_labels(counts) {
        let w = UnicodeWidthStr::width(label.as_str());
        ranges.push((filter, x, x + w));
        x += w + 1;
    }
    ranges
}

fn expanded_dock_header_line(
    state: &TuiState,
    counts: crate::tui::task_dock::TaskDockCounts,
    filtered_total: usize,
    scroll: usize,
    list_rows: usize,
    _clock: MotionClock,
    width: u16,
) -> Line<'static> {
    let max_width = usize::from(width);
    let focus = semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD);
    let muted = semantic_style(state, SemanticColor::Muted);
    let active_filter = state.task_dock().filter();

    let mut spans = vec![
        Span::styled("▾ ".to_owned(), focus),
        Span::styled("Tasks  ".to_owned(), focus),
    ];
    for (filter, label) in expanded_filter_labels(counts) {
        let style = if filter == active_filter {
            focus
        } else {
            muted
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
    }

    let range_text = if filtered_total > list_rows && list_rows > 0 {
        let start = scroll + 1;
        let end = (scroll + list_rows).min(filtered_total);
        format!("{start}–{end}/{filtered_total}  [Collapse]")
    } else {
        "[Collapse]".to_owned()
    };
    let right_w = UnicodeWidthStr::width(range_text.as_str());
    let used: usize = spans.iter().map(Span::width).sum();
    if max_width > used + right_w {
        spans.push(Span::raw(
            " ".repeat(max_width.saturating_sub(used + right_w)),
        ));
        spans.push(Span::styled(range_text, muted));
    }
    Line::from(spans)
}

fn dock_entry_line(
    state: &TuiState,
    entry: &TaskDockEntry,
    clock: MotionClock,
    width: u16,
) -> Line<'static> {
    let max_width = usize::from(width);
    let muted = semantic_style(state, SemanticColor::Muted);
    let secondary = semantic_style(state, SemanticColor::TextSecondary);
    match entry {
        TaskDockEntry::Background(summary) => {
            let running = matches!(summary.state(), BackgroundProcessState::Running { .. });
            let glyph = if running {
                motion::spinner_glyph(Duration::from_millis(clock.millis()))
            } else if matches!(summary.state(), BackgroundProcessState::Exited { code: 0 }) {
                "✓"
            } else {
                "✗"
            };
            let status_color = if running {
                SemanticColor::Focus
            } else if matches!(summary.state(), BackgroundProcessState::Exited { code: 0 }) {
                SemanticColor::Success
            } else {
                SemanticColor::Warning
            };
            let id_text = format!("{:<8}", summary.id().as_str());
            let state_text = format!("{:<18}", summary.state().label());
            let action_text = if running { "[View] [Stop]" } else { "[View]" };
            let action_w = UnicodeWidthStr::width(action_text);

            let mut spans = vec![
                Span::raw("  "),
                Span::styled(
                    format!("{glyph} "),
                    semantic_style(state, status_color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    id_text,
                    semantic_style(state, SemanticColor::ToolKeyword).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(state_text, semantic_style(state, status_color)),
            ];
            let used: usize = spans.iter().map(Span::width).sum();
            let cmd_budget = max_width.saturating_sub(used).saturating_sub(action_w + 3);
            if cmd_budget >= 6 {
                let cmd = summary
                    .command()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                spans.push(Span::styled(truncate_chars(&cmd, cmd_budget), secondary));
            }
            if running {
                spans = motion::flow_gradient_spans(
                    spans,
                    motion::RUNNING_FLOW_STOPS,
                    motion::RUNNING_FLOW_WAVELENGTH,
                    motion::RUNNING_FLOW_CHARS_PER_SEC,
                    0,
                    state.theme().color(SemanticColor::Muted),
                    clock,
                    state.animations_enabled(),
                );
            }
            let used_after: usize = spans.iter().map(Span::width).sum();
            if max_width > used_after + action_w {
                spans.push(Span::raw(
                    " ".repeat(max_width.saturating_sub(used_after + action_w)),
                ));
                if running {
                    spans.push(Span::styled(
                        "[View] ".to_owned(),
                        semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
                    ));
                    spans.push(Span::styled(
                        "[Stop]".to_owned(),
                        semantic_style(state, SemanticColor::Error).add_modifier(Modifier::BOLD),
                    ));
                } else {
                    spans.push(Span::styled("[View]".to_owned(), muted));
                }
            }
            Line::from(spans)
        }
        TaskDockEntry::Subagent {
            agent_id,
            display_name,
            phase,
            summary,
            ..
        } => {
            let glyph = phase.glyph(clock);
            let color = phase.color();
            let action_text = "[Inspect ▸]";
            let action_w = UnicodeWidthStr::width(action_text);
            let raw_label = display_name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(agent_id.as_str());
            let truncated_label = truncate_columns(raw_label, 24);
            let label_w = UnicodeWidthStr::width(truncated_label.as_str());
            let label_pad = " ".repeat(10usize.saturating_sub(label_w));
            let mut spans = vec![
                Span::raw("  "),
                Span::styled(
                    format!("{glyph} "),
                    semantic_style(state, color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{truncated_label}{label_pad}"),
                    semantic_style(state, SemanticColor::Heading).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{:<10}", phase.label().to_ascii_lowercase()),
                    semantic_style(state, color),
                ),
            ];
            let used: usize = spans.iter().map(Span::width).sum();
            let summary_budget = max_width.saturating_sub(used).saturating_sub(action_w + 3);
            if summary_budget >= 6 && !summary.trim().is_empty() {
                spans.push(Span::styled(
                    truncate_chars(summary.trim(), summary_budget),
                    secondary,
                ));
            }
            if phase.is_active() {
                spans = motion::flow_gradient_spans(
                    spans,
                    motion::RUNNING_FLOW_STOPS,
                    motion::RUNNING_FLOW_WAVELENGTH,
                    motion::RUNNING_FLOW_CHARS_PER_SEC,
                    0,
                    state.theme().color(SemanticColor::Muted),
                    clock,
                    state.animations_enabled(),
                );
            }
            let used_after: usize = spans.iter().map(Span::width).sum();
            if max_width > used_after + action_w {
                spans.push(Span::raw(
                    " ".repeat(max_width.saturating_sub(used_after + action_w)),
                ));
                spans.push(Span::styled(
                    action_text.to_owned(),
                    semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
                ));
            }
            Line::from(spans)
        }
    }
}

const BACKGROUND_MODAL_MAX_WIDTH: u16 = 104;
const BACKGROUND_MODAL_HEIGHT: u16 = 28;

/// Renders the interactive Background Process Output inspector overlay.
pub(crate) fn render_background_task_overlay(
    frame: &mut Frame<'_>,
    state: &TuiState,
    overlay: &BackgroundTaskOverlay,
) {
    let title = format!("Background task {}", overlay.process_id());
    let modal = modal_rects(
        frame.area(),
        BACKGROUND_MODAL_MAX_WIDTH,
        BACKGROUND_MODAL_HEIGHT,
    );
    render_modal_frame(
        frame,
        state,
        modal,
        &title,
        SemanticColor::BorderFocus,
        true,
    );
    let body = modal.inner;
    if body.is_empty() {
        return;
    }

    let bg_style = modal_surface_style(state);
    frame.render_widget(Block::default().style(bg_style), body);

    let (meta_lines, output_lines) = build_background_overlay_lines(state, overlay, body.width);
    let meta_height = u16::try_from(meta_lines.len())
        .unwrap_or(0)
        .min(body.height);
    if meta_height > 0 {
        frame.render_widget(
            Paragraph::new(meta_lines).style(bg_style),
            Rect::new(body.x, body.y, body.width, meta_height),
        );
    }

    let footer_height = u16::from(body.height > meta_height + 2);
    let output_area = Rect {
        x: body.x,
        y: body.y.saturating_add(meta_height),
        width: body.width,
        height: body.height.saturating_sub(meta_height + footer_height),
    };
    if !output_area.is_empty() {
        let total_rows = Paragraph::new(output_lines.clone())
            .wrap(Wrap { trim: false })
            .line_count(output_area.width);
        let max_scroll = total_rows.saturating_sub(usize::from(output_area.height));
        let scroll = if overlay.follow_tail() {
            max_scroll
        } else {
            overlay.scroll().min(max_scroll)
        };
        let scroll_u16 = u16::try_from(scroll).unwrap_or(u16::MAX);
        frame.render_widget(
            Paragraph::new(output_lines)
                .wrap(Wrap { trim: false })
                .scroll((scroll_u16, 0))
                .style(bg_style),
            output_area,
        );
    }
    if footer_height > 0 {
        let hint = if overlay.is_running() {
            "↑↓ Scroll   R Refresh   X Stop   Esc Close"
        } else {
            "↑↓ Scroll   R Refresh   Esc Close"
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                hint,
                semantic_style(state, SemanticColor::Muted).patch(bg_style),
            ))),
            Rect::new(body.x, body.bottom().saturating_sub(1), body.width, 1),
        );
    }
}

/// Resolves pointer hits inside the Background Process Output inspector overlay.
pub(crate) fn background_task_overlay_pointer(
    state: &TuiState,
    overlay: &BackgroundTaskOverlay,
    area: Rect,
    position: Position,
) -> Option<BackgroundTaskPointer> {
    let modal = modal_rects(area, BACKGROUND_MODAL_MAX_WIDTH, BACKGROUND_MODAL_HEIGHT);
    if modal.close_hit(position) {
        return Some(BackgroundTaskPointer::Close);
    }
    let body = modal.inner;
    if !body.contains(position) {
        return None;
    }
    // Action bar sits on row 2 of body (after status + command rows).
    if position.y == body.y.saturating_add(2) {
        let rel_x = usize::from(position.x.saturating_sub(body.x));
        if rel_x < 10 {
            return Some(BackgroundTaskPointer::Refresh);
        }
        if overlay.is_running() && (12..26).contains(&rel_x) {
            return Some(BackgroundTaskPointer::Stop);
        }
        let close_start = if overlay.is_running() { 28 } else { 12 };
        if (close_start..close_start + 8).contains(&rel_x) {
            return Some(BackgroundTaskPointer::Close);
        }
    }
    let _ = state;
    None
}

fn build_background_overlay_lines(
    state: &TuiState,
    overlay: &BackgroundTaskOverlay,
    width: u16,
) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
    let muted = semantic_style(state, SemanticColor::Muted);
    let focus = semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD);
    let error = semantic_style(state, SemanticColor::Error).add_modifier(Modifier::BOLD);
    let assistant = semantic_style(state, SemanticColor::Assistant);
    let secondary = semantic_style(state, SemanticColor::TextSecondary);
    let max_w = usize::from(width).max(8);

    let mut meta = Vec::new();
    if let Some(output) = overlay.output() {
        let summary = output.summary();
        let state_color = match summary.state() {
            BackgroundProcessState::Running { .. } => SemanticColor::Focus,
            BackgroundProcessState::Exited { code: 0 } => SemanticColor::Success,
            _ => SemanticColor::Warning,
        };
        meta.push(Line::from(vec![
            Span::styled("Status: ".to_owned(), muted),
            Span::styled(
                summary.state().label(),
                semantic_style(state, state_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("   cwd: {}", summary.cwd().unwrap_or(".")), muted),
        ]));
        let cmd = summary
            .command()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        meta.push(Line::from(vec![
            Span::styled("Command: ".to_owned(), muted),
            Span::styled(truncate_chars(&cmd, max_w.saturating_sub(9)), assistant),
        ]));
    } else if let Some(err) = overlay.error() {
        meta.push(Line::from(Span::styled(err.to_owned(), error)));
        meta.push(Line::from(""));
    } else {
        meta.push(Line::from(Span::styled(
            "Loading output…".to_owned(),
            muted,
        )));
        meta.push(Line::from(""));
    }

    let mut actions = vec![Span::styled("[Refresh]".to_owned(), focus)];
    if overlay.is_running() {
        actions.push(Span::raw("   "));
        actions.push(Span::styled("[Stop process]".to_owned(), error));
    }
    actions.push(Span::raw("  "));
    actions.push(Span::styled("[Close]".to_owned(), muted));
    meta.push(Line::from(actions));
    meta.push(Line::from(Span::styled(
        "─".repeat(max_w),
        semantic_style(state, SemanticColor::Border),
    )));

    let mut body_lines = Vec::new();
    if let Some(output) = overlay.output() {
        let stdout = output.stdout();
        let stderr = output.stderr();
        if !stdout.is_empty() {
            for line in stdout.lines() {
                body_lines.push(Line::from(Span::styled(
                    crate::text::without_control_chars(line),
                    assistant,
                )));
            }
        }
        if !stderr.is_empty() {
            if !body_lines.is_empty() {
                body_lines.push(Line::from(""));
                body_lines.push(Line::from(Span::styled("stderr:".to_owned(), error)));
            }
            for line in stderr.lines() {
                body_lines.push(Line::from(Span::styled(
                    crate::text::without_control_chars(line),
                    secondary,
                )));
            }
        }
        if body_lines.is_empty() {
            body_lines.push(Line::from(Span::styled(
                "(no output captured yet)".to_owned(),
                muted,
            )));
        }
        if output.stdout_truncated() || output.stderr_truncated() {
            body_lines.push(Line::from(Span::styled(
                "(capture reached byte limit)".to_owned(),
                muted,
            )));
        }
    }
    (meta, body_lines)
}
