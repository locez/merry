//! Model picker and reasoning effort picker ModalFrame renderers and hit-testers.

use super::{ModelPointer, PICKER_WIDTH, action_hint_line};
use crate::tui::{
    click_target::truncate_columns,
    modal_frame::{
        ModalRects, modal_rects, modal_selection_style, modal_surface_style, render_modal_frame,
    },
    overlay_render::semantic_style,
    provider_overlay::ModelPickerOverlay,
    reasoning_picker::ReasoningPickerOverlay,
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

pub(in crate::tui) fn render_model_picker(
    frame: &mut Frame<'_>,
    state: &TuiState,
    picker: &ModelPickerOverlay,
) {
    let (modal, rows) = model_picker_layout(frame.area());
    render_modal_frame(
        frame,
        state,
        modal,
        "Models",
        SemanticColor::BorderFocus,
        true,
    );
    if modal.inner.is_empty() {
        return;
    }
    let surface = modal_surface_style(state);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                picker.display_name().to_owned(),
                semantic_style(state, SemanticColor::Focus)
                    .add_modifier(Modifier::BOLD)
                    .patch(surface),
            ),
            Span::styled(
                format!("  {}", picker.alias()),
                semantic_style(state, SemanticColor::Muted).patch(surface),
            ),
        ]))
        .style(surface),
        rows[0],
    );
    let query = picker.query_viewport(rows[1].width.saturating_sub(2) as usize);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "❯ ",
                semantic_style(state, SemanticColor::Focus)
                    .add_modifier(Modifier::BOLD)
                    .patch(surface),
            ),
            Span::styled(
                if picker.query().is_empty() {
                    "Search models or type custom ID".to_owned()
                } else {
                    query.text.clone()
                },
                if picker.query().is_empty() {
                    semantic_style(state, SemanticColor::Muted).patch(surface)
                } else {
                    semantic_style(state, SemanticColor::Assistant).patch(surface)
                },
            ),
        ]))
        .style(surface),
        rows[1],
    );
    let visible = picker.visible_models();
    let manual = picker.manual_model();
    let list_height = usize::from(rows[2].height).max(1);
    let offset = picker
        .selected()
        .saturating_sub(list_height.saturating_sub(1));
    let id_col = usize::from(rows[2].width).saturating_sub(18).clamp(16, 44);
    let mut all_lines = visible
        .iter()
        .enumerate()
        .map(|(index, model)| {
            let selected = index == picker.selected();
            let base = modal_selection_style(state, selected);
            Line::from(vec![
                Span::styled(if selected { "▌ " } else { "  " }, base),
                Span::styled(
                    format!(
                        "{:<width$}",
                        truncate_columns(model.id(), id_col),
                        width = id_col
                    ),
                    base.add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    model.owner().unwrap_or("").to_owned(),
                    semantic_style(state, SemanticColor::ToolKeyword).patch(base),
                ),
            ])
        })
        .collect::<Vec<_>>();
    if let Some(manual) = manual {
        let manual_index = visible.len();
        let selected = picker.selected() == manual_index;
        let base = modal_selection_style(state, selected);
        all_lines.push(Line::from(vec![
            Span::styled(if selected { "▌ " } else { "  " }, base),
            Span::styled(
                format!("Use custom: {manual}"),
                semantic_style(state, SemanticColor::Focus)
                    .add_modifier(Modifier::BOLD)
                    .patch(base),
            ),
        ]));
    }
    let lines = all_lines
        .into_iter()
        .skip(offset)
        .take(list_height)
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines).style(surface), rows[2]);
    let status = if picker.is_loading() {
        "Refreshing models from provider API..."
    } else {
        picker.error().unwrap_or("")
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                status,
                if picker.error().is_some() {
                    semantic_style(state, SemanticColor::Error).patch(surface)
                } else {
                    semantic_style(state, SemanticColor::Muted).patch(surface)
                },
            )),
            action_hint_line(
                state,
                &[("Enter", "Use"), ("F5", "Refresh"), ("Esc", "Back")],
                surface,
            ),
        ])
        .style(surface),
        rows[3],
    );
    frame.set_cursor_position(Position::new(
        rows[1]
            .x
            .saturating_add(2)
            .saturating_add(query.cursor_column as u16)
            .min(rows[1].right().saturating_sub(1)),
        rows[1].y,
    ));
}

pub(in crate::tui) fn render_reasoning_picker(
    frame: &mut Frame<'_>,
    state: &TuiState,
    picker: &ReasoningPickerOverlay,
) {
    let (modal, rows) = reasoning_layout(frame.area());
    render_modal_frame(
        frame,
        state,
        modal,
        "Thinking mode",
        SemanticColor::BorderFocus,
        true,
    );
    if modal.inner.is_empty() {
        return;
    }
    let surface = modal_surface_style(state);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                picker.model().to_owned(),
                semantic_style(state, SemanticColor::Focus)
                    .add_modifier(Modifier::BOLD)
                    .patch(surface),
            ),
            Span::styled(
                format!("  {}", picker.alias()),
                semantic_style(state, SemanticColor::Muted).patch(surface),
            ),
        ]))
        .style(surface),
        rows[0],
    );

    let query = picker.query_viewport(rows[1].width.saturating_sub(2) as usize);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "❯ ",
                semantic_style(state, SemanticColor::Focus)
                    .add_modifier(Modifier::BOLD)
                    .patch(surface),
            ),
            Span::styled(
                if picker.query().is_empty() {
                    "Filter presets or type custom value".to_owned()
                } else {
                    query.text.clone()
                },
                if picker.query().is_empty() {
                    semantic_style(state, SemanticColor::Muted).patch(surface)
                } else {
                    semantic_style(state, SemanticColor::Assistant).patch(surface)
                },
            ),
        ]))
        .style(surface),
        rows[1],
    );

    let presets = picker.visible_presets();
    let custom = picker.custom_candidate();
    let list_height = usize::from(rows[2].height).max(1);
    let offset = picker
        .selected()
        .saturating_sub(list_height.saturating_sub(1));
    let mut all_lines = presets
        .iter()
        .enumerate()
        .map(|(index, label)| {
            let selected = index == picker.selected();
            let base = modal_selection_style(state, selected);
            Line::from(vec![
                Span::styled(if selected { "▌ " } else { "  " }, base),
                Span::styled((*label).to_owned(), base.add_modifier(Modifier::BOLD)),
            ])
        })
        .collect::<Vec<_>>();
    if let Some(custom) = custom {
        let custom_index = presets.len();
        let selected = picker.selected() == custom_index;
        let base = modal_selection_style(state, selected);
        all_lines.push(Line::from(vec![
            Span::styled(if selected { "▌ " } else { "  " }, base),
            Span::styled(
                format!("Use custom: {custom}"),
                semantic_style(state, SemanticColor::Focus)
                    .add_modifier(Modifier::BOLD)
                    .patch(base),
            ),
        ]));
    }
    let lines = all_lines
        .into_iter()
        .skip(offset)
        .take(list_height)
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines).style(surface), rows[2]);

    let status = picker
        .error()
        .unwrap_or("Pick a preset or type any provider-supported reasoning effort");
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                status,
                if picker.error().is_some() {
                    semantic_style(state, SemanticColor::Error).patch(surface)
                } else {
                    semantic_style(state, SemanticColor::Muted).patch(surface)
                },
            )),
            action_hint_line(state, &[("Enter", "Use"), ("Esc", "Back")], surface),
        ])
        .style(surface),
        rows[3],
    );
    frame.set_cursor_position(Position::new(
        rows[1]
            .x
            .saturating_add(2)
            .saturating_add(query.cursor_column as u16)
            .min(rows[1].right().saturating_sub(1)),
        rows[1].y,
    ));
}

pub(crate) fn model_picker_pointer(
    area: Rect,
    picker: &ModelPickerOverlay,
    position: Position,
) -> Option<ModelPointer> {
    let (modal, rows) = model_picker_layout(area);
    if modal.close_hit(position) {
        return Some(ModelPointer::Close);
    }
    if !rows[2].contains(position) {
        return None;
    }
    let visible = picker.visible_models();
    let list_height = usize::from(rows[2].height).max(1);
    let offset = picker
        .selected()
        .saturating_sub(list_height.saturating_sub(1));
    let local = usize::from(position.y.saturating_sub(rows[2].y));
    if local >= list_height {
        return None;
    }
    let index = offset + local;
    if index < visible.len() {
        return Some(ModelPointer::Row(index));
    }
    if index == visible.len() && picker.manual_model().is_some() {
        return Some(ModelPointer::Manual);
    }
    None
}

pub(crate) fn reasoning_pointer(
    area: Rect,
    picker: &ReasoningPickerOverlay,
    position: Position,
) -> Option<Result<usize, ()>> {
    let (modal, rows) = reasoning_layout(area);
    if modal.close_hit(position) {
        return Some(Err(()));
    }
    if !rows[2].contains(position) {
        return None;
    }
    let list_height = usize::from(rows[2].height).max(1);
    let offset = picker
        .selected()
        .saturating_sub(list_height.saturating_sub(1));
    let local = usize::from(position.y.saturating_sub(rows[2].y));
    if local >= list_height {
        return None;
    }
    let index = offset + local;
    (index < picker.option_count()).then_some(Ok(index))
}

fn model_picker_layout(area: Rect) -> (ModalRects, std::rc::Rc<[Rect]>) {
    let modal = modal_rects(area, PICKER_WIDTH, 22);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(modal.inner);
    (modal, rows)
}

fn reasoning_layout(area: Rect) -> (ModalRects, std::rc::Rc<[Rect]>) {
    let modal = modal_rects(area, PICKER_WIDTH, 20);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(modal.inner);
    (modal, rows)
}
