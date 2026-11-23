//! Two-column grouped Settings Workbench renderer, compact drill-down renderer, and hit-tester.

mod compact;

use compact::{
    render_compact_back_bar, render_compact_categories, render_compact_choice_menu,
    split_compact_back_header,
};

use super::{
    click_target::truncate_columns,
    keymap::alt_modifier_name,
    modal_frame::{
        ModalRects, modal_rects, modal_selection_style, modal_surface_style, render_divider,
        render_modal_frame, render_vertical_divider,
    },
    overlay_render::semantic_style,
    settings_overlay::{SettingItem, SettingsCategory, SettingsFocus, SettingsOverlay},
    state::TuiState,
    theme::SemanticColor,
};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

const WORKBENCH_WIDTH: u16 = 104;
const WORKBENCH_HEIGHT: u16 = 24;
const TWO_COLUMN_MIN_INNER: u16 = 64;
const STACKED_ITEM_MAX_WIDTH: u16 = 52;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsPointer {
    Close,
    Back,
    Category(SettingsCategory),
    Item(SettingItem),
    Choice(usize),
    ResetSelected,
}

pub(crate) fn settings_uses_compact_layout(area: Rect) -> bool {
    !settings_layout(area).two_column
}

#[derive(Debug, Clone, Copy)]
struct SettingsLayout {
    modal: ModalRects,
    two_column: bool,
    categories_pane: Rect,
    divider: Rect,
    items_pane: Rect,
    footer_divider: Rect,
    footer: Rect,
}

fn settings_layout(area: Rect) -> SettingsLayout {
    let modal = modal_rects(area, WORKBENCH_WIDTH, WORKBENCH_HEIGHT);
    let inner = modal.inner;
    if inner.width == 0 || inner.height == 0 {
        return SettingsLayout {
            modal,
            two_column: false,
            categories_pane: Rect::default(),
            divider: Rect::default(),
            items_pane: Rect::default(),
            footer_divider: Rect::default(),
            footer: Rect::default(),
        };
    }

    let has_footer_divider = inner.height >= 8;
    let footer_height: u16 = if inner.height >= 7 { 2 } else { 1 };
    let v_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(u16::from(has_footer_divider)),
            Constraint::Length(footer_height),
        ])
        .split(inner);

    let main = v_chunks[0];
    let footer_divider = v_chunks[1];
    let footer = v_chunks[2];

    if main.width >= TWO_COLUMN_MIN_INNER {
        let left_width = (main.width / 4).clamp(18, 24);
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(left_width),
                Constraint::Length(3),
                Constraint::Min(1),
            ])
            .split(main);
        let divider = Rect::new(cols[1].x + 1, cols[1].y, 1, cols[1].height);
        SettingsLayout {
            modal,
            two_column: true,
            categories_pane: cols[0],
            divider,
            items_pane: cols[2],
            footer_divider,
            footer,
        }
    } else {
        SettingsLayout {
            modal,
            two_column: false,
            categories_pane: main,
            divider: Rect::default(),
            items_pane: main,
            footer_divider,
            footer,
        }
    }
}

pub(super) fn render_settings(frame: &mut Frame<'_>, state: &TuiState, settings: &SettingsOverlay) {
    let layout = settings_layout(frame.area());
    render_modal_frame(
        frame,
        state,
        layout.modal,
        "Settings",
        SemanticColor::BorderFocus,
        true,
    );
    if layout.modal.inner.is_empty() {
        return;
    }

    if layout.two_column {
        render_categories_rail(frame, state, settings, layout.categories_pane);
        render_vertical_divider(frame, state, layout.divider);
        render_items_pane(frame, state, settings, layout.items_pane, true);
    } else if let Some(choice_menu) = settings.choice_menu() {
        render_compact_choice_menu(frame, state, choice_menu, layout.items_pane);
    } else if settings.focus() == SettingsFocus::Categories {
        render_compact_categories(frame, state, settings, layout.categories_pane);
    } else {
        let (back_bar, body) = split_compact_back_header(layout.items_pane);
        render_compact_back_bar(
            frame,
            state,
            "Settings",
            settings.category().label(),
            back_bar,
        );
        render_items_pane(frame, state, settings, body, false);
    }

    render_divider(frame, state, layout.footer_divider);
    render_footer(frame, state, settings, layout.footer, layout.two_column);
}

fn render_categories_rail(
    frame: &mut Frame<'_>,
    state: &TuiState,
    settings: &SettingsOverlay,
    area: Rect,
) {
    if area.is_empty() {
        return;
    }
    let surface = modal_surface_style(state);
    let focused = settings.focus() == SettingsFocus::Categories;
    let header_slot = if focused {
        SemanticColor::Focus
    } else {
        SemanticColor::Muted
    };
    let mut lines = Vec::with_capacity(usize::from(area.height));
    lines.push(Line::from(Span::styled(
        "  CATEGORIES",
        semantic_style(state, header_slot)
            .add_modifier(Modifier::BOLD)
            .patch(surface),
    )));
    lines.push(Line::styled("", surface));

    let width = usize::from(area.width);
    for category in SettingsCategory::ALL {
        let active = settings.category() == category;
        let base = modal_selection_style(state, active);
        let bar = if active { "▌ " } else { "  " };
        let bar_style = if active {
            semantic_style(
                state,
                if focused {
                    SemanticColor::Focus
                } else {
                    SemanticColor::Assistant
                },
            )
            .add_modifier(Modifier::BOLD)
            .patch(base)
        } else {
            base
        };
        let label = truncate_columns(category.label(), width.saturating_sub(3).max(1));
        let pad = width.saturating_sub(2 + UnicodeWidthStr::width(label.as_str()));
        let text_style = if active {
            base.add_modifier(Modifier::BOLD)
        } else {
            semantic_style(state, SemanticColor::TextSecondary).patch(base)
        };
        lines.push(Line::from(vec![
            Span::styled(bar, bar_style),
            Span::styled(label, text_style),
            Span::styled(" ".repeat(pad), base),
        ]));
    }

    frame.render_widget(Paragraph::new(lines).style(surface), area);
}

fn item_row_height(two_column: bool, area: Rect) -> usize {
    if !two_column && area.width < STACKED_ITEM_MAX_WIDTH && area.height >= 4 {
        2
    } else {
        1
    }
}

fn render_items_pane(
    frame: &mut Frame<'_>,
    state: &TuiState,
    settings: &SettingsOverlay,
    area: Rect,
    two_column: bool,
) {
    if area.is_empty() {
        return;
    }
    let surface = modal_surface_style(state);
    let items = settings.category().items();
    let focused = settings.focus() == SettingsFocus::Items;
    let selected_item = settings.selected_item();

    let row_height = item_row_height(two_column, area);
    let has_header = two_column && area.height >= 6;
    let header_lines = if has_header { 2usize } else { 0usize };
    let visible_slots = (usize::from(area.height).saturating_sub(header_lines) / row_height).max(1);
    let selected_index = settings.selected_index().min(items.len().saturating_sub(1));
    let scroll_offset = selected_index
        .saturating_sub(visible_slots.saturating_sub(1))
        .min(items.len().saturating_sub(visible_slots));

    let label_width = if area.width >= 46 { 24usize } else { 18usize };
    let value_budget = usize::from(area.width)
        .saturating_sub(label_width + 4)
        .max(1);

    let mut lines = Vec::with_capacity(usize::from(area.height));
    if has_header {
        if settings.category() == SettingsCategory::Shortcuts {
            let title = "Keyboard shortcuts";
            let subtitle_budget =
                usize::from(area.width).saturating_sub(UnicodeWidthStr::width(title) + 3);
            let mut header_spans = vec![Span::styled(
                title,
                semantic_style(state, SemanticColor::Heading)
                    .add_modifier(Modifier::BOLD)
                    .patch(surface),
            )];
            if subtitle_budget >= 12 {
                let subtitle = format!("(Command palette: {}+X fixed)", alt_modifier_name());
                header_spans.push(Span::styled("   ", surface));
                header_spans.push(Span::styled(
                    truncate_columns(&subtitle, subtitle_budget),
                    semantic_style(state, SemanticColor::Muted).patch(surface),
                ));
            }
            lines.push(Line::from(header_spans));
        } else {
            lines.push(Line::from(Span::styled(
                settings.category().label(),
                semantic_style(state, SemanticColor::Heading)
                    .add_modifier(Modifier::BOLD)
                    .patch(surface),
            )));
        }
        lines.push(Line::styled("", surface));
    }

    let mut editor_cursor: Option<(usize, usize, usize)> = None;

    for (visible_idx, &item) in items
        .iter()
        .skip(scroll_offset)
        .take(visible_slots)
        .enumerate()
    {
        let selected = focused && selected_item == Some(item);
        let base = if selected {
            modal_selection_style(state, true)
        } else {
            surface
        };
        let marker = if selected { "▌ " } else { "  " };
        let marker_style = if selected {
            semantic_style(state, SemanticColor::Focus)
                .add_modifier(Modifier::BOLD)
                .patch(base)
        } else {
            base
        };
        let label_style = if selected {
            semantic_style(state, SemanticColor::Assistant)
                .add_modifier(Modifier::BOLD)
                .patch(base)
        } else {
            semantic_style(state, SemanticColor::TextSecondary).patch(base)
        };

        let active_editor = active_editor_for_item(state, item);
        let capturing_shortcut = matches!(
            item,
            SettingItem::Shortcut(action) if settings.capturing_shortcut() == Some(action)
        );
        let stacked = row_height == 2;

        if stacked {
            let width = usize::from(area.width);
            let show_chevron = item.opens_choice_menu()
                || item.opens_model_picker()
                || item.opens_reasoning_picker()
                || item.opens_inline_editor()
                || item.is_action_only()
                || item == SettingItem::ActiveProvider;
            let label_budget = width
                .saturating_sub(if show_chevron { 5 } else { 3 })
                .max(1);
            let label_text = truncate_columns(item.label(), label_budget);
            let mut top_spans = vec![
                Span::styled(marker, marker_style),
                Span::styled(label_text.clone(), label_style),
            ];
            let used = 2 + UnicodeWidthStr::width(label_text.as_str());
            if show_chevron {
                let pad = width.saturating_sub(used + 2);
                top_spans.push(Span::styled(" ".repeat(pad), base));
                top_spans.push(Span::styled(
                    " ▸",
                    semantic_style(state, SemanticColor::Focus).patch(base),
                ));
            } else {
                top_spans.push(Span::styled(" ".repeat(width.saturating_sub(used)), base));
            }
            lines.push(Line::from(top_spans));

            let stacked_budget = width.saturating_sub(5).max(1);
            let (value_text, value_slot) = if let Some(editor) = active_editor {
                let viewport = editor.viewport(stacked_budget.saturating_sub(10).max(1));
                editor_cursor = Some((
                    header_lines + visible_idx * 2 + 1,
                    4,
                    viewport.cursor_column,
                ));
                (format!("{}  editing", viewport.text), SemanticColor::Focus)
            } else if capturing_shortcut {
                (
                    truncate_columns("Press key combo… (Esc cancel)", stacked_budget),
                    SemanticColor::Focus,
                )
            } else {
                let raw = state.setting_value(item);
                let slot = setting_value_color(state, item);
                (truncate_columns(&raw, stacked_budget), slot)
            };
            let value_used = 4 + UnicodeWidthStr::width(value_text.as_str());
            lines.push(Line::from(vec![
                Span::styled("    ", base),
                Span::styled(value_text, semantic_style(state, value_slot).patch(base)),
                Span::styled(" ".repeat(width.saturating_sub(value_used)), base),
            ]));
            continue;
        }

        let label = format!(
            "{:<width$}",
            truncate_columns(item.label(), label_width.saturating_sub(1).max(1)),
            width = label_width
        );
        let (value_text, value_slot) = if let Some(editor) = active_editor {
            let viewport = editor.viewport(value_budget.saturating_sub(10).max(1));
            editor_cursor = Some((
                header_lines + visible_idx,
                2 + label_width,
                viewport.cursor_column,
            ));
            (format!("{}  editing", viewport.text), SemanticColor::Focus)
        } else if capturing_shortcut {
            (
                truncate_columns("Press key combo… (Esc cancel)", value_budget),
                SemanticColor::Focus,
            )
        } else {
            let raw = state.setting_value(item);
            let slot = setting_value_color(state, item);
            (truncate_columns(&raw, value_budget), slot)
        };

        let spans = vec![
            Span::styled(marker, marker_style),
            Span::styled(label, label_style),
            Span::styled(value_text, semantic_style(state, value_slot).patch(base)),
        ];

        lines.push(Line::from(spans));
    }

    if settings.category() == SettingsCategory::Permissions
        && lines.len() + 2 <= usize::from(area.height)
    {
        lines.push(Line::styled("", surface));
        lines.push(Line::from(Span::styled(
            truncate_columns(
                "  Configure paths, env, rules, skills & MCP in config.toml.",
                usize::from(area.width),
            ),
            semantic_style(state, SemanticColor::Muted).patch(surface),
        )));
    }

    frame.render_widget(Paragraph::new(lines).style(surface), area);

    if let Some((row, col_offset, cursor_col)) = editor_cursor {
        let cursor_x = area
            .x
            .saturating_add(u16::try_from(col_offset).unwrap_or(4))
            .saturating_add(u16::try_from(cursor_col).unwrap_or(0))
            .min(area.right().saturating_sub(1));
        let cursor_y = area
            .y
            .saturating_add(u16::try_from(row).unwrap_or(0))
            .min(area.bottom().saturating_sub(1));
        frame.set_cursor_position(Position::new(cursor_x, cursor_y));
    }
}

fn setting_value_color(state: &TuiState, item: SettingItem) -> SemanticColor {
    if item.is_action_only() {
        SemanticColor::Focus
    } else if state.is_setting_overridden(item) {
        SemanticColor::ToolKeyword
    } else {
        SemanticColor::Muted
    }
}

fn footer_hint_prefix(two_column: bool, settings: &SettingsOverlay) -> &'static str {
    if two_column {
        if settings.focus() == SettingsFocus::Items {
            if settings.category() == SettingsCategory::Shortcuts {
                "↑↓ Move   Enter Rebind   Tab Categories   Esc Back   "
            } else {
                "↑↓ Move   ←→ Change   Enter Edit   Tab Categories   Esc Back   "
            }
        } else {
            "↑↓ Move   ←→ Change   Enter Edit   Tab Categories   Esc Close   "
        }
    } else if settings.choice_menu().is_some() {
        "↑↓ Move  Enter Select  Esc Back"
    } else if settings.focus() == SettingsFocus::Categories {
        "↑↓ Move  Enter Open  Esc Close"
    } else if settings.category() == SettingsCategory::Shortcuts {
        "Enter Rebind  Esc Back  "
    } else {
        "Enter Open  Esc Back  "
    }
}

fn render_footer(
    frame: &mut Frame<'_>,
    state: &TuiState,
    settings: &SettingsOverlay,
    area: Rect,
    two_column: bool,
) {
    if area.is_empty() {
        return;
    }
    let surface = modal_surface_style(state);
    let selected = settings.selected_item();

    let mut lines = Vec::with_capacity(usize::from(area.height));
    if area.height >= 2 {
        let info = if settings.is_capturing_shortcut() {
            Line::from(Span::styled(
                truncate_columns(
                    "Press a supported key combo to rebind (Esc cancels).",
                    usize::from(area.width),
                ),
                semantic_style(state, SemanticColor::Focus).patch(surface),
            ))
        } else if let Some(notice) = settings.notice() {
            Line::from(Span::styled(
                truncate_columns(notice, usize::from(area.width)),
                semantic_style(state, SemanticColor::Status).patch(surface),
            ))
        } else if settings.focus() == SettingsFocus::Categories {
            Line::from(Span::styled(
                truncate_columns(settings.category().summary(), usize::from(area.width)),
                semantic_style(state, SemanticColor::Muted).patch(surface),
            ))
        } else if let Some(menu) = settings.choice_menu() {
            Line::from(Span::styled(
                truncate_columns(menu.item().description(), usize::from(area.width)),
                semantic_style(state, SemanticColor::Muted).patch(surface),
            ))
        } else if let Some(item) = selected {
            let source_tag = if item.is_action_only() {
                ""
            } else if item.is_next_session() {
                if state.is_setting_overridden(item) {
                    "Saved · next session · "
                } else {
                    "Next session · "
                }
            } else if state.is_setting_overridden(item) {
                "[override] "
            } else {
                ""
            };
            let source_color = if state.is_setting_overridden(item) {
                SemanticColor::ToolKeyword
            } else {
                SemanticColor::Muted
            };
            let desc_budget = usize::from(area.width)
                .saturating_sub(UnicodeWidthStr::width(source_tag))
                .max(1);
            Line::from(vec![
                Span::styled(
                    source_tag,
                    semantic_style(state, source_color).patch(surface),
                ),
                Span::styled(
                    truncate_columns(item.description(), desc_budget),
                    semantic_style(state, SemanticColor::Muted).patch(surface),
                ),
            ])
        } else {
            Line::styled("", surface)
        };
        lines.push(info);
    }

    let can_reset = !settings.is_capturing_shortcut()
        && settings.choice_menu().is_none()
        && settings.focus() == SettingsFocus::Items
        && selected.is_some_and(|item| state.is_setting_overridden(item));
    let mut hint_spans = if settings.is_capturing_shortcut() {
        vec![
            Span::styled(
                "Key combo",
                semantic_style(state, SemanticColor::Focus)
                    .add_modifier(Modifier::BOLD)
                    .patch(surface),
            ),
            Span::styled(
                " Bind   ",
                semantic_style(state, SemanticColor::Muted).patch(surface),
            ),
            Span::styled(
                "Esc",
                semantic_style(state, SemanticColor::Focus)
                    .add_modifier(Modifier::BOLD)
                    .patch(surface),
            ),
            Span::styled(
                " Cancel",
                semantic_style(state, SemanticColor::Muted).patch(surface),
            ),
        ]
    } else if settings.choice_menu().is_some() {
        vec![
            Span::styled(
                "↑↓",
                semantic_style(state, SemanticColor::Focus)
                    .add_modifier(Modifier::BOLD)
                    .patch(surface),
            ),
            Span::styled(
                " Move  ",
                semantic_style(state, SemanticColor::Muted).patch(surface),
            ),
            Span::styled(
                "Enter",
                semantic_style(state, SemanticColor::Focus)
                    .add_modifier(Modifier::BOLD)
                    .patch(surface),
            ),
            Span::styled(
                " Select  ",
                semantic_style(state, SemanticColor::Muted).patch(surface),
            ),
            Span::styled(
                "Esc",
                semantic_style(state, SemanticColor::Focus)
                    .add_modifier(Modifier::BOLD)
                    .patch(surface),
            ),
            Span::styled(
                " Back",
                semantic_style(state, SemanticColor::Muted).patch(surface),
            ),
        ]
    } else if settings.focus() == SettingsFocus::Categories {
        let gap = if two_column { "   " } else { "  " };
        vec![
            Span::styled(
                "↑↓",
                semantic_style(state, SemanticColor::Focus)
                    .add_modifier(Modifier::BOLD)
                    .patch(surface),
            ),
            Span::styled(
                format!(" Move{gap}"),
                semantic_style(state, SemanticColor::Muted).patch(surface),
            ),
            Span::styled(
                "Enter",
                semantic_style(state, SemanticColor::Focus)
                    .add_modifier(Modifier::BOLD)
                    .patch(surface),
            ),
            Span::styled(
                format!(" Open{gap}"),
                semantic_style(state, SemanticColor::Muted).patch(surface),
            ),
            Span::styled(
                "Esc",
                semantic_style(state, SemanticColor::Focus)
                    .add_modifier(Modifier::BOLD)
                    .patch(surface),
            ),
            Span::styled(
                " Close",
                semantic_style(state, SemanticColor::Muted).patch(surface),
            ),
        ]
    } else if two_column {
        if settings.category() == SettingsCategory::Shortcuts {
            vec![
                Span::styled(
                    "↑↓",
                    semantic_style(state, SemanticColor::Focus)
                        .add_modifier(Modifier::BOLD)
                        .patch(surface),
                ),
                Span::styled(
                    " Move   ",
                    semantic_style(state, SemanticColor::Muted).patch(surface),
                ),
                Span::styled(
                    "Enter",
                    semantic_style(state, SemanticColor::Focus)
                        .add_modifier(Modifier::BOLD)
                        .patch(surface),
                ),
                Span::styled(
                    " Rebind   ",
                    semantic_style(state, SemanticColor::Muted).patch(surface),
                ),
                Span::styled(
                    "Tab",
                    semantic_style(state, SemanticColor::Focus)
                        .add_modifier(Modifier::BOLD)
                        .patch(surface),
                ),
                Span::styled(
                    " Categories   ",
                    semantic_style(state, SemanticColor::Muted).patch(surface),
                ),
                Span::styled(
                    "Esc",
                    semantic_style(state, SemanticColor::Focus)
                        .add_modifier(Modifier::BOLD)
                        .patch(surface),
                ),
                Span::styled(
                    " Back",
                    semantic_style(state, SemanticColor::Muted).patch(surface),
                ),
            ]
        } else {
            vec![
                Span::styled(
                    "↑↓",
                    semantic_style(state, SemanticColor::Focus)
                        .add_modifier(Modifier::BOLD)
                        .patch(surface),
                ),
                Span::styled(
                    " Move   ",
                    semantic_style(state, SemanticColor::Muted).patch(surface),
                ),
                Span::styled(
                    "←→",
                    semantic_style(state, SemanticColor::Focus)
                        .add_modifier(Modifier::BOLD)
                        .patch(surface),
                ),
                Span::styled(
                    " Change   ",
                    semantic_style(state, SemanticColor::Muted).patch(surface),
                ),
                Span::styled(
                    "Enter",
                    semantic_style(state, SemanticColor::Focus)
                        .add_modifier(Modifier::BOLD)
                        .patch(surface),
                ),
                Span::styled(
                    " Edit   ",
                    semantic_style(state, SemanticColor::Muted).patch(surface),
                ),
                Span::styled(
                    "Tab",
                    semantic_style(state, SemanticColor::Focus)
                        .add_modifier(Modifier::BOLD)
                        .patch(surface),
                ),
                Span::styled(
                    " Categories   ",
                    semantic_style(state, SemanticColor::Muted).patch(surface),
                ),
                Span::styled(
                    "Esc",
                    semantic_style(state, SemanticColor::Focus)
                        .add_modifier(Modifier::BOLD)
                        .patch(surface),
                ),
                Span::styled(
                    " Back",
                    semantic_style(state, SemanticColor::Muted).patch(surface),
                ),
            ]
        }
    } else {
        let enter_label = if settings.category() == SettingsCategory::Shortcuts {
            " Rebind  "
        } else {
            " Open  "
        };
        vec![
            Span::styled(
                "Enter",
                semantic_style(state, SemanticColor::Focus)
                    .add_modifier(Modifier::BOLD)
                    .patch(surface),
            ),
            Span::styled(
                enter_label,
                semantic_style(state, SemanticColor::Muted).patch(surface),
            ),
            Span::styled(
                "Esc",
                semantic_style(state, SemanticColor::Focus)
                    .add_modifier(Modifier::BOLD)
                    .patch(surface),
            ),
            Span::styled(
                " Back",
                semantic_style(state, SemanticColor::Muted).patch(surface),
            ),
        ]
    };

    if can_reset {
        hint_spans.push(Span::styled(if two_column { "   " } else { "  " }, surface));
        let mut chip_style =
            semantic_style(state, SemanticColor::Warning).add_modifier(Modifier::BOLD);
        if let Some(bg) = state.theme().bg_color(SemanticColor::ChipBackground) {
            chip_style = chip_style.bg(bg);
        }
        hint_spans.push(Span::styled("[r Reset]", chip_style));
    }

    lines.push(Line::from(hint_spans));
    frame.render_widget(Paragraph::new(lines).style(surface), area);
}

fn active_editor_for_item(state: &TuiState, item: SettingItem) -> Option<&super::input::TextInput> {
    state.settings_editor_for(item)
}

pub(crate) fn settings_pointer(
    state: &TuiState,
    settings: &SettingsOverlay,
    area: Rect,
    position: Position,
) -> Option<SettingsPointer> {
    let layout = settings_layout(area);
    if layout.modal.close_hit(position) {
        return Some(SettingsPointer::Close);
    }
    if settings.is_editing() || settings.is_capturing_shortcut() {
        return None;
    }

    if layout.two_column {
        if layout.categories_pane.contains(position) {
            let local_y = usize::from(position.y.saturating_sub(layout.categories_pane.y));
            if local_y >= 2 {
                return SettingsCategory::ALL
                    .get(local_y - 2)
                    .copied()
                    .map(SettingsPointer::Category);
            }
            return None;
        }
        if layout.items_pane.contains(position) {
            return items_pane_pointer(settings, layout.items_pane, true, position);
        }
    } else if let Some(menu) = settings.choice_menu() {
        let (back_bar, body) = split_compact_back_header(layout.items_pane);
        if back_bar.contains(position) && position.y == back_bar.y {
            return Some(SettingsPointer::Back);
        }
        if body.contains(position) {
            let choices = menu.choices();
            let list_height = usize::from(body.height).max(1);
            let selected = menu.selected().min(choices.len().saturating_sub(1));
            let scroll_offset = selected
                .saturating_sub(list_height.saturating_sub(1))
                .min(choices.len().saturating_sub(list_height));
            let index = scroll_offset + usize::from(position.y.saturating_sub(body.y));
            return (index < choices.len()).then_some(SettingsPointer::Choice(index));
        }
    } else if settings.focus() == SettingsFocus::Categories {
        if layout.categories_pane.contains(position) {
            let selected_idx = SettingsCategory::ALL
                .iter()
                .position(|&category| category == settings.category())
                .unwrap_or(0);
            let visible_rows = usize::from(layout.categories_pane.height).max(1);
            let scroll_offset = selected_idx
                .saturating_sub(visible_rows.saturating_sub(1))
                .min(SettingsCategory::ALL.len().saturating_sub(visible_rows));
            let index =
                scroll_offset + usize::from(position.y.saturating_sub(layout.categories_pane.y));
            return SettingsCategory::ALL
                .get(index)
                .copied()
                .map(SettingsPointer::Category);
        }
    } else {
        let (back_bar, body) = split_compact_back_header(layout.items_pane);
        if back_bar.contains(position) && position.y == back_bar.y {
            return Some(SettingsPointer::Back);
        }
        if body.contains(position) {
            return items_pane_pointer(settings, body, false, position);
        }
    }

    if layout.footer.contains(position)
        && settings.choice_menu().is_none()
        && settings.focus() == SettingsFocus::Items
        && settings
            .selected_item()
            .is_some_and(|item| state.is_setting_overridden(item))
    {
        let hints_row = layout
            .footer
            .y
            .saturating_add(if layout.footer.height >= 2 { 1 } else { 0 });
        if position.y == hints_row {
            let prefix = footer_hint_prefix(layout.two_column, settings);
            let chip_start = layout
                .footer
                .x
                .saturating_add(u16::try_from(UnicodeWidthStr::width(prefix)).unwrap_or(0));
            if position.x >= chip_start && position.x < chip_start.saturating_add(9) {
                return Some(SettingsPointer::ResetSelected);
            }
        }
    }

    None
}

fn items_pane_pointer(
    settings: &SettingsOverlay,
    area: Rect,
    two_column: bool,
    position: Position,
) -> Option<SettingsPointer> {
    let items = settings.category().items();
    if items.is_empty() {
        return None;
    }
    let row_height = item_row_height(two_column, area);
    let has_header = two_column && area.height >= 6;
    let header_lines = if has_header { 2usize } else { 0usize };
    let visible_slots = (usize::from(area.height).saturating_sub(header_lines) / row_height).max(1);
    let selected_index = settings.selected_index().min(items.len().saturating_sub(1));
    let scroll_offset = selected_index
        .saturating_sub(visible_slots.saturating_sub(1))
        .min(items.len().saturating_sub(visible_slots));
    let local_y = usize::from(position.y.saturating_sub(area.y));
    if local_y >= header_lines {
        let index = scroll_offset + ((local_y - header_lines) / row_height);
        return items.get(index).copied().map(SettingsPointer::Item);
    }
    None
}
