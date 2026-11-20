//! Shared modal chrome, responsive sizing tiers, consistent padding, dividers,
//! and clickable top-right `[×]` close hit-testing across all TUI overlays.

use super::{state::TuiState, text_wrap::semantic_style, theme::SemanticColor};
use ratatui::{
    Frame,
    layout::{Alignment, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

/// Responsive sizing and inner-padding geometry shared by rendering and mouse
/// hit-testing for any centered modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ModalRects {
    pub(crate) region: Rect,
    pub(crate) inner: Rect,
    pub(crate) close_button: Option<Rect>,
}

impl ModalRects {
    pub(crate) fn close_hit(&self, position: Position) -> bool {
        self.close_button
            .is_some_and(|button| button.contains(position))
    }
}

/// Computes centered outer bounds, responsive horizontal/vertical inner padding,
/// and the top-right `[×]` close button rect for a modal dialog.
pub(crate) fn modal_rects(area: Rect, max_width: u16, desired_height: u16) -> ModalRects {
    let width = area.width.saturating_sub(4).min(max_width).max(1);
    let height = area.height.saturating_sub(2).min(desired_height).max(1);
    let region = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    let h_pad: u16 = if region.width >= 76 {
        3
    } else if region.width >= 32 {
        2
    } else {
        1
    };
    let v_pad: u16 = 1;
    let inner = Rect::new(
        region.x.saturating_add(h_pad),
        region.y.saturating_add(v_pad),
        region.width.saturating_sub(h_pad.saturating_mul(2)),
        region.height.saturating_sub(v_pad.saturating_mul(2)),
    );
    let close_button = (region.width >= 16 && region.height >= 3).then(|| {
        // Sits inside the top border on the right: `... [×] ╮`
        Rect::new(region.right().saturating_sub(5), region.y, 3, 1)
    });
    ModalRects {
        region,
        inner,
        close_button,
    }
}

/// Renders a unified rounded modal container with `Surface` fill, semantic
/// border, branded `M` title prefix, and optional clickable `[×]` close badge.
pub(crate) fn render_modal_frame(
    frame: &mut Frame<'_>,
    state: &TuiState,
    rects: ModalRects,
    title: &str,
    border_slot: SemanticColor,
    show_close: bool,
) {
    let surface = modal_surface_style(state);
    frame.render_widget(Clear, rects.region);
    let clean_title = title
        .trim()
        .strip_prefix("M  ")
        .map(str::trim)
        .unwrap_or(title.trim());
    let mut block = Block::default()
        .title(Line::from(vec![
            Span::styled(
                " 🎀 ",
                semantic_style(state, SemanticColor::Status)
                    .add_modifier(Modifier::BOLD)
                    .patch(surface),
            ),
            Span::styled(
                format!("{clean_title} "),
                semantic_style(state, SemanticColor::Heading)
                    .add_modifier(Modifier::BOLD)
                    .patch(surface),
            ),
        ]))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(semantic_style(state, border_slot).add_modifier(Modifier::BOLD))
        .style(surface);

    if show_close && rects.close_button.is_some() {
        block = block.title(
            Line::from(vec![
                Span::styled(
                    "[×]",
                    semantic_style(state, SemanticColor::Muted)
                        .add_modifier(Modifier::BOLD)
                        .patch(surface),
                ),
                Span::styled(" ", surface),
            ])
            .alignment(Alignment::Right),
        );
    }

    frame.render_widget(block, rects.region);
}

/// Renders a subtle horizontal hairline divider (`─`) across `area`.
pub(crate) fn render_divider(frame: &mut Frame<'_>, state: &TuiState, area: Rect) {
    if area.is_empty() {
        return;
    }
    let rule = "─".repeat(usize::from(area.width));
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            rule,
            semantic_style(state, SemanticColor::Border).patch(modal_surface_style(state)),
        ))),
        area,
    );
}

/// Renders a subtle vertical hairline divider (`│`) down `area`.
pub(crate) fn render_vertical_divider(frame: &mut Frame<'_>, state: &TuiState, area: Rect) {
    if area.is_empty() {
        return;
    }
    let style = semantic_style(state, SemanticColor::Border).patch(modal_surface_style(state));
    let lines: Vec<Line<'static>> = (0..area.height).map(|_| Line::styled("│", style)).collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// Elevated modal background style (`SemanticColor::Surface`).
pub(crate) fn modal_surface_style(state: &TuiState) -> Style {
    state
        .theme()
        .bg_color(SemanticColor::Surface)
        .map_or_else(Style::default, |color| Style::default().bg(color))
}

/// Active/selected row style inside modals (`SemanticColor::SelectionBackground`).
pub(crate) fn modal_selection_style(state: &TuiState, selected: bool) -> Style {
    if !selected {
        return modal_surface_style(state);
    }
    let foreground = state
        .theme()
        .color(SemanticColor::Assistant)
        .unwrap_or(Color::White);
    let background = state
        .theme()
        .bg_color(SemanticColor::SelectionBackground)
        .unwrap_or(Color::Magenta);
    Style::default().fg(foreground).bg(background)
}

#[cfg(test)]
mod tests {
    use super::modal_rects;
    use ratatui::layout::{Position, Rect};

    #[test]
    fn modal_rects_provide_responsive_padding_and_close_hit_target() {
        let wide = modal_rects(Rect::new(0, 0, 120, 40), 104, 24);
        assert_eq!(wide.region.width, 104);
        assert_eq!(wide.inner.x, wide.region.x + 3);
        let close = wide.close_button.expect("close button should exist");
        assert!(wide.close_hit(Position::new(close.x + 1, close.y)));
        assert!(!wide.close_hit(Position::new(wide.region.x, wide.region.y)));

        let narrow = modal_rects(Rect::new(0, 0, 40, 16), 80, 20);
        assert_eq!(narrow.region.width, 36);
        assert_eq!(narrow.inner.x, narrow.region.x + 2);
    }
}
