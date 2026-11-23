//! Pointer hit-testing for the two-column Provider Workbench and form overlays.

use super::{ProviderWorkbenchPointer, workbench_layout};
use crate::tui::{
    click_target::chip_hit,
    provider_overlay::{
        ProviderFormField, ProviderFormOverlay, ProviderListItem, ProviderManagerOverlay,
        ProviderWorkbenchFocus,
    },
    state::TuiState,
};
use ratatui::layout::{Position, Rect};
use unicode_width::UnicodeWidthStr;

pub(crate) fn provider_manager_pointer(
    area: Rect,
    manager: &ProviderManagerOverlay,
    position: Position,
) -> Option<ProviderWorkbenchPointer> {
    let layout = workbench_layout(area);
    if layout.modal.close_hit(position) {
        return Some(ProviderWorkbenchPointer::Close);
    }
    if layout.two_column {
        if layout.left_pane.contains(position) {
            return left_column_pointer(
                layout.left_pane,
                manager.items().len(),
                manager.selected(),
                position,
            );
        }
        if layout.right_pane.contains(position)
            && let Some(form) = manager.detail_form()
        {
            return form_detail_pointer(
                layout.right_pane,
                form,
                !manager.is_adding(),
                manager.current_alias() == manager.selected_item().map(ProviderListItem::alias),
                position,
            );
        }
        return None;
    }

    if manager.focus() == ProviderWorkbenchFocus::Detail
        && let Some(form) = manager.detail_form()
    {
        return form_detail_pointer(
            layout.right_pane,
            form,
            !manager.is_adding(),
            manager.current_alias() == manager.selected_item().map(ProviderListItem::alias),
            position,
        );
    }
    if !layout.left_pane.contains(position) {
        return None;
    }
    let visible_height = usize::from(layout.left_pane.height.saturating_sub(1)).max(1);
    let offset = manager
        .selected()
        .saturating_sub(visible_height.saturating_sub(1));
    let shown = manager
        .items()
        .len()
        .saturating_sub(offset)
        .min(visible_height);
    let local = usize::from(position.y.saturating_sub(layout.left_pane.y));
    if local == 0 || local > shown {
        return None;
    }
    Some(ProviderWorkbenchPointer::SelectProvider(offset + local - 1))
}

pub(crate) fn provider_form_pointer(
    area: Rect,
    state: &TuiState,
    form: &ProviderFormOverlay,
    position: Position,
) -> Option<ProviderWorkbenchPointer> {
    let layout = workbench_layout(area);
    if layout.modal.close_hit(position) {
        return Some(ProviderWorkbenchPointer::Close);
    }
    let items = state.provider_workbench_items();
    if layout.two_column && !items.is_empty() && layout.left_pane.contains(position) {
        let selected = form
            .editing_alias()
            .and_then(|alias| items.iter().position(|item| item.alias() == alias))
            .unwrap_or(0);
        return left_column_pointer(layout.left_pane, items.len(), selected, position);
    }
    if layout.right_pane.contains(position) {
        return form_detail_pointer(layout.right_pane, form, false, false, position);
    }
    None
}

fn left_column_pointer(
    pane: Rect,
    item_count: usize,
    selected: usize,
    position: Position,
) -> Option<ProviderWorkbenchPointer> {
    let local_y = usize::from(position.y.saturating_sub(pane.y));
    let add_row = usize::from(pane.height.saturating_sub(1));
    if local_y == add_row {
        return Some(ProviderWorkbenchPointer::AddProvider);
    }
    let list_capacity = usize::from(pane.height.saturating_sub(4)).max(1);
    let offset = selected.saturating_sub(list_capacity.saturating_sub(1));
    let shown = item_count.saturating_sub(offset).min(list_capacity);
    if local_y >= 2 && local_y < 2 + shown {
        return Some(ProviderWorkbenchPointer::SelectProvider(
            offset + local_y - 2,
        ));
    }
    None
}

fn form_detail_pointer(
    pane: Rect,
    form: &ProviderFormOverlay,
    show_workbench_chips: bool,
    is_active: bool,
    position: Position,
) -> Option<ProviderWorkbenchPointer> {
    let local_y = usize::from(position.y.saturating_sub(pane.y));
    let has_header_gap = pane.height >= 13;
    let first_field_row = if has_header_gap { 2 } else { 1 };
    let label_col_width: usize = if pane.width >= 48 { 16 } else { 14 };
    let value_width = usize::from(pane.width)
        .saturating_sub(label_col_width + 3)
        .max(1);

    let non_save_count = ProviderFormField::ALL.len() - 1;
    if local_y >= first_field_row && local_y < first_field_row + non_save_count {
        let field = ProviderFormField::ALL[local_y - first_field_row];
        if field == ProviderFormField::ApiKey && form.has_copyable_api_key() {
            let copy_label = " [Copy]";
            if value_width >= UnicodeWidthStr::width(copy_label) + 4 {
                let key_budget = value_width
                    .saturating_sub(UnicodeWidthStr::width(copy_label))
                    .max(1);
                let masked = form.masked_api_key();
                let visible_masked =
                    crate::tui::click_target::truncate_columns(&masked, key_budget);
                let text_width = UnicodeWidthStr::width(visible_masked.as_str());
                let copy_x = pane.x.saturating_add(
                    u16::try_from(2 + label_col_width + text_width + 1).unwrap_or(0),
                );
                if position.x >= copy_x && position.x < copy_x.saturating_add(6) {
                    return Some(ProviderWorkbenchPointer::CopyApiKey);
                }
            }
        }
        if field == ProviderFormField::Model {
            let browse_label = " [Browse ▸]";
            if value_width >= UnicodeWidthStr::width(browse_label) + 4 {
                let model_budget = value_width
                    .saturating_sub(UnicodeWidthStr::width(browse_label))
                    .max(1);
                let text_width = form
                    .field_viewport(field, model_budget)
                    .map(|vp| UnicodeWidthStr::width(vp.text.as_str()))
                    .unwrap_or(0);
                let browse_x = pane.x.saturating_add(
                    u16::try_from(2 + label_col_width + text_width + 1).unwrap_or(0),
                );
                if position.x >= browse_x && position.x < browse_x.saturating_add(10) {
                    return Some(ProviderWorkbenchPointer::BrowseModels);
                }
            }
        }
        return Some(ProviderWorkbenchPointer::Field(field));
    }

    let save_row = first_field_row + non_save_count + usize::from(pane.height >= 15);
    if local_y == save_row {
        let chips: Vec<&str> = if show_workbench_chips {
            vec![
                "Save provider",
                if is_active { "Active" } else { "Set active" },
                "Delete",
            ]
        } else {
            vec!["Save provider"]
        };
        match chip_hit(
            Position::new(pane.x.saturating_add(2), position.y),
            &chips,
            position,
        ) {
            Some(0) => return Some(ProviderWorkbenchPointer::Field(ProviderFormField::Save)),
            Some(1) => return Some(ProviderWorkbenchPointer::SetActive),
            Some(2) => return Some(ProviderWorkbenchPointer::DeleteSelected),
            _ => {}
        }
    }
    None
}
