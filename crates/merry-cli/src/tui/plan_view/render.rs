//! Fullscreen framed Plan Review & History overlay renderer and pointer
//! hit-tester (sharing `SubagentTakeoverOverlay` chrome + `render_timeline_pane`
//! for full Markdown rendering and `Alt+C` inline comment highlights).

use super::{PlanViewOverlay, PlanViewPointer, plan_status_glyph, plan_status_label};
use crate::tui::{
    click_target::truncate_columns,
    modal_frame::{
        ModalRects, modal_selection_style, modal_surface_style, render_divider,
        render_vertical_divider,
    },
    overlay_render::semantic_style,
    render::{TimelineMouseDown, render_timeline_pane, timeline_mouse_down},
    state::TuiState,
    theme::SemanticColor,
};
use merry_core::{PlanDocument, PlanDocumentStatus};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Position, Rect, Size},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

const PLAN_COMPOSER_HEIGHT: u16 = 3;
const COMPACT_FOOTER_WIDTH: u16 = 84;
const TIGHT_CHIP_GAP_WIDTH: u16 = 72;

#[derive(Debug, Clone)]
struct FooterChipItem {
    label: String,
    color: SemanticColor,
    pointer: PlanViewPointer,
    /// Horizontal offset relative to the footer left edge, in terminal columns.
    x_offset: u16,
    /// Rendered width including surrounding `[` and `]`, in terminal columns.
    width: u16,
}

#[derive(Debug, Clone, Copy)]
struct PlanViewLayout {
    modal: ModalRects,
    rail: Option<Rect>,
    divider: Option<Rect>,
    timeline: Rect,
    composer: Rect,
    footer_divider: Rect,
    footer: Rect,
}

fn can_review_plan(state: &TuiState, view: &PlanViewOverlay) -> bool {
    state
        .plan()
        .plan_by_id(view.selected_plan_id())
        .is_some_and(|doc| {
            matches!(
                doc.summary.status,
                PlanDocumentStatus::AwaitingApproval | PlanDocumentStatus::Draft
            )
        })
}

fn plan_view_layout(
    area: Rect,
    plan_count: usize,
    view: Option<&PlanViewOverlay>,
    can_review: bool,
) -> PlanViewLayout {
    let margin_x = u16::from(area.width >= 48);
    let region = Rect::new(
        area.x.saturating_add(margin_x),
        area.y,
        area.width.saturating_sub(margin_x.saturating_mul(2)).max(1),
        area.height.max(1),
    );
    let h_pad: u16 = if region.width >= 60 { 2 } else { 1 };
    let inner = Rect::new(
        region.x.saturating_add(h_pad),
        region.y.saturating_add(1),
        region.width.saturating_sub(h_pad.saturating_mul(2)),
        region.height.saturating_sub(2),
    );
    let close_button = (region.width >= 16 && region.height >= 3)
        .then(|| Rect::new(region.right().saturating_sub(5), region.y, 3, 1));
    let modal = ModalRects {
        region,
        inner,
        close_button,
    };

    if inner.is_empty() {
        return PlanViewLayout {
            modal,
            rail: None,
            divider: None,
            timeline: Rect::default(),
            composer: Rect::default(),
            footer_divider: Rect::default(),
            footer: Rect::default(),
        };
    }

    let max_footer_rows = inner.height.saturating_sub(4).clamp(1, 4);
    let footer_rows = view.map_or(1, |overlay| {
        let rows = layout_footer_rows(overlay, can_review, inner.width);
        u16::try_from(rows.len())
            .unwrap_or(max_footer_rows)
            .clamp(1, max_footer_rows)
    });
    let has_footer_divider = inner.height >= footer_rows.saturating_add(5);
    let v_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(u16::from(has_footer_divider)),
            Constraint::Length(footer_rows),
        ])
        .split(inner);

    let main = v_chunks[0];
    let (rail, divider, content) = if plan_count > 1 && main.width >= 76 {
        let rail_w = (main.width / 4).clamp(20, 28);
        let h_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(rail_w),
                Constraint::Length(3),
                Constraint::Min(1),
            ])
            .split(main);
        (
            Some(h_chunks[0]),
            Some(Rect::new(
                h_chunks[1].x + 1,
                h_chunks[1].y,
                1,
                h_chunks[1].height,
            )),
            h_chunks[2],
        )
    } else {
        (None, None, main)
    };

    let composer_h = if content.height >= 6 {
        PLAN_COMPOSER_HEIGHT
    } else {
        0
    };
    let right_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(composer_h)])
        .split(content);

    PlanViewLayout {
        modal,
        rail,
        divider,
        timeline: right_chunks[0],
        composer: right_chunks[1],
        footer_divider: v_chunks[1],
        footer: v_chunks[2],
    }
}

pub(crate) fn plan_view_timeline_rect(state: &TuiState, area: Rect) -> Rect {
    let view = match state.overlay() {
        Some(crate::tui::overlay::Overlay::PlanView(view)) => Some(view),
        _ => None,
    };
    let can_review = view.is_some_and(|v| can_review_plan(state, v));
    plan_view_layout(area, state.plan().plans().len(), view, can_review).timeline
}

pub(crate) fn render_plan_view(frame: &mut Frame<'_>, state: &TuiState, view: &PlanViewOverlay) {
    let plans = state.plan().plans();
    let can_review = can_review_plan(state, view);
    let layout = plan_view_layout(frame.area(), plans.len(), Some(view), can_review);
    let selected_index = plans
        .iter()
        .position(|doc| &doc.summary.id == view.selected_plan_id())
        .unwrap_or(0);
    let current = plans.get(selected_index);
    let border_slot = current.map_or(SemanticColor::BorderFocus, |doc| {
        status_color(doc.summary.status)
    });
    let surface = modal_surface_style(state);

    frame.render_widget(Clear, layout.modal.region);

    let mut right_spans = vec![Span::styled(" ", surface)];
    if let Some(doc) = current {
        right_spans.push(Span::styled(
            format!("[{}]", plan_status_label(doc.summary.status)),
            semantic_style(state, status_color(doc.summary.status))
                .add_modifier(Modifier::BOLD)
                .patch(surface),
        ));
        if plans.len() > 1 {
            right_spans.push(Span::styled(
                format!("  {}/{}", selected_index + 1, plans.len()),
                semantic_style(state, SemanticColor::Muted).patch(surface),
            ));
        }
        right_spans.push(Span::styled(
            if layout.modal.region.width >= 56 {
                "  "
            } else {
                " "
            },
            surface,
        ));
    }
    right_spans.push(Span::styled(
        "[×]",
        semantic_style(state, SemanticColor::Muted)
            .add_modifier(Modifier::BOLD)
            .patch(surface),
    ));
    right_spans.push(Span::styled(" ", surface));

    let right_width: usize = right_spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum();
    let badge_width = UnicodeWidthStr::width(crate::tui::glyphs::BRAND_PAW_BADGE);
    let max_title_width = usize::from(layout.modal.region.width)
        .saturating_sub(right_width + badge_width + 4)
        .max(6);
    let raw_title = match current {
        Some(doc) if layout.modal.region.width >= 68 => format!(
            "{} Plan · {}  ({})  v{}",
            plan_status_glyph(doc.summary.status),
            doc.summary.title,
            doc.summary.file_name,
            doc.summary.revision
        ),
        Some(doc) => format!(
            "{} {}",
            plan_status_glyph(doc.summary.status),
            doc.summary.title
        ),
        None => "Plans".to_owned(),
    };
    let title_text = truncate_columns(&raw_title, max_title_width);

    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(
                crate::tui::glyphs::BRAND_PAW_BADGE,
                semantic_style(state, SemanticColor::Status)
                    .add_modifier(Modifier::BOLD)
                    .patch(surface),
            ),
            Span::styled(
                format!("{title_text} "),
                semantic_style(state, SemanticColor::Heading)
                    .add_modifier(Modifier::BOLD)
                    .patch(surface),
            ),
        ]))
        .title(Line::from(right_spans).alignment(Alignment::Right))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(semantic_style(state, border_slot).add_modifier(Modifier::BOLD))
        .style(surface);
    frame.render_widget(block, layout.modal.region);

    if layout.modal.inner.is_empty() {
        return;
    }

    if let Some(rail) = layout.rail {
        render_plan_rail(frame, state, view, plans, selected_index, rail);
    }
    if let Some(divider) = layout.divider {
        render_vertical_divider(frame, state, divider);
    }

    if !layout.timeline.is_empty() {
        if current.is_none() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "No plans have been written in this session yet.",
                    semantic_style(state, SemanticColor::Muted).patch(surface),
                )))
                .style(surface),
                layout.timeline,
            );
        } else {
            render_timeline_pane(frame, view.child_state(), layout.timeline);
        }
    }

    if !layout.composer.is_empty() {
        render_plan_composer(frame, state, view, can_review, layout.composer);
    }

    render_divider(frame, state, layout.footer_divider);
    render_plan_footer(frame, state, view, can_review, layout.footer);
}

fn render_plan_rail(
    frame: &mut Frame<'_>,
    state: &TuiState,
    view: &PlanViewOverlay,
    plans: &[PlanDocument],
    selected_index: usize,
    area: Rect,
) {
    let surface = modal_surface_style(state);
    let visible_rows = usize::from(area.height.saturating_sub(1));
    let scroll = view.effective_rail_scroll(selected_index, plans.len(), visible_rows);
    let mut header_spans = vec![Span::styled(
        "PLANS",
        semantic_style(state, SemanticColor::Focus)
            .add_modifier(Modifier::BOLD)
            .patch(surface),
    )];
    if visible_rows > 0 && plans.len() > visible_rows {
        let end = (scroll + visible_rows).min(plans.len());
        header_spans.push(Span::styled(
            format!(" {}-{}/{}", scroll + 1, end, plans.len()),
            semantic_style(state, SemanticColor::Muted).patch(surface),
        ));
    }
    let mut lines = vec![Line::from(header_spans)];
    let width = usize::from(area.width);
    for (index, doc) in plans.iter().enumerate().skip(scroll).take(visible_rows) {
        let selected = index == selected_index;
        let base = modal_selection_style(state, selected);
        let glyph = plan_status_glyph(doc.summary.status);
        let label = truncate_columns(&doc.summary.title, width.saturating_sub(5).max(1));
        let pad = width.saturating_sub(4 + UnicodeWidthStr::width(label.as_str()));
        let row_spans = vec![
            Span::styled(if selected { "▌ " } else { "  " }, base),
            Span::styled(
                format!("{glyph} "),
                semantic_style(state, status_color(doc.summary.status))
                    .add_modifier(Modifier::BOLD)
                    .patch(base),
            ),
            Span::styled(label, base.add_modifier(Modifier::BOLD)),
            Span::styled(" ".repeat(pad), base),
        ];
        lines.push(Line::from(row_spans));
    }
    frame.render_widget(Paragraph::new(lines).style(surface), area);
}

fn render_plan_composer(
    frame: &mut Frame<'_>,
    parent: &TuiState,
    view: &PlanViewOverlay,
    can_review: bool,
    area: Rect,
) {
    if area.height < 3 || area.width < 6 {
        return;
    }
    let child = view.child_state();
    let border_style = semantic_style(parent, SemanticColor::Border);
    let focus_style = semantic_style(
        parent,
        if child.pending_comment_quote().is_some() {
            SemanticColor::Warning
        } else {
            SemanticColor::Focus
        },
    )
    .add_modifier(Modifier::BOLD);
    let surface_bg = parent.theme().bg_color(SemanticColor::SurfaceSubtle);
    let band_style = surface_bg.map_or_else(Style::default, |bg| Style::default().bg(bg));

    let raw_label = if let Some(hint) = view.hint() {
        hint.to_owned()
    } else if child.is_commenting() {
        child
            .composer_title(area.width.saturating_sub(6))
            .trim()
            .to_owned()
    } else if can_review {
        if area.width >= 62 {
            "Feedback / approval note (Alt+C to annotate selected text)".to_owned()
        } else {
            "Comment / feedback".to_owned()
        }
    } else {
        "Plan archive (read-only)".to_owned()
    };
    let label = truncate_columns(&raw_label, usize::from(area.width).saturating_sub(5).max(8));
    let prefix = format!("── {label} ");
    let prefix_w = UnicodeWidthStr::width(prefix.as_str());
    let tail_len = usize::from(area.width).saturating_sub(prefix_w);
    let top_line = Line::from(vec![
        Span::styled("── ", border_style),
        Span::styled(label, focus_style),
        Span::styled(" ", border_style),
        Span::styled("─".repeat(tail_len), border_style),
    ]);
    frame.render_widget(
        Paragraph::new(top_line),
        Rect::new(area.x, area.y, area.width, 1),
    );

    let input_row = Rect::new(area.x, area.y.saturating_add(1), area.width, 1);
    frame.render_widget(Block::default().style(band_style), input_row);
    let prompt_x = input_row.x.saturating_add(1);
    if prompt_x < input_row.right() {
        let prompt_cell = &mut frame.buffer_mut()[(prompt_x, input_row.y)];
        prompt_cell.set_symbol("❯");
        prompt_cell.set_style(focus_style.patch(band_style));
    }

    let text_x = input_row.x.saturating_add(3);
    let text_w = input_row.width.saturating_sub(4).max(1);
    let text_area = Rect::new(text_x, input_row.y, text_w, 1);
    if child.plain_input_text().is_none_or(str::is_empty) {
        let placeholder = if child.pending_comment_quote().is_some() {
            "Type your comment for the highlighted quote and press Enter…"
        } else if child.comment_count() > 0 {
            "Add an optional summary note, then press Enter (request changes) or Ctrl+A (approve)…"
        } else if child.is_commenting() {
            "Drag-select text above to add inline comments, or type revision feedback…"
        } else {
            "Type revision feedback or press Alt+C to comment on selected text…"
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_columns(placeholder, usize::from(text_w)),
                semantic_style(parent, SemanticColor::Muted)
                    .add_modifier(Modifier::DIM)
                    .patch(band_style),
            ))),
            text_area,
        );
        frame.set_cursor_position(Position::new(text_area.x, text_area.y));
    } else {
        let vp = child.input_viewport_rows(usize::from(text_w), 1);
        let input_lines = crate::tui::render::styled_input_lines(
            parent,
            &vp.text,
            &vp.image_placeholders,
            &vp.row_selections,
            band_style,
        );
        frame.render_widget(
            Paragraph::new(input_lines)
                .style(semantic_style(parent, SemanticColor::Assistant).patch(band_style)),
            text_area,
        );
        let cursor_x = text_area
            .x
            .saturating_add(u16::try_from(vp.cursor_column).unwrap_or(0))
            .min(text_area.right().saturating_sub(1));
        frame.set_cursor_position(Position::new(cursor_x, text_area.y));
    }

    let bottom_y = area.y.saturating_add(2);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(usize::from(area.width)),
            border_style,
        ))),
        Rect::new(area.x, bottom_y, area.width, 1),
    );
}

fn footer_chips(
    view: &PlanViewOverlay,
    can_review: bool,
    compact: bool,
) -> Vec<(String, SemanticColor, PlanViewPointer)> {
    let comment_count = view.child_state().comment_count();
    let commenting = view.child_state().is_commenting();
    let mut chips = Vec::new();
    if can_review {
        let approve_label = if compact {
            if comment_count > 0 {
                format!("Approve ({comment_count})")
            } else {
                "Approve".to_owned()
            }
        } else if comment_count > 0 {
            format!("Approve w/ {comment_count} comments")
        } else {
            "Approve".to_owned()
        };
        chips.push((
            approve_label,
            SemanticColor::Success,
            PlanViewPointer::Approve,
        ));
        chips.push((
            if compact {
                "Revise".to_owned()
            } else {
                "Request changes".to_owned()
            },
            SemanticColor::Warning,
            PlanViewPointer::RequestChanges,
        ));
        chips.push((
            "Abandon".to_owned(),
            SemanticColor::Error,
            PlanViewPointer::Abandon,
        ));
    }
    let comment_label = if compact {
        if comment_count > 0 {
            format!("Comment ({comment_count})")
        } else if commenting {
            "Comment ●".to_owned()
        } else {
            "Comment".to_owned()
        }
    } else if comment_count > 0 {
        format!("Alt+C Comment ({comment_count})")
    } else if commenting {
        "Alt+C Comment ●".to_owned()
    } else {
        "Alt+C Comment".to_owned()
    };
    chips.push((
        comment_label,
        if commenting {
            SemanticColor::Focus
        } else {
            SemanticColor::Muted
        },
        PlanViewPointer::ToggleComment,
    ));
    chips.push((
        "Copy".to_owned(),
        SemanticColor::Muted,
        PlanViewPointer::CopyPlan,
    ));
    chips.push((
        if compact {
            "Back".to_owned()
        } else {
            "← Back".to_owned()
        },
        SemanticColor::Muted,
        PlanViewPointer::Close,
    ));
    chips
}

fn layout_footer_rows(
    view: &PlanViewOverlay,
    can_review: bool,
    avail_width: u16,
) -> Vec<Vec<FooterChipItem>> {
    let compact = avail_width < COMPACT_FOOTER_WIDTH;
    let gap: u16 = if avail_width < TIGHT_CHIP_GAP_WIDTH {
        1
    } else {
        2
    };
    let chips = footer_chips(view, can_review, compact);
    let mut rows: Vec<Vec<FooterChipItem>> = Vec::new();
    let mut current_row: Vec<FooterChipItem> = Vec::new();
    let mut cursor_x: u16 = 0;

    for (label, color, pointer) in chips {
        let chip_width = u16::try_from(UnicodeWidthStr::width(label.as_str()).saturating_add(2))
            .unwrap_or(u16::MAX);
        let needed = if current_row.is_empty() {
            chip_width
        } else {
            gap.saturating_add(chip_width)
        };
        if !current_row.is_empty() && cursor_x.saturating_add(needed) > avail_width {
            rows.push(current_row);
            current_row = Vec::new();
            cursor_x = 0;
        }
        let x_offset = if current_row.is_empty() {
            0
        } else {
            cursor_x.saturating_add(gap)
        };
        current_row.push(FooterChipItem {
            label,
            color,
            pointer,
            x_offset,
            width: chip_width,
        });
        cursor_x = x_offset.saturating_add(chip_width);
    }
    if !current_row.is_empty() {
        rows.push(current_row);
    }
    rows
}

fn render_plan_footer(
    frame: &mut Frame<'_>,
    state: &TuiState,
    view: &PlanViewOverlay,
    can_review: bool,
    area: Rect,
) {
    if area.is_empty() {
        return;
    }
    let surface = modal_surface_style(state);
    let chip_bg = state.theme().bg_color(SemanticColor::ChipBackground);
    let rows = layout_footer_rows(view, can_review, area.width);
    let single_row = rows.len() == 1;
    let avail = usize::from(area.width);

    let mut lines = Vec::with_capacity(rows.len().min(usize::from(area.height)));
    for (row_index, row) in rows.iter().take(usize::from(area.height)).enumerate() {
        let mut spans = Vec::new();
        let mut cursor = 0u16;
        for chip in row {
            if chip.x_offset > cursor {
                spans.push(Span::styled(
                    " ".repeat(usize::from(chip.x_offset - cursor)),
                    surface,
                ));
            }
            let mut style = semantic_style(state, chip.color).add_modifier(Modifier::BOLD);
            if let Some(bg) = chip_bg {
                style = style.bg(bg);
            }
            spans.push(Span::styled(format!("[{}]", chip.label), style));
            cursor = chip.x_offset.saturating_add(chip.width);
        }

        if single_row && row_index == 0 {
            let hint = if can_review {
                "a Approve · r Revise · q Abandon · Esc Back"
            } else {
                "y Copy · ↑↓ Scroll · Esc Back"
            };
            let chips_width = usize::from(cursor);
            let hint_width = UnicodeWidthStr::width(hint);
            if chips_width + 2 + hint_width <= avail {
                let pad = avail.saturating_sub(chips_width + hint_width);
                spans.push(Span::styled(" ".repeat(pad), surface));
                spans.push(Span::styled(
                    hint,
                    semantic_style(state, SemanticColor::Muted).patch(surface),
                ));
            }
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines).style(surface), area);
}

pub(crate) fn plan_view_pointer(
    state: &TuiState,
    area: Rect,
    position: Position,
) -> Option<PlanViewPointer> {
    let plans = state.plan().plans();
    let view = match state.overlay() {
        Some(crate::tui::overlay::Overlay::PlanView(view)) => Some(view),
        _ => None,
    };
    let can_review = view.is_some_and(|v| can_review_plan(state, v));
    let layout = plan_view_layout(area, plans.len(), view, can_review);
    if layout.modal.close_hit(position) {
        return Some(PlanViewPointer::Close);
    }
    if let Some(rail) = layout.rail
        && rail.contains(position)
        && let Some(view) = view
    {
        let local_y = usize::from(position.y.saturating_sub(rail.y));
        if local_y >= 1 {
            let visible_rows = usize::from(rail.height.saturating_sub(1));
            let selected_index = plans
                .iter()
                .position(|doc| &doc.summary.id == view.selected_plan_id())
                .unwrap_or(0);
            let scroll = view.effective_rail_scroll(selected_index, plans.len(), visible_rows);
            if let Some(doc) = plans.get(scroll.saturating_add(local_y - 1)) {
                return Some(PlanViewPointer::SelectPlan(doc.summary.id.clone()));
            }
        }
    }
    if layout.composer.contains(position) {
        return Some(PlanViewPointer::Composer);
    }
    if layout.timeline.contains(position)
        && let Some(view) = view
    {
        return match timeline_mouse_down(
            view.child_state(),
            layout.timeline,
            Size::new(area.width, area.height),
            position,
        ) {
            TimelineMouseDown::Copy(text) => Some(PlanViewPointer::TimelineCopy(text)),
            TimelineMouseDown::CopyTooLarge => Some(PlanViewPointer::TimelineCopyTooLarge),
            TimelineMouseDown::Select(selection) => {
                Some(PlanViewPointer::TimelineSelect(selection))
            }
            _ => None,
        };
    }
    if layout.footer.contains(position)
        && let Some(view) = view
    {
        let row_index = usize::from(position.y.saturating_sub(layout.footer.y));
        let col = position.x.saturating_sub(layout.footer.x);
        let rows = layout_footer_rows(view, can_review, layout.footer.width);
        return rows.get(row_index).and_then(|row| {
            row.iter()
                .find(|chip| col >= chip.x_offset && col < chip.x_offset.saturating_add(chip.width))
                .map(|chip| chip.pointer.clone())
        });
    }
    None
}

/// Synchronizes the embedded plan timeline viewport (clamping its initial
/// top-of-document scroll offset to the real `max_scroll`) and rail scroll
/// position before rendering or handling scroll input.
pub(crate) fn prepare_viewport(state: &mut TuiState, area: Rect) {
    let plan_count = state.plan().plans().len();
    let (layout, selected_index) = {
        let Some(crate::tui::overlay::Overlay::PlanView(view)) = state.overlay() else {
            return;
        };
        let can_review = can_review_plan(state, view);
        let selected_index = state
            .plan()
            .plans()
            .iter()
            .position(|doc| &doc.summary.id == view.selected_plan_id())
            .unwrap_or(0);
        (
            plan_view_layout(area, plan_count, Some(view), can_review),
            selected_index,
        )
    };
    let Some(crate::tui::overlay::Overlay::PlanView(view)) = state.overlay_mut() else {
        return;
    };
    if let Some(rail) = layout.rail {
        let visible_rows = usize::from(rail.height.saturating_sub(1));
        view.sync_rail_scroll(selected_index, plan_count, visible_rows);
    }
    if !layout.timeline.is_empty() && view.child_state().text_selection().is_none() {
        crate::tui::render::prepare_timeline_viewport(view.child_state_mut(), layout.timeline);
    }
}

/// Handles mouse wheel scrolling inside [`PlanViewOverlay`]:
/// - Hovering the left `PLANS` rail steps to the previous/next plan (and scrolls
///   the rail viewport automatically when there are more plans than visible rows).
/// - Hovering the plan document pane scrolls the Markdown timeline.
pub(crate) fn handle_mouse_scroll(
    state: &mut TuiState,
    area: Rect,
    position: Position,
    up: bool,
    lines: usize,
) -> crate::tui::controller::ControllerEffect {
    prepare_viewport(state, area);

    let plan_count = state.plan().plans().len();
    let (layout, selected_index) = {
        let Some(crate::tui::overlay::Overlay::PlanView(view)) = state.overlay() else {
            return crate::tui::controller::ControllerEffect::None;
        };
        let can_review = can_review_plan(state, view);
        let selected_index = state
            .plan()
            .plans()
            .iter()
            .position(|doc| &doc.summary.id == view.selected_plan_id())
            .unwrap_or(0);
        (
            plan_view_layout(area, plan_count, Some(view), can_review),
            selected_index,
        )
    };

    if layout.rail.is_some_and(|rail| rail.contains(position)) {
        if plan_count > 1 {
            let next_index = if up {
                selected_index.saturating_sub(1)
            } else {
                selected_index
                    .saturating_add(1)
                    .min(plan_count.saturating_sub(1))
            };
            if next_index != selected_index
                && let Some(next_id) = state
                    .plan()
                    .plans()
                    .get(next_index)
                    .map(|doc| doc.summary.id.clone())
            {
                state.select_plan_view(next_id);
                prepare_viewport(state, area);
            }
        }
        return crate::tui::controller::ControllerEffect::None;
    }

    if let Some(crate::tui::overlay::Overlay::PlanView(view)) = state.overlay_mut() {
        let step = lines.max(1);
        if up {
            view.scroll_up(step);
        } else {
            view.scroll_down(step);
        }
    }
    crate::tui::controller::ControllerEffect::None
}

fn status_color(status: PlanDocumentStatus) -> SemanticColor {
    match status {
        PlanDocumentStatus::Draft => SemanticColor::Focus,
        PlanDocumentStatus::AwaitingApproval => SemanticColor::Warning,
        PlanDocumentStatus::Approved => SemanticColor::Success,
        PlanDocumentStatus::Abandoned => SemanticColor::Muted,
    }
}
