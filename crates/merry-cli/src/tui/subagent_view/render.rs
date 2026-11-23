//! Fullscreen framed interactive Subagent Takeover View renderer and pointer
//! hit-tester (reusing the parent timeline renderer + embedded follow-up
//! composer, matching Section 4.2 of the TUI redesign spec).

use super::{SubagentEntry, SubagentTakeoverOverlay, SubagentTakeoverPointer};
use crate::tui::{
    click_target::{chip_hit, truncate_columns},
    modal_frame::{
        ModalRects, modal_selection_style, modal_surface_style, render_divider,
        render_vertical_divider,
    },
    motion::{self, MotionClock},
    overlay_render::semantic_style,
    render::{TimelineMouseDown, render_timeline_pane, timeline_link_at, timeline_mouse_down},
    state::TuiState,
    theme::SemanticColor,
};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Position, Rect, Size},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

const SUBAGENT_COMPOSER_HEIGHT: u16 = 3;

#[derive(Debug, Clone, Copy)]
struct TakeoverLayout {
    modal: ModalRects,
    rail: Option<Rect>,
    divider: Option<Rect>,
    timeline: Rect,
    composer: Rect,
    footer_divider: Rect,
    footer: Rect,
}

fn takeover_layout(area: Rect, subagent_count: usize) -> TakeoverLayout {
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
        return TakeoverLayout {
            modal,
            rail: None,
            divider: None,
            timeline: Rect::default(),
            composer: Rect::default(),
            footer_divider: Rect::default(),
            footer: Rect::default(),
        };
    }

    let has_footer_divider = inner.height >= 6;
    let v_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(u16::from(has_footer_divider)),
            Constraint::Length(1),
        ])
        .split(inner);

    let main = v_chunks[0];
    let (rail, divider, content) = if subagent_count > 1 && main.width >= 76 {
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
        SUBAGENT_COMPOSER_HEIGHT
    } else {
        0
    };
    let right_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(composer_h)])
        .split(content);

    TakeoverLayout {
        modal,
        rail,
        divider,
        timeline: right_chunks[0],
        composer: right_chunks[1],
        footer_divider: v_chunks[1],
        footer: v_chunks[2],
    }
}

pub(crate) fn render_subagent_takeover(
    frame: &mut Frame<'_>,
    state: &TuiState,
    takeover: &SubagentTakeoverOverlay,
) {
    let subagents = state.subagents();
    let layout = takeover_layout(frame.area(), subagents.len());
    let selected_index = subagents
        .iter()
        .position(|entry| entry.agent_id == takeover.selected_agent_id())
        .unwrap_or(0);
    let current = subagents.get(selected_index);
    let border_slot = current.map_or(SemanticColor::BorderFocus, |entry| entry.phase.color());
    let surface = modal_surface_style(state);
    let clock = MotionClock::now();

    frame.render_widget(Clear, layout.modal.region);
    let title_text = match current {
        Some(entry) => {
            if let Some(display_name) = entry
                .display_name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                format!(
                    "{} Subagent · {} ({})",
                    entry.phase.glyph(clock),
                    display_name,
                    entry.agent_id
                )
            } else {
                format!(
                    "{} Subagent · {} ({})",
                    entry.phase.glyph(clock),
                    entry.agent_id,
                    entry.task_id
                )
            }
        }
        None => "Subagents".to_owned(),
    };
    let mut block = Block::default()
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
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(semantic_style(state, border_slot).add_modifier(Modifier::BOLD))
        .style(surface);

    let mut right_spans = vec![Span::styled(" ", surface)];
    if let Some(entry) = current {
        right_spans.push(Span::styled(
            format!("[{}]", entry.phase.label()),
            semantic_style(state, entry.phase.color())
                .add_modifier(Modifier::BOLD)
                .patch(surface),
        ));
        if subagents.len() > 1 {
            right_spans.push(Span::styled(
                format!("  {}/{}", selected_index + 1, subagents.len()),
                semantic_style(state, SemanticColor::Muted).patch(surface),
            ));
        }
        right_spans.push(Span::styled("  ", surface));
    }
    right_spans.push(Span::styled(
        "[×]",
        semantic_style(state, SemanticColor::Muted)
            .add_modifier(Modifier::BOLD)
            .patch(surface),
    ));
    right_spans.push(Span::styled(" ", surface));
    block = block.title(Line::from(right_spans).alignment(Alignment::Right));
    frame.render_widget(block, layout.modal.region);

    if layout.modal.inner.is_empty() {
        return;
    }

    if let Some(rail) = layout.rail {
        render_subagent_rail(frame, state, subagents, selected_index, clock, rail);
    }
    if let Some(divider) = layout.divider {
        render_vertical_divider(frame, state, divider);
    }

    // Reuse the main virtualized timeline renderer for full Markdown, Thinking
    // rails, running shimmer, and collapsible tool outputs!
    if !layout.timeline.is_empty() {
        if takeover.child_state().timeline().is_empty() {
            let empty_msg = current.map_or(
                "No subagents have been launched in this session.",
                |_| "Waiting for subagent transcript…",
            );
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    empty_msg,
                    semantic_style(state, SemanticColor::Muted).patch(surface),
                )))
                .style(surface),
                layout.timeline,
            );
        } else {
            render_timeline_pane(frame, takeover.child_state(), layout.timeline);
        }
    }

    // Interactive composer directly below the subagent timeline.
    if !layout.composer.is_empty() {
        render_subagent_composer(frame, state, takeover, current, layout.composer);
    }

    render_divider(frame, state, layout.footer_divider);
    let is_active = current.is_some_and(|entry| entry.phase.is_active());
    let can_close = current.is_some_and(|entry| !entry.phase.is_active() && !entry.closed);
    render_takeover_footer(
        frame,
        state,
        subagents.len() > 1,
        is_active,
        can_close,
        layout.footer,
    );
}

fn render_subagent_composer(
    frame: &mut Frame<'_>,
    parent: &TuiState,
    takeover: &SubagentTakeoverOverlay,
    current: Option<&SubagentEntry>,
    area: Rect,
) {
    if area.height < 3 || area.width < 6 {
        return;
    }
    let border_style = semantic_style(parent, SemanticColor::Border);
    let focus_style = semantic_style(parent, SemanticColor::Focus).add_modifier(Modifier::BOLD);
    let surface_bg = parent.theme().bg_color(SemanticColor::SurfaceSubtle);
    let band_style = surface_bg.map_or_else(Style::default, |bg| Style::default().bg(bg));

    let label = current.map_or_else(
        || "Subagent input".to_owned(),
        |entry| {
            let name = entry.display_label();
            if entry.phase.is_active() {
                format!("Guide {name}")
            } else {
                format!("Follow up with {name}")
            }
        },
    );
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
    let child = takeover.child_state();
    if child.plain_input_text().is_none_or(str::is_empty) {
        let placeholder = if current.is_some_and(|e| e.phase.is_active()) {
            "Type guidance for this subagent and press Enter…"
        } else {
            "Send a follow-up message to continue with this subagent…"
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

fn render_subagent_rail(
    frame: &mut Frame<'_>,
    state: &TuiState,
    subagents: &[SubagentEntry],
    selected_index: usize,
    clock: MotionClock,
    area: Rect,
) {
    let surface = modal_surface_style(state);
    let mut lines = vec![Line::from(Span::styled(
        "AGENTS",
        semantic_style(state, SemanticColor::Focus)
            .add_modifier(Modifier::BOLD)
            .patch(surface),
    ))];
    let width = usize::from(area.width);
    for (index, entry) in subagents
        .iter()
        .enumerate()
        .take(usize::from(area.height.saturating_sub(1)))
    {
        let selected = index == selected_index;
        let base = modal_selection_style(state, selected);
        let glyph = entry.phase.glyph(clock);
        let label = truncate_columns(entry.display_label(), width.saturating_sub(5).max(1));
        let pad = width.saturating_sub(4 + UnicodeWidthStr::width(label.as_str()));
        let mut row_spans = vec![
            Span::styled(if selected { "▌ " } else { "  " }, base),
            Span::styled(
                format!("{glyph} "),
                semantic_style(state, entry.phase.color())
                    .add_modifier(Modifier::BOLD)
                    .patch(base),
            ),
            Span::styled(label, base.add_modifier(Modifier::BOLD)),
        ];
        if entry.phase.is_active() {
            row_spans = motion::flow_gradient_spans(
                row_spans,
                motion::RUNNING_FLOW_STOPS,
                motion::RUNNING_FLOW_WAVELENGTH,
                motion::RUNNING_FLOW_CHARS_PER_SEC,
                0,
                None,
                clock,
                state.animations_enabled(),
            );
        }
        row_spans.push(Span::styled(" ".repeat(pad), base));
        lines.push(Line::from(row_spans));
    }
    frame.render_widget(Paragraph::new(lines).style(surface), area);
}

fn render_takeover_footer(
    frame: &mut Frame<'_>,
    state: &TuiState,
    has_multiple: bool,
    is_active: bool,
    can_close: bool,
    area: Rect,
) {
    if area.is_empty() {
        return;
    }
    let surface = modal_surface_style(state);
    let chip_bg = state.theme().bg_color(SemanticColor::ChipBackground);
    let make_chip = |label: &str, color: SemanticColor| {
        let mut st = semantic_style(state, color).add_modifier(Modifier::BOLD);
        if let Some(bg) = chip_bg {
            st = st.bg(bg);
        }
        Span::styled(format!("[{label}]"), st)
    };
    let mut spans = vec![make_chip("← Back", SemanticColor::Focus)];
    if is_active {
        spans.push(Span::styled("  ", surface));
        spans.push(make_chip("Stop agent", SemanticColor::Error));
    } else if can_close {
        spans.push(Span::styled("  ", surface));
        spans.push(make_chip("Close", SemanticColor::Muted));
    }
    if has_multiple {
        spans.push(Span::styled("  ", surface));
        spans.push(make_chip("Prev", SemanticColor::ToolKeyword));
        spans.push(Span::styled("  ", surface));
        spans.push(make_chip("Next", SemanticColor::ToolKeyword));
    }
    spans.push(Span::styled("  ", surface));
    spans.push(make_chip("Refresh", SemanticColor::Muted));

    let hint = "Enter Send · Esc Back · ↑↓ Scroll";
    let chips_width: usize = spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum();
    let avail = usize::from(area.width);
    if chips_width + 2 + UnicodeWidthStr::width(hint) <= avail {
        let pad = avail.saturating_sub(chips_width + UnicodeWidthStr::width(hint));
        spans.push(Span::styled(" ".repeat(pad), surface));
        spans.push(Span::styled(
            hint,
            semantic_style(state, SemanticColor::Muted).patch(surface),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(surface), area);
}

pub(crate) fn subagent_view_timeline_rect(state: &TuiState, area: Rect) -> Rect {
    takeover_layout(area, state.subagents().len()).timeline
}

pub(crate) fn subagent_takeover_pointer(
    state: &TuiState,
    area: Rect,
    position: Position,
) -> Option<SubagentTakeoverPointer> {
    let subagents = state.subagents();
    let layout = takeover_layout(area, subagents.len());
    if layout.modal.close_hit(position) {
        return Some(SubagentTakeoverPointer::Back);
    }
    if let Some(rail) = layout.rail
        && rail.contains(position)
    {
        let local_y = usize::from(position.y.saturating_sub(rail.y));
        if local_y >= 1
            && let Some(entry) = subagents.get(local_y - 1)
        {
            return Some(SubagentTakeoverPointer::SelectAgent(entry.agent_id.clone()));
        }
    }
    if layout.composer.contains(position) {
        return Some(SubagentTakeoverPointer::Composer);
    }
    if layout.timeline.contains(position)
        && let Some(crate::tui::overlay::Overlay::SubagentInspect(takeover)) = state.overlay()
    {
        return match timeline_mouse_down(
            takeover.child_state(),
            layout.timeline,
            Size::new(area.width, area.height),
            position,
        ) {
            TimelineMouseDown::ToggleCommand(index) => {
                Some(SubagentTakeoverPointer::TimelineToggleCommand(index))
            }
            TimelineMouseDown::ToggleThinking(index) => {
                Some(SubagentTakeoverPointer::TimelineToggleThinking(index))
            }
            TimelineMouseDown::Copy(text) => Some(SubagentTakeoverPointer::TimelineCopy(text)),
            TimelineMouseDown::CopyTooLarge => Some(SubagentTakeoverPointer::TimelineCopyTooLarge),
            _ => timeline_link_at(takeover.child_state(), layout.timeline, position)
                .map(|link| SubagentTakeoverPointer::TimelineOpenUrl(link.url)),
        };
    }
    if layout.footer.contains(position) {
        let selected_id = match state.overlay() {
            Some(crate::tui::overlay::Overlay::SubagentInspect(takeover)) => {
                takeover.selected_agent_id()
            }
            _ => "",
        };
        let selected_entry = state.subagent_by_id(selected_id);
        let is_active = selected_entry.is_some_and(|entry| entry.phase.is_active());
        let can_close =
            selected_entry.is_some_and(|entry| !entry.phase.is_active() && !entry.closed);
        let has_multiple = subagents.len() > 1;
        let mut chips: Vec<(&str, SubagentTakeoverPointer)> =
            vec![("← Back", SubagentTakeoverPointer::Back)];
        if is_active {
            chips.push(("Stop agent", SubagentTakeoverPointer::Stop));
        } else if can_close {
            chips.push(("Close", SubagentTakeoverPointer::Close));
        }
        if has_multiple {
            chips.push(("Prev", SubagentTakeoverPointer::Previous));
            chips.push(("Next", SubagentTakeoverPointer::Next));
        }
        chips.push(("Refresh", SubagentTakeoverPointer::Refresh));
        let labels: Vec<&str> = chips.iter().map(|(label, _)| *label).collect();
        return chip_hit(
            Position::new(layout.footer.x, layout.footer.y),
            &labels,
            position,
        )
        .and_then(|idx| chips.get(idx).map(|(_, ptr)| ptr.clone()));
    }
    None
}
