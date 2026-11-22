//! Master-detail workbench frame, provider list column, compact list, and footer.

use super::{
    action_hint_line, protocol_display_label,
    workbench_detail::{render_empty_detail_pane, render_form_detail_pane},
    workbench_layout,
};
use crate::tui::{
    click_target::truncate_columns,
    modal_frame::{
        modal_selection_style, modal_surface_style, render_divider, render_modal_frame,
        render_vertical_divider,
    },
    overlay_render::semantic_style,
    provider_overlay::{
        ProviderFormField, ProviderFormOverlay, ProviderListItem, ProviderManagerOverlay,
        ProviderWorkbenchFocus,
    },
    state::TuiState,
    theme::SemanticColor,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

pub(in crate::tui) fn render_provider_manager(
    frame: &mut Frame<'_>,
    state: &TuiState,
    manager: &ProviderManagerOverlay,
) {
    let layout = workbench_layout(frame.area());
    render_modal_frame(
        frame,
        state,
        layout.modal,
        "Providers",
        SemanticColor::BorderFocus,
        true,
    );
    if layout.modal.inner.is_empty() {
        return;
    }

    if layout.two_column {
        render_provider_list_column(
            frame,
            state,
            manager.items(),
            manager.current_alias(),
            (!manager.is_adding()).then_some(manager.selected()),
            manager.is_adding(),
            manager.focus() == ProviderWorkbenchFocus::List,
            layout.left_pane,
        );
        render_vertical_divider(frame, state, layout.divider);
        if let Some(form) = manager.detail_form() {
            render_form_detail_pane(
                frame,
                state,
                form,
                manager.focus() == ProviderWorkbenchFocus::Detail,
                !manager.is_adding(),
                manager.current_alias() == manager.selected_item().map(ProviderListItem::alias),
                layout.right_pane,
            );
        } else {
            render_empty_detail_pane(frame, state, layout.right_pane);
        }
    } else if manager.focus() == ProviderWorkbenchFocus::Detail
        && let Some(form) = manager.detail_form()
    {
        render_form_detail_pane(
            frame,
            state,
            form,
            true,
            !manager.is_adding(),
            manager.current_alias() == manager.selected_item().map(ProviderListItem::alias),
            layout.right_pane,
        );
    } else {
        render_compact_manager_list(frame, state, manager, layout.left_pane);
    }

    render_divider(frame, state, layout.footer_divider);
    let notice = manager.notice().unwrap_or("");
    let actions: &[(&str, &str)] = if manager.focus() == ProviderWorkbenchFocus::Detail {
        &[
            ("Tab", "Next"),
            ("Left/Right", "Change"),
            ("Ctrl+S", "Save"),
            ("Esc", "List"),
        ]
    } else if manager.selected_item().is_some() {
        &[
            ("N", "Add"),
            ("Enter", "Switch"),
            ("M", "Models"),
            ("E", "Edit"),
            ("D", "Delete"),
            ("Tab", "Inline"),
            ("Esc", "Back"),
        ]
    } else {
        &[("N", "Add"), ("Esc", "Back")]
    };
    render_workbench_footer(frame, state, notice, actions, layout.footer);
}

pub(in crate::tui) fn render_provider_form(
    frame: &mut Frame<'_>,
    state: &TuiState,
    form: &ProviderFormOverlay,
) {
    let layout = workbench_layout(frame.area());
    render_modal_frame(
        frame,
        state,
        layout.modal,
        form.title(),
        SemanticColor::BorderFocus,
        true,
    );
    if layout.modal.inner.is_empty() {
        return;
    }

    let items = state.provider_workbench_items();
    if layout.two_column && !items.is_empty() {
        let selected_idx = form
            .editing_alias()
            .and_then(|alias| items.iter().position(|item| item.alias() == alias));
        render_provider_list_column(
            frame,
            state,
            items,
            state.current_provider_alias(),
            selected_idx,
            !form.is_editing(),
            false,
            layout.left_pane,
        );
        render_vertical_divider(frame, state, layout.divider);
        render_form_detail_pane(frame, state, form, true, false, false, layout.right_pane);
    } else {
        render_form_detail_pane(frame, state, form, true, false, false, layout.right_pane);
    }

    render_divider(frame, state, layout.footer_divider);
    let actions: &[(&str, &str)] = match form.selected_field() {
        ProviderFormField::Model => &[("Enter", "Models"), ("Ctrl+S", "Save"), ("Esc", "Back")],
        ProviderFormField::ReasoningEffort => &[
            ("Ctrl+Space", "Presets"),
            ("Ctrl+S", "Save"),
            ("Esc", "Back"),
        ],
        ProviderFormField::Save => &[("Enter", "Save"), ("Esc", "Back")],
        _ => &[
            ("Tab", "Next"),
            ("Left/Right", "Change"),
            ("Ctrl+S", "Save"),
            ("Esc", "Back"),
        ],
    };
    render_workbench_footer(
        frame,
        state,
        form.notice().unwrap_or(""),
        actions,
        layout.footer,
    );
}

#[allow(clippy::too_many_arguments)]
fn render_provider_list_column(
    frame: &mut Frame<'_>,
    state: &TuiState,
    items: &[ProviderListItem],
    current_alias: Option<&str>,
    selected_index: Option<usize>,
    adding_selected: bool,
    focused: bool,
    area: Rect,
) {
    if area.is_empty() {
        return;
    }
    let surface = modal_surface_style(state);
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(usize::from(area.height));
    let header_slot = if focused {
        SemanticColor::Focus
    } else {
        SemanticColor::Muted
    };
    lines.push(Line::from(Span::styled(
        "  PROVIDERS",
        semantic_style(state, header_slot)
            .add_modifier(Modifier::BOLD)
            .patch(surface),
    )));
    lines.push(Line::styled("", surface));

    let list_capacity = usize::from(area.height.saturating_sub(4)).max(1);
    let offset = selected_index
        .unwrap_or(0)
        .saturating_sub(list_capacity.saturating_sub(1));
    let width = usize::from(area.width);

    for (index, item) in items.iter().enumerate().skip(offset).take(list_capacity) {
        let selected = selected_index == Some(index);
        let active = current_alias == Some(item.alias());
        let base = modal_selection_style(state, selected);
        let bar = if selected { "▌ " } else { "  " };
        let bar_style = if selected {
            semantic_style(state, SemanticColor::Focus).patch(base)
        } else {
            base
        };
        let dot = if active { "● " } else { "  " };
        let dot_style = if active {
            semantic_style(state, SemanticColor::Success)
                .add_modifier(Modifier::BOLD)
                .patch(base)
        } else {
            base
        };
        let name_budget = width.saturating_sub(4).max(1);
        let name = truncate_columns(item.display_name(), name_budget);
        let pad = width.saturating_sub(4 + UnicodeWidthStr::width(name.as_str()));
        lines.push(Line::from(vec![
            Span::styled(bar, bar_style),
            Span::styled(dot, dot_style),
            Span::styled(name, base.add_modifier(Modifier::BOLD)),
            Span::styled(" ".repeat(pad), base),
        ]));
    }
    if items.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No providers",
            semantic_style(state, SemanticColor::Muted).patch(surface),
        )));
    }

    let add_row = usize::from(area.height.saturating_sub(1));
    while lines.len() + 2 <= add_row {
        lines.push(Line::styled("", surface));
    }
    if lines.len() < add_row {
        lines.push(Line::styled(
            "─".repeat(width),
            semantic_style(state, SemanticColor::Border).patch(surface),
        ));
    }
    let add_base = modal_selection_style(state, adding_selected);
    let add_label = truncate_columns("+ Add provider", width.saturating_sub(2));
    let add_pad = width.saturating_sub(2 + UnicodeWidthStr::width(add_label.as_str()));
    lines.push(Line::from(vec![
        Span::styled(
            if adding_selected { "▌ " } else { "  " },
            semantic_style(state, SemanticColor::Focus)
                .add_modifier(Modifier::BOLD)
                .patch(add_base),
        ),
        Span::styled(
            add_label,
            semantic_style(state, SemanticColor::ToolKeyword)
                .add_modifier(Modifier::BOLD)
                .patch(add_base),
        ),
        Span::styled(" ".repeat(add_pad), add_base),
    ]));

    frame.render_widget(Paragraph::new(lines).style(surface), area);
}

fn render_compact_manager_list(
    frame: &mut Frame<'_>,
    state: &TuiState,
    manager: &ProviderManagerOverlay,
    area: Rect,
) {
    if area.is_empty() {
        return;
    }
    let surface = modal_surface_style(state);
    let mut lines = vec![Line::from(vec![
        Span::styled("  ", surface),
        Span::styled(
            format!("{:<18}", "Provider"),
            semantic_style(state, SemanticColor::Focus)
                .add_modifier(Modifier::BOLD)
                .patch(surface),
        ),
        Span::styled(
            format!("{:<17}", "Protocol"),
            semantic_style(state, SemanticColor::Muted).patch(surface),
        ),
        Span::styled(
            "Model",
            semantic_style(state, SemanticColor::Muted).patch(surface),
        ),
    ])];
    let visible_height = usize::from(area.height.saturating_sub(1)).max(1);
    let offset = manager
        .selected()
        .saturating_sub(visible_height.saturating_sub(1));
    for (index, item) in manager
        .items()
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible_height)
    {
        let selected = index == manager.selected();
        let base = modal_selection_style(state, selected);
        let active = manager.current_alias() == Some(item.alias());
        let kind = protocol_display_label(item.kind(), item.protocol());
        let model = match (active, item.model()) {
            (true, Some(model)) => format!("● {model}"),
            (true, None) => "● active".to_owned(),
            (false, Some(model)) => model.to_owned(),
            (false, None) => String::new(),
        };
        lines.push(Line::from(vec![
            Span::styled(if selected { "▌ " } else { "  " }, base),
            Span::styled(
                format!("{:<18}", truncate_columns(item.display_name(), 17)),
                base.add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{kind:<17}"), base),
            Span::styled(
                model,
                semantic_style(state, SemanticColor::Success).patch(base),
            ),
        ]));
    }
    if manager.items().is_empty() {
        lines.push(Line::from(Span::styled(
            "  No providers",
            semantic_style(state, SemanticColor::Muted).patch(surface),
        )));
    }
    frame.render_widget(Paragraph::new(lines).style(surface), area);
}

fn render_workbench_footer(
    frame: &mut Frame<'_>,
    state: &TuiState,
    notice: &str,
    actions: &[(&str, &str)],
    area: Rect,
) {
    if area.is_empty() {
        return;
    }
    let surface = modal_surface_style(state);
    let mut lines = Vec::new();
    if area.height >= 2 {
        lines.push(Line::from(Span::styled(
            notice.to_owned(),
            semantic_style(state, SemanticColor::Muted).patch(surface),
        )));
    }
    lines.push(action_hint_line(state, actions, surface));
    frame.render_widget(Paragraph::new(lines).style(surface), area);
}
