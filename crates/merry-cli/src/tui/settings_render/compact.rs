//! Compact/mobile 3-level drill-down renderers for the Settings Workbench.

use crate::tui::{
    click_target::truncate_columns,
    modal_frame::{modal_selection_style, modal_surface_style},
    overlay_render::semantic_style,
    settings_overlay::{SettingsCategory, SettingsChoiceMenu, SettingsOverlay},
    state::TuiState,
    theme::SemanticColor,
};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

pub(super) fn split_compact_back_header(area: Rect) -> (Rect, Rect) {
    if area.height <= 2 {
        return (Rect::default(), area);
    }
    let header_height = if area.height >= 6 { 2 } else { 1 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(header_height), Constraint::Min(1)])
        .split(area);
    (chunks[0], chunks[1])
}

pub(super) fn render_compact_back_bar(
    frame: &mut Frame<'_>,
    state: &TuiState,
    parent: &str,
    current: &str,
    area: Rect,
) {
    if area.is_empty() {
        return;
    }
    let surface = modal_surface_style(state);
    let max_width = usize::from(area.width);
    let back_prefix = format!("◂ {parent} / ");
    let remaining = max_width
        .saturating_sub(UnicodeWidthStr::width(back_prefix.as_str()))
        .max(1);
    let mut lines = vec![Line::from(vec![
        Span::styled(
            back_prefix,
            semantic_style(state, SemanticColor::Focus)
                .add_modifier(Modifier::BOLD)
                .patch(surface),
        ),
        Span::styled(
            truncate_columns(current, remaining),
            semantic_style(state, SemanticColor::Heading)
                .add_modifier(Modifier::BOLD)
                .patch(surface),
        ),
    ])];
    if area.height >= 2 {
        lines.push(Line::styled("", surface));
    }
    frame.render_widget(Paragraph::new(lines).style(surface), area);
}

pub(super) fn render_compact_categories(
    frame: &mut Frame<'_>,
    state: &TuiState,
    settings: &SettingsOverlay,
    area: Rect,
) {
    if area.is_empty() {
        return;
    }
    let surface = modal_surface_style(state);
    let width = usize::from(area.width);
    let show_summary = width >= 44;
    let selected_idx = SettingsCategory::ALL
        .iter()
        .position(|&category| category == settings.category())
        .unwrap_or(0);
    let visible_rows = usize::from(area.height).max(1);
    let scroll_offset = selected_idx
        .saturating_sub(visible_rows.saturating_sub(1))
        .min(SettingsCategory::ALL.len().saturating_sub(visible_rows));

    let mut lines = Vec::with_capacity(visible_rows);
    for category in SettingsCategory::ALL
        .into_iter()
        .skip(scroll_offset)
        .take(visible_rows)
    {
        let active = settings.category() == category;
        let base = modal_selection_style(state, active);
        let bar = if active { "▌ " } else { "  " };
        let bar_style = if active {
            semantic_style(state, SemanticColor::Focus)
                .add_modifier(Modifier::BOLD)
                .patch(base)
        } else {
            base
        };
        let text_style = if active {
            semantic_style(state, SemanticColor::Assistant)
                .add_modifier(Modifier::BOLD)
                .patch(base)
        } else {
            semantic_style(state, SemanticColor::TextSecondary).patch(base)
        };
        let label = category.label();
        let arrow = " ▸";
        let mut spans = vec![Span::styled(bar, bar_style)];
        if show_summary {
            let label_col = format!("{:<13}", truncate_columns(label, 12));
            let summary_budget = width.saturating_sub(2 + 13 + 3).max(1);
            let summary = truncate_columns(category.summary(), summary_budget);
            let used = 2 + 13 + UnicodeWidthStr::width(summary.as_str()) + 2;
            let pad = width.saturating_sub(used);
            spans.push(Span::styled(label_col, text_style));
            spans.push(Span::styled(
                summary,
                semantic_style(state, SemanticColor::Muted).patch(base),
            ));
            spans.push(Span::styled(" ".repeat(pad), base));
            spans.push(Span::styled(
                arrow,
                semantic_style(state, SemanticColor::Focus).patch(base),
            ));
        } else {
            let label_budget = width.saturating_sub(5).max(1);
            let label_text = truncate_columns(label, label_budget);
            let used = 2 + UnicodeWidthStr::width(label_text.as_str()) + 2;
            let pad = width.saturating_sub(used);
            spans.push(Span::styled(label_text, text_style));
            spans.push(Span::styled(" ".repeat(pad), base));
            spans.push(Span::styled(
                arrow,
                semantic_style(state, SemanticColor::Focus).patch(base),
            ));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines).style(surface), area);
}

pub(super) fn render_compact_choice_menu(
    frame: &mut Frame<'_>,
    state: &TuiState,
    menu: &SettingsChoiceMenu,
    area: Rect,
) {
    if area.is_empty() {
        return;
    }
    let (back_bar, body) = split_compact_back_header(area);
    render_compact_back_bar(
        frame,
        state,
        menu.item().category().label(),
        menu.item().label(),
        back_bar,
    );
    if body.is_empty() {
        return;
    }
    let surface = modal_surface_style(state);
    let choices = menu.choices();
    let list_height = usize::from(body.height).max(1);
    let selected = menu.selected().min(choices.len().saturating_sub(1));
    let scroll_offset = selected
        .saturating_sub(list_height.saturating_sub(1))
        .min(choices.len().saturating_sub(list_height));
    let width = usize::from(body.width);
    let label_budget = width.saturating_sub(6).max(1);

    let mut lines = Vec::with_capacity(list_height);
    for (visible_idx, choice) in choices
        .iter()
        .skip(scroll_offset)
        .take(list_height)
        .enumerate()
    {
        let is_selected = scroll_offset + visible_idx == selected;
        let base = if is_selected {
            modal_selection_style(state, true)
        } else {
            surface
        };
        let marker = if is_selected { "▌ " } else { "  " };
        let check = if choice.current { "✓ " } else { "  " };
        let label = truncate_columns(&choice.label, label_budget);
        let used = 4 + UnicodeWidthStr::width(label.as_str());
        let pad = width.saturating_sub(used);
        lines.push(Line::from(vec![
            Span::styled(
                marker,
                semantic_style(state, SemanticColor::Focus)
                    .add_modifier(Modifier::BOLD)
                    .patch(base),
            ),
            Span::styled(
                check,
                semantic_style(
                    state,
                    if choice.current {
                        SemanticColor::ToolKeyword
                    } else {
                        SemanticColor::Muted
                    },
                )
                .patch(base),
            ),
            Span::styled(
                label,
                semantic_style(
                    state,
                    if is_selected {
                        SemanticColor::Assistant
                    } else {
                        SemanticColor::TextSecondary
                    },
                )
                .add_modifier(if is_selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                })
                .patch(base),
            ),
            Span::styled(" ".repeat(pad), base),
        ]));
    }
    frame.render_widget(Paragraph::new(lines).style(surface), body);
}
