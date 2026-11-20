//! Widget rendering for the command details modal, history rail, and split stream panes.

use super::{
    CommandDetails, DetailsOutput, HistoryPlace, StreamPane, finished_command_indices,
    layout::{
        FooterChipAction, OutputPanes, PaneRects, build_layout, footer_actions,
        history_scroll_offset, regions, split_detail_area,
    },
    shown_pane,
};
use crate::tui::{
    click_target::truncate_columns,
    keymap::KeyAction,
    overlay_render::code_surface_style,
    state::{CommandFailure, CommandView, TimelineItem, TuiState},
    text_wrap::semantic_style,
    theme::SemanticColor,
};
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};
use std::borrow::Cow;
use unicode_width::UnicodeWidthStr;

pub(crate) fn render_details(frame: &mut Frame<'_>, state: &TuiState, details: &CommandDetails) {
    let layout_regions = regions(frame.area(), state, details);
    if layout_regions.detail_area.is_empty() {
        return;
    }

    let code_theme = state.code_theme();
    let layout = details
        .layout
        .as_ref()
        .filter(|layout| {
            layout.width == layout_regions.detail_area.width && layout.code_theme == code_theme
        })
        .map_or_else(
            || {
                Cow::Owned(build_layout(
                    state,
                    details,
                    layout_regions.detail_area.width,
                ))
            },
            Cow::Borrowed,
        );

    let split = split_detail_area(
        layout_regions.detail_area,
        layout.header_lines.len(),
        &details.output,
    );

    let place = HistoryPlace::resolve(state, details.timeline_index);
    let active = shown_pane(&details.output, details.active_pane);
    let (title_scroll, title_max) = match split.output {
        OutputPanes::Single { body, .. } => {
            let max = layout
                .stdout_lines
                .len()
                .saturating_sub(usize::from(body.height));
            (details.stdout_scroll.min(max), max)
        }
        OutputPanes::Process { stdout, stderr, .. } => match active {
            StreamPane::Stdout => {
                let max = layout
                    .stdout_lines
                    .len()
                    .saturating_sub(usize::from(stdout.body.height));
                (details.stdout_scroll.min(max), max)
            }
            StreamPane::Stderr => {
                let max = layout
                    .stderr_lines
                    .len()
                    .saturating_sub(usize::from(stderr.body.height));
                (details.stderr_scroll.min(max), max)
            }
        },
    };

    // The dialog is inset by at least one column. Timeline fold markers are
    // painted in that gutter, so they would sit outside the border next to
    // every history row.
    cover_dialog_margins(frame, layout_regions.region);
    frame.render_widget(Clear, layout_regions.region);

    let border_slot = border_color_for_view(&details.view);
    let mut block = Block::default()
        .title(Line::from(vec![
            Span::styled(
                " 🎀 ",
                semantic_style(state, SemanticColor::Status).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" Command {} ", place.label()),
                semantic_style(state, border_slot).add_modifier(Modifier::BOLD),
            ),
        ]))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(semantic_style(state, border_slot).add_modifier(Modifier::BOLD))
        .style(code_surface_style(state));

    if let Some(right_title) = title_badges_line(state, &details.view, title_scroll, title_max) {
        block = block.title(right_title.alignment(Alignment::Right));
    }
    frame.render_widget(block, layout_regions.region);

    if let Some(rail) = layout_regions.history_rail {
        render_history_rail(frame, state, place, rail);
    }
    if let Some(divider) = layout_regions.rail_divider {
        render_vertical_divider(frame, state, divider);
    }

    if !split.header.is_empty() {
        let header_visible = layout
            .header_lines
            .iter()
            .take(usize::from(split.header.height))
            .cloned()
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(header_visible), split.header);
    }

    match split.output {
        OutputPanes::Single { divider, body } => {
            if let Some(div) = divider {
                frame.render_widget(
                    Paragraph::new(section_rule(
                        state,
                        None,
                        None,
                        SemanticColor::Muted,
                        div.width,
                    )),
                    div,
                );
            }
            render_scrollable_lines(frame, &layout.stdout_lines, details.stdout_scroll, body);
        }
        OutputPanes::Process {
            stdout,
            stderr,
            stdout_has_body,
            stderr_has_body,
        } => {
            let split_active = stdout_has_body && stderr_has_body;
            render_stream_pane(
                frame,
                state,
                "stdout",
                SemanticColor::ToolKeyword,
                stdout,
                stdout_has_body,
                split_active && details.active_pane == StreamPane::Stdout,
                &layout.stdout_lines,
                details.stdout_scroll,
            );
            render_stream_pane(
                frame,
                state,
                "stderr",
                SemanticColor::Error,
                stderr,
                stderr_has_body,
                split_active && details.active_pane == StreamPane::Stderr,
                &layout.stderr_lines,
                details.stderr_scroll,
            );
        }
    }

    if let Some(divider) = layout_regions.footer_divider {
        let rule = "─".repeat(usize::from(divider.width));
        frame.render_widget(
            Paragraph::new(Line::styled(
                rule,
                semantic_style(state, SemanticColor::Muted).add_modifier(Modifier::DIM),
            )),
            divider,
        );
    }
    if let Some(chips_rect) = layout_regions.chips_row {
        render_footer_chips(frame, state, details, place, chips_rect);
    }
    frame.render_widget(layout_regions.hints, layout_regions.hints_rect);
}

#[allow(clippy::too_many_arguments)]
fn render_stream_pane(
    frame: &mut Frame<'_>,
    state: &TuiState,
    label: &str,
    accent: SemanticColor,
    pane: PaneRects,
    has_body: bool,
    focused: bool,
    lines: &[Line<'static>],
    scroll: usize,
) {
    if pane.full.is_empty() {
        return;
    }
    if !has_body {
        let rule = section_rule(
            state,
            Some(label),
            Some("(empty)"),
            SemanticColor::Muted,
            pane.header.width,
        );
        frame.render_widget(Paragraph::new(rule), pane.header);
        return;
    }

    let max_scroll = lines.len().saturating_sub(usize::from(pane.body.height));
    let clamped = scroll.min(max_scroll);
    let count_label = if lines.len() == 1 {
        "1 line".to_owned()
    } else {
        format!("{} lines", lines.len())
    };
    let meta = if let Some(pct) = clamped.saturating_mul(100).checked_div(max_scroll) {
        let pos = if clamped == 0 {
            "Top".to_owned()
        } else if clamped >= max_scroll {
            "End".to_owned()
        } else {
            format!("{pct}%")
        };
        format!("{count_label} · {pos}")
    } else {
        count_label
    };
    let title_label = if focused {
        format!("▸ {label}")
    } else {
        label.to_owned()
    };
    let header_line = section_rule(
        state,
        Some(&title_label),
        Some(&meta),
        accent,
        pane.header.width,
    );
    frame.render_widget(Paragraph::new(header_line), pane.header);
    render_scrollable_lines(frame, lines, clamped, pane.body);
}

fn render_scrollable_lines(
    frame: &mut Frame<'_>,
    lines: &[Line<'static>],
    scroll: usize,
    body: Rect,
) {
    if body.is_empty() {
        return;
    }
    let max_scroll = lines.len().saturating_sub(usize::from(body.height));
    let visible = lines
        .iter()
        .skip(scroll.min(max_scroll))
        .take(usize::from(body.height))
        .cloned()
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(visible), body);
}

fn border_color_for_view(view: &CommandView) -> SemanticColor {
    match view {
        CommandView::Finished {
            exit_code, failure, ..
        } => {
            if *failure == Some(CommandFailure::Failed) || exit_code.is_some_and(|code| code != 0) {
                SemanticColor::Error
            } else if *failure == Some(CommandFailure::Cancelled) {
                SemanticColor::Warning
            } else {
                SemanticColor::Focus
            }
        }
        CommandView::Running { .. } => SemanticColor::Focus,
    }
}

fn title_badges_line(
    state: &TuiState,
    view: &CommandView,
    scroll: usize,
    max_scroll: usize,
) -> Option<Line<'static>> {
    let CommandView::Finished {
        exit_code,
        failure,
        elapsed,
        ..
    } = view
    else {
        return None;
    };

    let mut spans = vec![Span::raw(" ")];
    let (badge_label, badge_color) = match failure {
        Some(CommandFailure::Cancelled) => ("CANCELLED".to_owned(), SemanticColor::Warning),
        Some(CommandFailure::Failed) => (
            exit_code.map_or_else(|| "FAILED".to_owned(), |code| format!("EXIT {code}")),
            SemanticColor::Error,
        ),
        None => match exit_code {
            Some(0) => ("EXIT 0".to_owned(), SemanticColor::Success),
            Some(code) => (format!("EXIT {code}"), SemanticColor::Error),
            None => ("DONE".to_owned(), SemanticColor::Muted),
        },
    };
    spans.push(Span::styled(
        format!("[{badge_label}]"),
        semantic_style(state, badge_color).add_modifier(Modifier::BOLD),
    ));

    if let Some(elapsed) = elapsed {
        spans.push(Span::styled(
            format!("  {:.1}s", elapsed.as_secs_f64()),
            semantic_style(state, SemanticColor::Muted),
        ));
    }

    if let Some(pct) = scroll.saturating_mul(100).checked_div(max_scroll) {
        let progress = if scroll == 0 {
            "Top".to_owned()
        } else if scroll >= max_scroll {
            "End".to_owned()
        } else {
            format!("{pct}%")
        };
        spans.push(Span::styled(
            format!("  {progress}"),
            semantic_style(state, SemanticColor::Muted),
        ));
    }

    spans.push(Span::raw(" "));
    Some(Line::from(spans))
}

fn cover_dialog_margins(frame: &mut Frame<'_>, dialog: Rect) {
    let area = frame.area();
    if dialog.x > area.x {
        frame.render_widget(
            Clear,
            Rect::new(area.x, dialog.y, dialog.x - area.x, dialog.height),
        );
    }
    let right = dialog.right();
    if right < area.right() {
        frame.render_widget(
            Clear,
            Rect::new(right, dialog.y, area.right() - right, dialog.height),
        );
    }
}

fn render_history_rail(frame: &mut Frame<'_>, state: &TuiState, place: HistoryPlace, rail: Rect) {
    if rail.height == 0 || rail.width == 0 {
        return;
    }
    let indices = finished_command_indices(state);
    let mut lines = Vec::with_capacity(usize::from(rail.height));
    lines.push(Line::from(vec![
        Span::styled(
            "HISTORY",
            semantic_style(state, SemanticColor::ToolKeyword).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" ({})", place.label()),
            semantic_style(state, SemanticColor::Muted),
        ),
    ]));

    let visible_rows = usize::from(rail.height.saturating_sub(1));
    if visible_rows > 0 && !indices.is_empty() {
        let scroll = place.rail_index().map_or(0, |current| {
            history_scroll_offset(current, indices.len(), visible_rows)
        });
        let row_width = usize::from(rail.width);

        for (offset, timeline_index) in indices.iter().skip(scroll).take(visible_rows).enumerate() {
            let pos = scroll + offset;
            let selected = place.rail_index() == Some(pos);
            let Some(TimelineItem::Command { view }) = state.timeline().get(*timeline_index) else {
                continue;
            };
            lines.push(history_row_line(state, pos + 1, view, selected, row_width));
        }
    }

    frame.render_widget(Paragraph::new(lines), rail);
}

fn history_row_line(
    state: &TuiState,
    ordinal: usize,
    view: &CommandView,
    selected: bool,
    row_width: usize,
) -> Line<'static> {
    let (label, icon, icon_color) = match view {
        CommandView::Finished {
            detail,
            command,
            exit_code,
            failure,
            ..
        } => {
            let raw = if command.is_empty() {
                detail.as_str()
            } else {
                command.as_str()
            };
            let compact = raw
                .lines()
                .next()
                .unwrap_or("")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let (icon, color) = match failure {
                Some(CommandFailure::Cancelled) => ("⊘", SemanticColor::Warning),
                Some(CommandFailure::Failed) => ("✗", SemanticColor::Error),
                None if exit_code.is_some_and(|code| code != 0) => ("✗", SemanticColor::Error),
                _ => ("✓", SemanticColor::Success),
            };
            (compact, icon, color)
        }
        CommandView::Running { detail, .. } => (detail.clone(), "…", SemanticColor::Muted),
    };

    let marker = if selected { "▸" } else { " " };
    let num = format!("{ordinal} ");
    let prefix_width = 2 + num.len() + 2;
    let cmd_max = row_width.saturating_sub(prefix_width);
    let mut truncated = truncate_columns(&label, cmd_max);
    if selected {
        let pad = cmd_max.saturating_sub(UnicodeWidthStr::width(truncated.as_str()));
        if pad > 0 {
            truncated.push_str(&" ".repeat(pad));
        }
    }

    let selected_bg = state.theme().bg_color(SemanticColor::SelectionBackground);
    let apply_bg = |style: Style| match (selected, selected_bg) {
        (true, Some(bg)) => style.bg(bg),
        _ => style,
    };

    let marker_style = apply_bg(if selected {
        semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD)
    } else {
        semantic_style(state, SemanticColor::Muted)
    });
    let num_style = apply_bg(semantic_style(state, SemanticColor::Muted));
    let icon_style = apply_bg(semantic_style(state, icon_color).add_modifier(Modifier::BOLD));
    let text_style = apply_bg(if selected {
        semantic_style(state, SemanticColor::Assistant).add_modifier(Modifier::BOLD)
    } else {
        semantic_style(state, SemanticColor::Muted)
    });

    Line::from(vec![
        Span::styled(format!("{marker} "), marker_style),
        Span::styled(num, num_style),
        Span::styled(format!("{icon} "), icon_style),
        Span::styled(truncated, text_style),
    ])
}

fn render_vertical_divider(frame: &mut Frame<'_>, state: &TuiState, divider: Rect) {
    let style = semantic_style(state, SemanticColor::Muted).add_modifier(Modifier::DIM);
    let lines = (0..divider.height)
        .map(|_| Line::styled("│", style))
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), divider);
}

fn render_footer_chips(
    frame: &mut Frame<'_>,
    state: &TuiState,
    details: &CommandDetails,
    place: HistoryPlace,
    chips_rect: Rect,
) {
    let actions = footer_actions(place);
    let mut spans = Vec::new();
    for (index, action) in actions.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
        }
        let style = match action {
            FooterChipAction::Previous => {
                if place.has_previous() {
                    semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD)
                } else {
                    semantic_style(state, SemanticColor::Muted).add_modifier(Modifier::DIM)
                }
            }
            FooterChipAction::Next => {
                if place.has_next() {
                    semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD)
                } else {
                    semantic_style(state, SemanticColor::Muted).add_modifier(Modifier::DIM)
                }
            }
            FooterChipAction::CopyCommand => {
                semantic_style(state, SemanticColor::ToolKeyword).add_modifier(Modifier::BOLD)
            }
            FooterChipAction::CopyOutput => {
                if matches!(details.output, DetailsOutput::Ready(_)) {
                    semantic_style(state, SemanticColor::ToolKeyword).add_modifier(Modifier::BOLD)
                } else {
                    semantic_style(state, SemanticColor::Muted).add_modifier(Modifier::DIM)
                }
            }
            FooterChipAction::Close => {
                semantic_style(state, SemanticColor::Muted).add_modifier(Modifier::BOLD)
            }
        };
        spans.push(Span::styled(format!("[{}]", action.label()), style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), chips_rect);
}

pub(super) fn footer_hints(
    state: &TuiState,
    details: &CommandDetails,
    width: u16,
) -> Paragraph<'static> {
    let close = state
        .keymap()
        .binding_label_for(KeyAction::OpenCommandDetails)
        .map_or_else(
            || "Esc close".to_owned(),
            |key| format!("{key} / Esc close"),
        );

    let muted = semantic_style(state, SemanticColor::Muted);
    let mut hints = if width < 48 {
        vec![
            Line::styled("A/D ←/→ cmd · ↑/↓ scroll", muted),
            Line::styled(format!("{close} · C/Y copy"), muted),
        ]
    } else if width < 82 {
        vec![
            Line::styled("A/D ←/→ switch · E pane · W/S ↑/↓/wheel · C/Y copy", muted),
            Line::styled(format!("{close} · Q quit"), muted),
        ]
    } else {
        vec![Line::styled(
            format!(
                "A/D ←/→ switch · E pane · W/S ↑/↓ or wheel scroll · C copy command · Y copy output · {close}"
            ),
            muted,
        )]
    };

    if let Some(feedback) = &details.feedback
        && !feedback.is_empty()
    {
        hints.push(Line::styled(
            feedback.clone(),
            semantic_style(state, SemanticColor::Warning).add_modifier(Modifier::BOLD),
        ));
    }
    Paragraph::new(hints).wrap(Wrap { trim: false })
}

fn section_rule(
    state: &TuiState,
    label: Option<&str>,
    meta: Option<&str>,
    label_color: SemanticColor,
    width: u16,
) -> Line<'static> {
    let dim_rule = semantic_style(state, SemanticColor::Muted).add_modifier(Modifier::DIM);
    let total_width = usize::from(width);
    let Some(label) = label else {
        return Line::styled("─".repeat(total_width), dim_rule);
    };

    let mut spans = vec![
        Span::styled("── ", dim_rule),
        Span::styled(
            label.to_owned(),
            semantic_style(state, label_color).add_modifier(Modifier::BOLD),
        ),
    ];
    let mut used = 3 + UnicodeWidthStr::width(label);
    if let Some(meta) = meta {
        let meta_text = format!(" · {meta}");
        used += UnicodeWidthStr::width(meta_text.as_str());
        spans.push(Span::styled(
            meta_text,
            semantic_style(state, SemanticColor::Muted),
        ));
    }
    if used + 2 <= total_width {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            "─".repeat(total_width.saturating_sub(used + 1)),
            dim_rule,
        ));
    }
    Line::from(spans)
}
