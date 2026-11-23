use super::{
    click_target::{OverlayPointer, chip_hit, chip_line},
    modal_frame::{
        ModalRects, modal_rects, modal_selection_style, modal_surface_style, render_divider,
        render_modal_frame,
    },
    overlay::{CommandPalette, CommandSpec, MessageDialogKind, MessageDialogOverlay, Overlay},
    state::TuiState,
    theme::SemanticColor,
};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

const MAX_OVERLAY_WIDTH: u16 = 80;

enum CommandPaletteRow<'a> {
    Category(&'a str),
    Command { index: usize, spec: &'a CommandSpec },
}

pub(crate) fn render_overlay(frame: &mut Frame<'_>, state: &TuiState) {
    let Some(overlay) = state.overlay() else {
        return;
    };

    match overlay {
        Overlay::CommandDetails(details) => {
            super::command_details::render_details(frame, state, details)
        }
        Overlay::CommandPalette(palette) => {
            let visible = palette.visible_commands();
            let command_rows = command_palette_rows(&visible);
            let height = (command_rows.len() as u16 + 5).clamp(8, 22);
            let modal = modal_rects(frame.area(), MAX_OVERLAY_WIDTH, height);
            render_modal_frame(
                frame,
                state,
                modal,
                "Commands",
                SemanticColor::BorderFocus,
                true,
            );
            let inner = modal.inner;
            if inner.width == 0 || inner.height == 0 {
                return;
            }

            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(2), Constraint::Min(1)])
                .split(inner);
            let query = palette.query_viewport(rows[0].width as usize);
            let query_text = if palette.query().is_empty() {
                Span::styled(
                    "Search commands",
                    semantic_style(state, SemanticColor::Muted).add_modifier(Modifier::DIM),
                )
            } else {
                Span::styled(query.text, semantic_style(state, SemanticColor::Assistant))
            };
            frame.render_widget(Paragraph::new(Line::from(query_text)), rows[0]);

            let list_width = rows[1].width as usize;
            let visible_height = usize::from(rows[1].height).max(1);
            let selected_row = command_rows
                .iter()
                .position(|row| {
                    matches!(row, CommandPaletteRow::Command { index, .. } if *index == palette.selected())
                })
                .unwrap_or(0);
            let scroll_offset = selected_row
                .saturating_sub(visible_height.saturating_sub(1))
                .min(command_rows.len().saturating_sub(visible_height));
            let lines = command_rows
                .iter()
                .skip(scroll_offset)
                .take(visible_height)
                .map(|row| match row {
                    CommandPaletteRow::Category(category) => command_category_line(state, category),
                    CommandPaletteRow::Command { index, spec } => {
                        let key_binding = spec
                            .key_action
                            .and_then(|action| state.keymap().binding_label_for(action))
                            .unwrap_or_default();
                        let slash_alias = spec
                            .slash_name()
                            .map(|name| format!("/{name}"))
                            .unwrap_or_default();
                        let shortcut = match (key_binding.is_empty(), slash_alias.is_empty()) {
                            (false, false) => format!("{key_binding}  {slash_alias}"),
                            (false, true) => key_binding,
                            (true, false) => slash_alias,
                            (true, true) => String::new(),
                        };
                        command_line(
                            state,
                            spec.label,
                            &shortcut,
                            list_width,
                            *index == palette.selected(),
                        )
                    }
                })
                .collect::<Vec<_>>();
            frame.render_widget(Paragraph::new(lines), rows[1]);

            let cursor_x = rows[0]
                .x
                .saturating_add(query.cursor_column as u16)
                .min(rows[0].right().saturating_sub(1));
            frame.set_cursor_position(Position::new(cursor_x, rows[0].y));
        }
        Overlay::Settings(settings) => {
            super::settings_render::render_settings(frame, state, settings)
        }
        Overlay::Skills(picker) => {
            super::skills_overlay::render_skills_overlay(frame, state, picker)
        }
        Overlay::ProviderManager(manager) => {
            super::provider_render::render_provider_manager(frame, state, manager)
        }
        Overlay::ProviderForm(form) => {
            super::provider_render::render_provider_form(frame, state, form)
        }
        Overlay::ModelPicker(picker) => {
            super::provider_render::render_model_picker(frame, state, picker)
        }
        Overlay::ReasoningPicker(picker) => {
            super::provider_render::render_reasoning_picker(frame, state, picker)
        }
        Overlay::PlanView(plan_view) => super::plan_view::render_plan_view(frame, state, plan_view),
        Overlay::PermissionReview(review) => {
            super::permission_render::render_permission_review(frame, state, review)
        }
        Overlay::Dialog(dialog) => render_message_dialog(frame, state, dialog),
        Overlay::SubagentInspect(takeover) => {
            super::subagent_view::render_subagent_takeover(frame, state, takeover)
        }
        Overlay::BackgroundTaskInspect(inspector) => {
            super::render::task_dock::render_background_task_overlay(frame, state, inspector)
        }
    }
}

fn render_message_dialog(frame: &mut Frame<'_>, state: &TuiState, dialog: &MessageDialogOverlay) {
    let split = dialog_split(frame.area(), dialog.message());
    let border_color = match dialog.kind() {
        MessageDialogKind::Info => SemanticColor::BorderFocus,
        MessageDialogKind::Error => SemanticColor::Error,
    };
    render_modal_frame(
        frame,
        state,
        split.modal,
        dialog.title(),
        border_color,
        true,
    );
    frame.render_widget(
        Paragraph::new(dialog.message().to_owned())
            .style(semantic_style(state, SemanticColor::Assistant).patch(code_surface_style(state)))
            .wrap(Wrap { trim: false }),
        split.body,
    );
    render_divider(frame, state, split.divider);
    frame.render_widget(
        Paragraph::new(chip_line(state, &["Close"])).style(code_surface_style(state)),
        split.footer,
    );
}

fn estimated_wrapped_lines(message: &str, width: usize) -> usize {
    message
        .lines()
        .map(|line| UnicodeWidthStr::width(line).max(1).div_ceil(width.max(1)))
        .sum::<usize>()
        .max(1)
}

fn command_line(
    state: &TuiState,
    label: &str,
    shortcut: &str,
    width: usize,
    selected: bool,
) -> Line<'static> {
    let prefix = "    ";
    let content_width = UnicodeWidthStr::width(prefix)
        + UnicodeWidthStr::width(label)
        + UnicodeWidthStr::width(shortcut);
    let gap = width.saturating_sub(content_width).max(1);
    let base = selection_style(state, selected);
    let accent = if selected {
        state
            .theme()
            .color(SemanticColor::Focus)
            .map_or(base, |color| base.fg(color))
            .add_modifier(Modifier::BOLD)
    } else {
        base
    };
    Line::from(vec![
        Span::styled(if selected { "  ▌ " } else { prefix }, accent),
        Span::styled(label.to_owned(), base.add_modifier(Modifier::BOLD)),
        Span::styled(" ".repeat(gap), base),
        Span::styled(
            shortcut.to_owned(),
            foreground_on_base(state, SemanticColor::ToolKeyword, base),
        ),
    ])
}

fn command_palette_rows<'a>(commands: &[&'a CommandSpec]) -> Vec<CommandPaletteRow<'a>> {
    let mut rows = Vec::with_capacity(commands.len().saturating_add(4));
    let mut previous_category = None;
    for (index, command) in commands.iter().enumerate() {
        if previous_category != Some(command.category) {
            rows.push(CommandPaletteRow::Category(command.category));
            previous_category = Some(command.category);
        }
        rows.push(CommandPaletteRow::Command {
            index,
            spec: command,
        });
    }
    rows
}

fn command_category_line(state: &TuiState, category: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            category.to_owned(),
            semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
        ),
    ])
}

pub(super) fn centered_rect(area: Rect, max_width: u16, desired_height: u16) -> Rect {
    let width = area.width.saturating_sub(4).min(max_width).max(1);
    let height = area.height.saturating_sub(2).min(desired_height).max(1);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

pub(super) fn inset(region: Rect, horizontal: u16, vertical: u16) -> Rect {
    Rect::new(
        region.x.saturating_add(horizontal),
        region.y.saturating_add(vertical),
        region.width.saturating_sub(horizontal.saturating_mul(2)),
        region.height.saturating_sub(vertical.saturating_mul(2)),
    )
}

pub(super) fn code_surface_style(state: &TuiState) -> Style {
    modal_surface_style(state)
}

pub(super) fn selection_style(state: &TuiState, selected: bool) -> Style {
    modal_selection_style(state, selected)
}

fn foreground_on_base(state: &TuiState, slot: SemanticColor, base: Style) -> Style {
    state
        .theme()
        .color(slot)
        .map_or(base, |color| base.fg(color))
}

pub(super) fn semantic_style(state: &TuiState, slot: SemanticColor) -> Style {
    state
        .theme()
        .color(slot)
        .map_or_else(Style::default, |color| Style::default().fg(color))
}

struct OverlaySplit {
    modal: ModalRects,
    body: Rect,
    divider: Rect,
    footer: Rect,
}

pub(crate) fn overlay_pointer(
    state: &TuiState,
    area: Rect,
    position: Position,
) -> Option<OverlayPointer> {
    match state.overlay()? {
        Overlay::CommandPalette(palette) => {
            let visible = palette.visible_commands();
            let command_rows = command_palette_rows(&visible);
            let height = (command_rows.len() as u16 + 5).clamp(8, 22);
            let modal = modal_rects(area, MAX_OVERLAY_WIDTH, height);
            if modal.close_hit(position) {
                return Some(OverlayPointer::CloseModal);
            }
            palette_command_at(modal, &command_rows, palette, position).map(OverlayPointer::Palette)
        }
        Overlay::Settings(settings) => {
            super::settings_render::settings_pointer(state, settings, area, position)
                .map(OverlayPointer::Settings)
        }
        Overlay::Skills(picker) => {
            match super::skills_overlay::skills_overlay_pointer(area, picker, position) {
                Some(super::skills_overlay::SkillsPointer::Close) => {
                    Some(OverlayPointer::CloseModal)
                }
                Some(super::skills_overlay::SkillsPointer::Row(index)) => {
                    Some(OverlayPointer::Skill(index))
                }
                None => None,
            }
        }
        Overlay::PlanView(_) => {
            super::plan_view::plan_view_pointer(state, area, position).map(OverlayPointer::PlanView)
        }
        Overlay::PermissionReview(review) => {
            super::permission_render::permission_review_pointer(state, area, review, position)
        }
        Overlay::Dialog(dialog) => {
            let split = dialog_split(area, dialog.message());
            if split.modal.close_hit(position) {
                return Some(OverlayPointer::DialogClose);
            }
            chip_hit(
                Position::new(split.footer.x, split.footer.y),
                &["Close"],
                position,
            )
            .map(|_| OverlayPointer::DialogClose)
        }
        Overlay::ProviderManager(manager) => {
            super::provider_render::provider_manager_pointer(area, manager, position)
                .map(OverlayPointer::ProviderWorkbench)
        }
        Overlay::ProviderForm(form) => {
            super::provider_render::provider_form_pointer(area, state, form, position)
                .map(OverlayPointer::ProviderWorkbench)
        }
        Overlay::ModelPicker(picker) => {
            match super::provider_render::model_picker_pointer(area, picker, position) {
                Some(super::provider_render::ModelPointer::Row(index)) => {
                    Some(OverlayPointer::Model(index))
                }
                Some(super::provider_render::ModelPointer::Manual) => {
                    Some(OverlayPointer::ManualModel)
                }
                Some(super::provider_render::ModelPointer::Close) => {
                    Some(OverlayPointer::CloseModal)
                }
                None => None,
            }
        }
        Overlay::ReasoningPicker(picker) => {
            match super::provider_render::reasoning_pointer(area, picker, position) {
                Some(Ok(index)) => Some(OverlayPointer::Reasoning(index)),
                Some(Err(())) => Some(OverlayPointer::CloseModal),
                None => None,
            }
        }
        Overlay::CommandDetails(details) => {
            super::command_details::details_pointer(state, area, details, position)
        }
        Overlay::SubagentInspect(_) => {
            super::subagent_view::subagent_takeover_pointer(state, area, position)
                .map(OverlayPointer::Subagent)
        }
        Overlay::BackgroundTaskInspect(inspector) => {
            super::render::task_dock::background_task_overlay_pointer(
                state, inspector, area, position,
            )
            .map(OverlayPointer::BackgroundTask)
        }
    }
}

fn dialog_split(area: Rect, message: &str) -> OverlaySplit {
    let desired_width = area.width.clamp(8, 72);
    let content_width = usize::from(desired_width.saturating_sub(6).max(1));
    let body_lines = estimated_wrapped_lines(message, content_width);
    let desired_height = u16::try_from(body_lines.saturating_add(7))
        .unwrap_or(20)
        .clamp(8, 20);
    split_overlay(area, desired_width, desired_height, 1)
}

fn split_overlay(area: Rect, width: u16, height: u16, footer_height: u16) -> OverlaySplit {
    let modal = modal_rects(area, width, height);
    let inner = modal.inner;
    let divider_height = u16::from(inner.height >= footer_height.saturating_add(3));
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(divider_height),
            Constraint::Length(footer_height),
        ])
        .split(inner);
    OverlaySplit {
        modal,
        body: rows[0],
        divider: rows[1],
        footer: rows[2],
    }
}

fn palette_command_at(
    modal: ModalRects,
    command_rows: &[CommandPaletteRow<'_>],
    palette: &CommandPalette,
    position: Position,
) -> Option<usize> {
    let inner = modal.inner;
    if inner.width == 0 || inner.height == 0 {
        return None;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(inner);
    if !rows[1].contains(position) {
        return None;
    }
    let visible_height = usize::from(rows[1].height).max(1);
    let selected_row = command_rows
        .iter()
        .position(|row| {
            matches!(row, CommandPaletteRow::Command { index, .. } if *index == palette.selected())
        })
        .unwrap_or(0);
    let scroll_offset = selected_row
        .saturating_sub(visible_height.saturating_sub(1))
        .min(command_rows.len().saturating_sub(visible_height));
    let local = usize::from(position.y.saturating_sub(rows[1].y));
    match command_rows.get(scroll_offset + local)? {
        CommandPaletteRow::Command { index, .. } => Some(*index),
        CommandPaletteRow::Category(_) => None,
    }
}
