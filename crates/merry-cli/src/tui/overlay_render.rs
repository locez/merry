use super::{
    click_target::{OverlayPointer, chip_hit, chip_line},
    keymap::KeyAction,
    overlay::SettingItem,
    overlay::{
        CommandPalette, CommandSpec, MessageDialogKind, MessageDialogOverlay, Overlay,
        PermissionReviewOverlay, PlanApprovalOverlay,
    },
    state::TuiState,
    theme::{SemanticColor, dim_color},
};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

const MAX_OVERLAY_WIDTH: u16 = 76;

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
            let region = centered_rect(frame.area(), MAX_OVERLAY_WIDTH, height);
            render_surface(frame, state, region, " M  Commands ");
            let inner = inset(region, 2, 1);
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
        Overlay::Settings(_) => render_settings(frame, state),
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
        Overlay::PlanApproval(approval) => render_plan_approval(frame, state, approval),
        Overlay::PermissionReview(review) => render_permission_review(frame, state, review),
        Overlay::Dialog(dialog) => render_message_dialog(frame, state, dialog),
        Overlay::Shortcuts(_) => render_shortcuts(frame, state),
    }
}

fn render_plan_approval(frame: &mut Frame<'_>, state: &TuiState, approval: &PlanApprovalOverlay) {
    let split = approval_split(frame.area(), approval.message());
    let region = split.region;
    frame.render_widget(Clear, region);
    frame.render_widget(
        Block::default()
            .title(" M  Approve plan and execute? ")
            .borders(Borders::ALL)
            .border_type(BorderType::Plain)
            .border_style(
                semantic_style(state, SemanticColor::Warning).add_modifier(Modifier::BOLD),
            )
            .style(code_surface_style(state)),
        region,
    );
    frame.render_widget(
        Paragraph::new(approval.message().to_owned())
            .style(semantic_style(state, SemanticColor::Assistant))
            .wrap(Wrap { trim: false }),
        split.body,
    );
    frame.render_widget(
        Paragraph::new(chip_line(state, &["Approve", "Back"])),
        split.footer,
    );
}

fn render_permission_review(
    frame: &mut Frame<'_>,
    state: &TuiState,
    review: &PermissionReviewOverlay,
) {
    let split = permission_split(frame.area(), review.body());
    let region = split.region;
    frame.render_widget(Clear, region);
    frame.render_widget(
        Block::default()
            .title(" Allow this command? ")
            .borders(Borders::ALL)
            .border_type(BorderType::Plain)
            .border_style(
                semantic_style(state, SemanticColor::Warning).add_modifier(Modifier::BOLD),
            )
            .style(code_surface_style(state)),
        region,
    );
    frame.render_widget(
        Paragraph::new(review.body().to_owned())
            .style(semantic_style(state, SemanticColor::Assistant))
            .wrap(Wrap { trim: false }),
        split.body,
    );
    frame.render_widget(
        Paragraph::new(chip_line(state, &["Allow", "Don't allow"])),
        split.footer,
    );
}

fn render_settings(frame: &mut Frame<'_>, state: &TuiState) {
    let region = centered_rect(frame.area(), MAX_OVERLAY_WIDTH, 22);
    render_surface(frame, state, region, " M  Settings ");
    let inner = inset(region, 2, 1);
    let selected = state.selected_setting();
    let model_editor_viewport = state
        .settings_model_editor()
        .map(|input| input.viewport(inner.width.saturating_sub(36).max(1).into()));
    let model_value = model_editor_viewport
        .as_ref()
        .map(|viewport| format!("{}  editing", viewport.text))
        .unwrap_or_else(|| state.setting_value(SettingItem::DefaultModel));
    let reasoning_editor_viewport = state
        .settings_reasoning_editor()
        .map(|input| input.viewport(inner.width.saturating_sub(36).max(1).into()));
    let reasoning_value = reasoning_editor_viewport
        .as_ref()
        .map(|viewport| format!("{}  editing", viewport.text))
        .unwrap_or_else(|| state.setting_value(SettingItem::ReasoningEffort));
    let context_window_editor_viewport = state
        .settings_context_window_editor()
        .map(|input| input.viewport(inner.width.saturating_sub(36).max(1).into()));
    let context_window_value = context_window_editor_viewport
        .as_ref()
        .map(|viewport| format!("{}  editing", viewport.text))
        .unwrap_or_else(|| state.setting_value(SettingItem::ContextWindow));
    let lines = vec![
        section_line(state, "Appearance"),
        setting_line(
            state,
            "Code theme",
            &state.setting_value(SettingItem::CodeTheme),
            selected == Some(SettingItem::CodeTheme),
        ),
        Line::default(),
        section_line(state, "Model defaults"),
        setting_line(
            state,
            "Default provider",
            &state.setting_value(SettingItem::DefaultProvider),
            selected == Some(SettingItem::DefaultProvider),
        ),
        setting_line(
            state,
            "Default model",
            &model_value,
            selected == Some(SettingItem::DefaultModel),
        ),
        setting_line(
            state,
            "Reasoning effort",
            &reasoning_value,
            selected == Some(SettingItem::ReasoningEffort),
        ),
        Line::default(),
        section_line(state, "Context"),
        setting_line(
            state,
            "Context window",
            &context_window_value,
            selected == Some(SettingItem::ContextWindow),
        ),
        setting_line(
            state,
            "Auto compaction",
            &state.setting_value(SettingItem::AutoCompaction),
            selected == Some(SettingItem::AutoCompaction),
        ),
        setting_line(
            state,
            "Context strategy",
            &state.setting_value(SettingItem::ContextStrategy),
            selected == Some(SettingItem::ContextStrategy),
        ),
        Line::default(),
        section_line(state, "Runtime"),
        setting_line(
            state,
            "Subagents",
            &state.setting_value(SettingItem::Subagents),
            selected == Some(SettingItem::Subagents),
        ),
        setting_line(
            state,
            "Max threads",
            &state.setting_value(SettingItem::MaxThreads),
            selected == Some(SettingItem::MaxThreads),
        ),
        Line::default(),
        setting_line(
            state,
            "Keyboard shortcuts",
            &state.setting_value(SettingItem::KeyboardShortcuts),
            selected == Some(SettingItem::KeyboardShortcuts),
        ),
        Line::from(Span::styled(
            state.settings_notice().unwrap_or(""),
            semantic_style(state, SemanticColor::Muted),
        )),
    ];
    debug_assert_eq!(lines.len(), SETTINGS_LINE_COUNT);
    let selected_line = selected.map(setting_line_index).unwrap_or(0);
    let visible_height = usize::from(inner.height).max(1);
    let scroll_offset = settings_scroll(selected_line, visible_height);
    frame.render_widget(
        Paragraph::new(lines).scroll((scroll_offset as u16, 0)),
        inner,
    );
    if let Some(viewport) = model_editor_viewport {
        let value_x = inner.x.saturating_add(26);
        let model_row = setting_line_index(SettingItem::DefaultModel);
        if let Some(visible_row) = model_row.checked_sub(scroll_offset)
            && visible_row < visible_height
        {
            frame.set_cursor_position(Position::new(
                value_x
                    .saturating_add(viewport.cursor_column as u16)
                    .min(inner.right().saturating_sub(1)),
                inner.y.saturating_add(visible_row as u16),
            ));
        }
    }
    if let Some(viewport) = context_window_editor_viewport {
        let value_x = inner.x.saturating_add(26);
        let context_window_row = setting_line_index(SettingItem::ContextWindow);
        if let Some(visible_row) = context_window_row.checked_sub(scroll_offset)
            && visible_row < visible_height
        {
            frame.set_cursor_position(Position::new(
                value_x
                    .saturating_add(viewport.cursor_column as u16)
                    .min(inner.right().saturating_sub(1)),
                inner.y.saturating_add(visible_row as u16),
            ));
        }
    }
    if let Some(viewport) = reasoning_editor_viewport {
        let value_x = inner.x.saturating_add(26);
        let reasoning_row = setting_line_index(SettingItem::ReasoningEffort);
        if let Some(visible_row) = reasoning_row.checked_sub(scroll_offset)
            && visible_row < visible_height
        {
            frame.set_cursor_position(Position::new(
                value_x
                    .saturating_add(viewport.cursor_column as u16)
                    .min(inner.right().saturating_sub(1)),
                inner.y.saturating_add(visible_row as u16),
            ));
        }
    }
}

fn setting_line_index(item: SettingItem) -> usize {
    match item {
        SettingItem::CodeTheme => 1,
        SettingItem::DefaultProvider => 4,
        SettingItem::DefaultModel => 5,
        SettingItem::ReasoningEffort => 6,
        SettingItem::ContextWindow => 9,
        SettingItem::AutoCompaction => 10,
        SettingItem::ContextStrategy => 11,
        SettingItem::Subagents => 14,
        SettingItem::MaxThreads => 15,
        SettingItem::KeyboardShortcuts => 17,
    }
}

fn render_shortcuts(frame: &mut Frame<'_>, state: &TuiState) {
    let lines = vec![
        shortcut_line_for_action(state, "Submit next", KeyAction::SubmitNext),
        shortcut_line_for_action(state, "Submit backlog", KeyAction::SubmitBacklog),
        shortcut_line_for_action(state, "Command palette", KeyAction::OpenCommandPanel),
        shortcut_line_for_action(state, "Toggle plan", KeyAction::TogglePlan),
        shortcut_line_for_action(state, "Command output", KeyAction::OpenCommandDetails),
        shortcut_line_for_action(state, "Follow latest", KeyAction::FollowLatest),
        #[cfg(feature = "web")]
        shortcut_line_for_action(
            state,
            "Open trajectory in browser",
            KeyAction::OpenSessionInBrowser,
        ),
        shortcut_line_for_action(
            state,
            "Previous user input",
            KeyAction::ReviewPreviousUserInput,
        ),
        shortcut_line_for_action(state, "Search history", KeyAction::HistorySearch),
        shortcut_line_for_action(state, "Comment mode", KeyAction::ToggleComment),
        shortcut_line_for_action(state, "Resume suspended", KeyAction::ResumeSuspended),
        shortcut_line_for_action(state, "Discard suspended", KeyAction::DiscardSuspended),
        shortcut_line_for_action(state, "Interrupt", KeyAction::Interrupt),
        shortcut_line_for_action(state, "Quit", KeyAction::Quit),
    ];
    let height = u16::try_from(lines.len()).unwrap_or(14).saturating_add(6);
    let region = centered_rect(frame.area(), MAX_OVERLAY_WIDTH, height);
    render_surface(frame, state, region, " M  Keyboard shortcuts ");
    let inner = inset(region, 2, 1);
    frame.render_widget(Paragraph::new(lines), inner);
}

pub(super) fn render_surface(frame: &mut Frame<'_>, state: &TuiState, region: Rect, title: &str) {
    frame.render_widget(Clear, region);
    frame.render_widget(
        Block::default()
            .title(title.to_owned())
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD))
            .style(code_surface_style(state)),
        region,
    );
}

fn render_message_dialog(frame: &mut Frame<'_>, state: &TuiState, dialog: &MessageDialogOverlay) {
    let split = dialog_split(frame.area(), dialog.message());
    let region = split.region;
    let border_color = match dialog.kind() {
        MessageDialogKind::Info => SemanticColor::Focus,
        MessageDialogKind::Error => SemanticColor::Error,
    };
    frame.render_widget(Clear, region);
    frame.render_widget(
        Block::default()
            .title(format!(" M  {} ", dialog.title()))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(semantic_style(state, border_color).add_modifier(Modifier::BOLD))
            .style(code_surface_style(state)),
        region,
    );
    frame.render_widget(
        Paragraph::new(dialog.message().to_owned())
            .style(semantic_style(state, SemanticColor::Assistant))
            .wrap(Wrap { trim: false }),
        split.body,
    );
    frame.render_widget(Paragraph::new(chip_line(state, &["Close"])), split.footer);
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

fn section_line(state: &TuiState, label: &str) -> Line<'static> {
    Line::from(Span::styled(
        label.to_owned(),
        semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
    ))
}

fn setting_line(state: &TuiState, label: &str, value: &str, selected: bool) -> Line<'static> {
    let base = selection_style(state, selected);
    Line::from(vec![
        Span::styled(if selected { "▌ " } else { "  " }, base),
        Span::styled(format!("{label:<24}"), base),
        Span::styled(
            value.to_owned(),
            semantic_style(state, SemanticColor::ToolKeyword).patch(base),
        ),
    ])
}

fn shortcut_line(state: &TuiState, label: &str, shortcut: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {label:<34}"), code_surface_style(state)),
        Span::styled(
            shortcut.to_owned(),
            semantic_style(state, SemanticColor::ToolKeyword),
        ),
    ])
}

fn shortcut_line_for_action(state: &TuiState, label: &str, action: KeyAction) -> Line<'static> {
    let shortcut = state
        .keymap()
        .binding_label_for(action)
        .unwrap_or_else(|| "Unbound".to_owned());
    shortcut_line(state, label, &shortcut)
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
    state
        .theme()
        .color(SemanticColor::CodeBackground)
        .map_or_else(Style::default, |color| Style::default().bg(color))
}

pub(super) fn selection_style(state: &TuiState, selected: bool) -> Style {
    if !selected {
        return code_surface_style(state);
    }
    let foreground = state
        .theme()
        .color(SemanticColor::Assistant)
        .unwrap_or(ratatui::style::Color::White);
    let background = state
        .theme()
        .color(SemanticColor::Focus)
        .map(dim_color)
        .unwrap_or(ratatui::style::Color::Magenta);
    Style::default().fg(foreground).bg(background)
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

const SETTINGS_LINE_COUNT: usize = 19;

struct OverlaySplit {
    region: Rect,
    body: Rect,
    footer: Rect,
}

pub(crate) fn overlay_pointer(
    state: &TuiState,
    area: Rect,
    position: Position,
) -> Option<OverlayPointer> {
    match state.overlay()? {
        Overlay::CommandPalette(palette) => {
            palette_command_at(area, palette, position).map(OverlayPointer::Palette)
        }
        Overlay::Settings(_) => {
            settings_item_at(state, area, position).map(OverlayPointer::Setting)
        }
        Overlay::PlanApproval(approval) => {
            let footer = approval_split(area, approval.message()).footer;
            match chip_hit(
                Position::new(footer.x, footer.y),
                &["Approve", "Back"],
                position,
            ) {
                Some(0) => Some(OverlayPointer::PlanApprove),
                Some(1) => Some(OverlayPointer::PlanBack),
                _ => None,
            }
        }
        Overlay::PermissionReview(review) => {
            let footer = permission_split(area, review.body()).footer;
            match chip_hit(
                Position::new(footer.x, footer.y),
                &["Allow", "Don't allow"],
                position,
            ) {
                Some(0) => Some(OverlayPointer::PermissionAllow),
                Some(1) => Some(OverlayPointer::PermissionReject),
                _ => None,
            }
        }
        Overlay::Dialog(dialog) => {
            let footer = dialog_split(area, dialog.message()).footer;
            chip_hit(Position::new(footer.x, footer.y), &["Close"], position)
                .map(|_| OverlayPointer::DialogClose)
        }
        Overlay::ProviderManager(manager) => {
            super::provider_render::provider_manager_row(area, manager, position)
                .map(OverlayPointer::Provider)
        }
        Overlay::ModelPicker(picker) => {
            match super::provider_render::model_picker_pointer(area, picker, position) {
                Some(super::provider_render::ModelPointer::Row(index)) => {
                    Some(OverlayPointer::Model(index))
                }
                Some(super::provider_render::ModelPointer::Manual) => {
                    Some(OverlayPointer::ManualModel)
                }
                None => None,
            }
        }
        Overlay::ReasoningPicker(picker) => {
            super::provider_render::reasoning_row(area, picker, position)
                .map(OverlayPointer::Reasoning)
        }
        Overlay::ProviderForm(_) | Overlay::CommandDetails(_) | Overlay::Shortcuts(_) => None,
    }
}

fn approval_split(area: Rect, message: &str) -> OverlaySplit {
    let desired_width = area.width.saturating_sub(4).min(MAX_OVERLAY_WIDTH);
    let content_width = usize::from(desired_width.saturating_sub(4)).max(1);
    let body_lines = estimated_wrapped_lines(message, content_width);
    let desired_height = u16::try_from(body_lines.saturating_add(6))
        .unwrap_or(24)
        .clamp(10, area.height.saturating_sub(2).min(24));
    split_overlay(area, desired_width, desired_height, 1)
}

fn permission_split(area: Rect, body: &str) -> OverlaySplit {
    let desired_width = area.width.saturating_sub(4).min(MAX_OVERLAY_WIDTH);
    let content_width = usize::from(desired_width.saturating_sub(4)).max(1);
    let body_lines = estimated_wrapped_lines(body, content_width);
    let available_height = area.height.saturating_sub(2);
    let max_height = if available_height < 12 {
        12
    } else {
        available_height.min(28)
    };
    let desired_height = u16::try_from(body_lines.saturating_add(7))
        .unwrap_or(26)
        .clamp(12, max_height);
    split_overlay(area, desired_width, desired_height, 2)
}

fn dialog_split(area: Rect, message: &str) -> OverlaySplit {
    let desired_width = area.width.clamp(8, 72);
    let content_width = usize::from(desired_width.saturating_sub(6).max(1));
    let body_lines = estimated_wrapped_lines(message, content_width);
    let desired_height = u16::try_from(body_lines.saturating_add(6))
        .unwrap_or(20)
        .clamp(8, 20);
    split_overlay(area, desired_width, desired_height, 1)
}

fn split_overlay(area: Rect, width: u16, height: u16, footer_height: u16) -> OverlaySplit {
    let region = centered_rect(area, width, height);
    let inner = inset(region, 2, 1);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(footer_height)])
        .split(inner);
    OverlaySplit {
        region,
        body: rows[0],
        footer: rows[1],
    }
}

fn palette_command_at(area: Rect, palette: &CommandPalette, position: Position) -> Option<usize> {
    let visible = palette.visible_commands();
    let command_rows = command_palette_rows(&visible);
    let height = (command_rows.len() as u16 + 5).clamp(8, 22);
    let region = centered_rect(area, MAX_OVERLAY_WIDTH, height);
    let inner = inset(region, 2, 1);
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

fn settings_item_at(state: &TuiState, area: Rect, position: Position) -> Option<SettingItem> {
    if state.settings_model_editor().is_some()
        || state.settings_reasoning_editor().is_some()
        || state.settings_context_window_editor().is_some()
    {
        return None;
    }
    let region = centered_rect(area, MAX_OVERLAY_WIDTH, 22);
    let inner = inset(region, 2, 1);
    if !inner.contains(position) {
        return None;
    }
    let selected_line = state
        .selected_setting()
        .map(setting_line_index)
        .unwrap_or(0);
    let visible_height = usize::from(inner.height).max(1);
    let scroll = settings_scroll(selected_line, visible_height);
    let line = scroll + usize::from(position.y.saturating_sub(inner.y));
    SettingItem::ALL
        .into_iter()
        .find(|item| setting_line_index(*item) == line)
}

fn settings_scroll(selected_line: usize, visible_height: usize) -> usize {
    selected_line
        .saturating_sub(visible_height.saturating_sub(1))
        .min(SETTINGS_LINE_COUNT.saturating_sub(visible_height))
}
