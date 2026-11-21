//! Interactive pending-input queue panel rendered directly above the composer.
//!
//! Hidden completely (`height == 0`) when there are no pending inputs. When
//! non-empty, each row supports click-to-edit (which pauses runtime auto-send
//! until editing finishes) and `[×]` removal, plus a `[Send]` chip whenever
//! auto-dispatch is paused.

use crate::tui::{
    render::composer::{set_input_cursor, styled_input_line},
    state::TuiState,
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
use std::borrow::Cow;
use unicode_width::UnicodeWidthStr;

pub(crate) const MAX_PENDING_VISIBLE_ITEMS: usize = 4;

const SEND_CHIP_TEXT: &str = "[Send]";
const EDIT_CHIP_TEXT: &str = "[edit]";
const SAVE_CHIP_TEXT: &str = "[Save]";
const REMOVE_CHIP_TEXT: &str = "[×]";

/// Clickable controls inside the pending-input queue strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingQueuePointer {
    Edit(u64),
    Remove(u64),
    SaveEdit,
    SendAll,
}

/// Desired row height for the pending queue (`0` when empty).
pub(crate) fn desired_pending_queue_height(state: &TuiState) -> u16 {
    let count = state.queue_preview().items.len();
    if count == 0 {
        return 0;
    }
    let visible = count.min(MAX_PENDING_VISIBLE_ITEMS);
    let overflow = usize::from(count > MAX_PENDING_VISIBLE_ITEMS);
    u16::try_from(1 + visible + overflow).unwrap_or(6)
}

/// Renders the pending queue above the composer and places the terminal cursor
/// inside the active inline editor when one is open.
pub(crate) fn render_pending_queue(frame: &mut Frame<'_>, state: &TuiState, region: Rect) {
    if region.width == 0 || region.height == 0 || !state.has_queue_preview_items() {
        return;
    }

    let surface_bg = state.theme().bg_color(SemanticColor::SurfaceSubtle);
    let surface_style = surface_bg.map_or_else(Style::default, |bg| Style::default().bg(bg));
    frame.render_widget(Block::default().style(surface_style), region);

    render_header_row(
        frame,
        state,
        Rect::new(region.x, region.y, region.width, 1),
        surface_style,
    );

    let items = &state.queue_preview().items;
    let shown_count = visible_item_count(items.len(), region.height);

    for (index, item) in items.iter().take(shown_count).enumerate() {
        let row_y = region.y.saturating_add(1 + index as u16);
        if row_y >= region.bottom() {
            break;
        }
        let row_rect = Rect::new(region.x, row_y, region.width, 1);
        render_item_row(
            frame,
            state,
            row_rect,
            index + 1,
            item.id,
            &item.text,
            surface_style,
        );
    }

    let hidden = items.len().saturating_sub(shown_count);
    if hidden > 0 {
        let overflow_y = region.y.saturating_add(1 + shown_count as u16);
        if overflow_y < region.bottom() {
            let pad = left_pad_str(region.width);
            let text = format!("{pad}… +{hidden} more pending");
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    truncate_chars(&text, usize::from(region.width)),
                    semantic_style(state, SemanticColor::Muted)
                        .add_modifier(Modifier::DIM)
                        .patch(surface_style),
                )))
                .style(surface_style),
                Rect::new(region.x, overflow_y, region.width, 1),
            );
        }
    }
}

/// Maps a mouse click inside `region` to the corresponding pending-queue action.
pub(crate) fn pending_queue_pointer_at(
    state: &TuiState,
    region: Rect,
    position: Position,
) -> Option<PendingQueuePointer> {
    if !region.contains(position) || !state.has_queue_preview_items() {
        return None;
    }

    if position.y == region.y {
        if let Some(send_rect) = send_chip_rect(state, region)
            && send_rect.contains(position)
        {
            return Some(PendingQueuePointer::SendAll);
        }
        return None;
    }

    let row_index = usize::from(position.y.saturating_sub(region.y.saturating_add(1)));
    let items = &state.queue_preview().items;
    let shown_count = visible_item_count(items.len(), region.height);
    let item = items.get(row_index).filter(|_| row_index < shown_count)?;

    let row_rect = Rect::new(region.x, position.y, region.width, 1);
    let (primary_chip, remove_chip) = row_action_rects(row_rect);

    if let Some(remove) = remove_chip
        && remove.contains(position)
    {
        return Some(PendingQueuePointer::Remove(item.id));
    }

    if state.editing_pending_id() == Some(item.id) {
        if let Some(save) = primary_chip
            && save.contains(position)
        {
            return Some(PendingQueuePointer::SaveEdit);
        }
        return None;
    }

    Some(PendingQueuePointer::Edit(item.id))
}

fn render_header_row(frame: &mut Frame<'_>, state: &TuiState, row: Rect, surface_style: Style) {
    let queue = state.queue_preview();
    let count = queue.items.len();
    let pad = left_pad_str(row.width);
    let editing_position = state.editing_pending_id().and_then(|id| {
        queue
            .items
            .iter()
            .position(|item| item.id == id)
            .map(|idx| idx + 1)
    });

    let title = format!("{pad}Pending ({count})");
    let title_style = semantic_style(state, SemanticColor::Focus)
        .add_modifier(Modifier::BOLD)
        .patch(surface_style);

    let (status_text, status_color): (Cow<'static, str>, SemanticColor) =
        if let Some(pos) = editing_position {
            (
                Cow::Owned(format!(" · editing #{pos} (paused)")),
                SemanticColor::Warning,
            )
        } else if queue.paused {
            (Cow::Borrowed(" · paused"), SemanticColor::Warning)
        } else {
            (
                Cow::Borrowed(" · sends together when ready"),
                SemanticColor::Muted,
            )
        };
    let status_style = semantic_style(state, status_color).patch(surface_style);

    let right_pad = right_pad_cols(row.width);
    let send_rect = send_chip_rect(state, row);

    let right_hint: Option<&'static str> = if send_rect.is_some() {
        None
    } else if editing_position.is_some() {
        Some("Enter save · Esc cancel")
    } else {
        Some("click to edit")
    };

    let right_reserved = if let Some(rect) = send_rect {
        usize::from(rect.width)
            .saturating_add(right_pad)
            .saturating_add(1)
    } else if let Some(hint) = right_hint {
        UnicodeWidthStr::width(hint)
            .saturating_add(right_pad)
            .saturating_add(1)
    } else {
        right_pad
    };

    let total_width = usize::from(row.width);
    let left_limit = total_width.saturating_sub(right_reserved);
    let title_w = UnicodeWidthStr::width(title.as_str());
    let mut spans = Vec::new();
    let mut used = 0usize;

    let shown_title = truncate_chars(&title, total_width);
    used += UnicodeWidthStr::width(shown_title.as_str());
    spans.push(Span::styled(shown_title, title_style));

    if left_limit > title_w {
        let shown_status = truncate_chars(&status_text, left_limit.saturating_sub(used));
        used += UnicodeWidthStr::width(shown_status.as_str());
        spans.push(Span::styled(shown_status, status_style));
    }

    if let Some(rect) = send_rect {
        let chip_offset = usize::from(rect.x.saturating_sub(row.x));
        if chip_offset > used {
            spans.push(Span::styled(" ".repeat(chip_offset - used), surface_style));
        }
        let mut chip_style = semantic_style(state, SemanticColor::Focus)
            .add_modifier(Modifier::BOLD)
            .patch(surface_style);
        if let Some(bg) = state.theme().bg_color(SemanticColor::ChipBackground) {
            chip_style = chip_style.bg(bg);
        }
        spans.push(Span::styled(SEND_CHIP_TEXT, chip_style));
    } else if let Some(hint) = right_hint {
        let hint_w = UnicodeWidthStr::width(hint);
        if total_width >= used + 1 + hint_w + right_pad {
            let gap = total_width - used - hint_w - right_pad;
            spans.push(Span::styled(" ".repeat(gap), surface_style));
            spans.push(Span::styled(
                hint,
                semantic_style(state, SemanticColor::Muted)
                    .add_modifier(Modifier::DIM)
                    .patch(surface_style),
            ));
        }
    }

    frame.render_widget(Paragraph::new(Line::from(spans)).style(surface_style), row);
}

fn render_item_row(
    frame: &mut Frame<'_>,
    state: &TuiState,
    row: Rect,
    display_index: usize,
    item_id: u64,
    item_text: &str,
    surface_style: Style,
) {
    let is_editing = state.editing_pending_id() == Some(item_id);
    let pad = left_pad_str(row.width);
    let (primary_chip, remove_chip) = row_action_rects(row);
    let right_edge = primary_chip
        .or(remove_chip)
        .map_or(row.right(), |rect| rect.x.saturating_sub(1));

    let row_bg = if is_editing {
        state
            .theme()
            .bg_color(SemanticColor::SelectionBackground)
            .or_else(|| state.theme().bg_color(SemanticColor::SurfaceSubtle))
    } else {
        state.theme().bg_color(SemanticColor::SurfaceSubtle)
    };
    let row_style = row_bg.map_or(surface_style, |bg| Style::default().bg(bg));

    let prefix = if is_editing {
        format!("{pad}{display_index}. ❯ ")
    } else {
        format!("{pad}{display_index}. ")
    };
    let prefix_w = UnicodeWidthStr::width(prefix.as_str());
    let content_x = row
        .x
        .saturating_add(u16::try_from(prefix_w).unwrap_or(row.width));
    let content_width = usize::from(right_edge.saturating_sub(content_x));

    let prefix_style = semantic_style(
        state,
        if is_editing {
            SemanticColor::Focus
        } else {
            SemanticColor::Muted
        },
    )
    .add_modifier(Modifier::BOLD)
    .patch(row_style);

    if is_editing && let Some(editor) = state.pending_editor() {
        frame.render_widget(Block::default().style(row_style), row);
        let prefix_rect = Rect::new(
            row.x,
            row.y,
            u16::try_from(prefix_w).unwrap_or(row.width).min(row.width),
            1,
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(prefix, prefix_style))).style(row_style),
            prefix_rect,
        );
        if content_width > 0 {
            let edit_rect = Rect::new(
                content_x,
                row.y,
                u16::try_from(content_width).unwrap_or(1),
                1,
            );
            let viewport = editor.input.viewport_rows(content_width, 1);
            let line = styled_input_line(
                state,
                &viewport.text,
                &viewport.image_placeholders,
                viewport.row_selections.first().and_then(|sel| sel.as_ref()),
                row_style,
            );
            frame.render_widget(Paragraph::new(line).style(row_style), edit_rect);
            set_input_cursor(frame, edit_rect, viewport.cursor_column, 0);
        }
        render_row_chips(frame, state, primary_chip, remove_chip, true, row_style);
        return;
    }

    let collapsed = item_text.replace('\n', " ↵ ");
    let shown_text = truncate_chars(&collapsed, content_width);
    let text_style = semantic_style(state, SemanticColor::TextSecondary).patch(row_style);

    let line = Line::from(vec![
        Span::styled(prefix, prefix_style),
        Span::styled(shown_text, text_style),
    ]);
    frame.render_widget(Paragraph::new(line).style(row_style), row);
    render_row_chips(frame, state, primary_chip, remove_chip, false, row_style);
}

fn render_row_chips(
    frame: &mut Frame<'_>,
    state: &TuiState,
    primary_chip: Option<Rect>,
    remove_chip: Option<Rect>,
    is_editing: bool,
    row_style: Style,
) {
    if let Some(rect) = primary_chip {
        let (label, color) = if is_editing {
            (SAVE_CHIP_TEXT, SemanticColor::Focus)
        } else {
            (EDIT_CHIP_TEXT, SemanticColor::Muted)
        };
        let mut style = semantic_style(state, color).patch(row_style);
        if is_editing {
            style = style.add_modifier(Modifier::BOLD);
        }
        frame.render_widget(Paragraph::new(Span::styled(label, style)), rect);
    }
    if let Some(rect) = remove_chip {
        let style = semantic_style(state, SemanticColor::Muted).patch(row_style);
        frame.render_widget(Paragraph::new(Span::styled(REMOVE_CHIP_TEXT, style)), rect);
    }
}

fn send_chip_rect(state: &TuiState, region: Rect) -> Option<Rect> {
    if state.is_editing_pending() || !state.queue_preview().paused {
        return None;
    }
    let chip_w = u16::try_from(UnicodeWidthStr::width(SEND_CHIP_TEXT)).ok()?;
    let right_pad = u16::try_from(right_pad_cols(region.width)).ok()?;
    if region.width < chip_w.saturating_add(right_pad).saturating_add(14) {
        return None;
    }
    let x = region
        .right()
        .saturating_sub(right_pad)
        .saturating_sub(chip_w);
    Some(Rect::new(x, region.y, chip_w, 1))
}

fn row_action_rects(row: Rect) -> (Option<Rect>, Option<Rect>) {
    let right_pad = u16::try_from(right_pad_cols(row.width)).unwrap_or(0);
    let remove_w = u16::try_from(UnicodeWidthStr::width(REMOVE_CHIP_TEXT)).unwrap_or(3);
    let primary_w = u16::try_from(UnicodeWidthStr::width(EDIT_CHIP_TEXT)).unwrap_or(6);
    if row.width < primary_w + 1 + remove_w + right_pad + 12 {
        return (None, None);
    }
    let remove_x = row
        .right()
        .saturating_sub(right_pad)
        .saturating_sub(remove_w);
    let primary_x = remove_x.saturating_sub(1).saturating_sub(primary_w);
    (
        Some(Rect::new(primary_x, row.y, primary_w, 1)),
        Some(Rect::new(remove_x, row.y, remove_w, 1)),
    )
}

fn visible_item_count(total: usize, region_height: u16) -> usize {
    let available_rows = usize::from(region_height.saturating_sub(1));
    let max_item_rows = available_rows.min(MAX_PENDING_VISIBLE_ITEMS);
    let has_dedicated_overflow_row = total > max_item_rows && available_rows > max_item_rows;
    if total > max_item_rows && !has_dedicated_overflow_row && max_item_rows > 1 {
        max_item_rows - 1
    } else {
        total.min(max_item_rows)
    }
}

fn left_pad_str(width: u16) -> &'static str {
    if width >= 40 { "  " } else { "" }
}

fn right_pad_cols(width: u16) -> usize {
    if width >= 40 { 1 } else { 0 }
}
