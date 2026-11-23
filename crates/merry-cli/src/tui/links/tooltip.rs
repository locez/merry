//! Floating anti-phishing URL hover tooltip rendering for timeline hyperlinks.

use super::{HoveredLink, strip_http_scheme, validate_external_url};
use crate::tui::{state::TuiState, text_wrap::semantic_style, theme::SemanticColor};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use unicode_width::UnicodeWidthChar;

/// Maximum inner text lines displayed inside a floating link hover tooltip.
const MAX_TOOLTIP_LINES: usize = 3;

/// Maximum inner text width of a floating link hover tooltip.
const MAX_TOOLTIP_INNER_WIDTH: u16 = 84;

/// Highlights the hovered hyperlink segment and draws a floating anti-phishing
/// URL preview tooltip anchored above (or below) the link within `bounds`.
pub(crate) fn render_link_tooltip(
    frame: &mut Frame<'_>,
    state: &TuiState,
    bounds: Rect,
    body: Rect,
    hovered: &HoveredLink,
    screen_y: u16,
) {
    if bounds.is_empty() || body.is_empty() {
        return;
    }

    let anchor_x = body.x.saturating_add(hovered.column);

    // Highlight the hovered link segment so the user sees the exact active span.
    if screen_y >= body.y && screen_y < body.bottom() {
        let start_x = anchor_x.max(body.x);
        let end_x = anchor_x.saturating_add(hovered.width).min(body.right());
        let buf = frame.buffer_mut();
        for x in start_x..end_x {
            let cell = &mut buf[(x, screen_y)];
            cell.set_style(
                cell.style()
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            );
        }
    }

    let clean_url = crate::text::without_control_chars(&hovered.url);
    let openable = validate_external_url(&hovered.url).is_ok();
    let surface = tooltip_surface_style(state);

    let max_inner_width = bounds
        .width
        .saturating_sub(4)
        .clamp(1, MAX_TOOLTIP_INNER_WIDTH);
    let wrapped_lines = build_tooltip_lines(state, &clean_url, openable, surface, max_inner_width);
    if wrapped_lines.is_empty() {
        return;
    }

    let content_width = wrapped_lines.iter().map(Line::width).max().unwrap_or(1);
    let content_lines = u16::try_from(wrapped_lines.len()).unwrap_or(1);
    let can_draw_boxed = bounds.width >= 6 && bounds.height >= content_lines.saturating_add(3);

    if can_draw_boxed {
        // Box: `│ ` + content + ` │` => content_width + 4.
        let box_width = u16::try_from(content_width)
            .unwrap_or(bounds.width)
            .saturating_add(4)
            .min(bounds.width);
        let box_height = content_lines.saturating_add(2);
        let tooltip_rect = place_tooltip_rect(bounds, anchor_x, screen_y, box_width, box_height);

        let border_slot = if openable {
            SemanticColor::BorderFocus
        } else {
            SemanticColor::Warning
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(semantic_style(state, border_slot).patch(surface))
            .style(surface);

        frame.render_widget(Clear, tooltip_rect);
        let inner = block.inner(tooltip_rect);
        frame.render_widget(block, tooltip_rect);
        if !inner.is_empty() {
            let padded_lines: Vec<Line<'static>> = wrapped_lines
                .into_iter()
                .map(|mut line| {
                    line.spans.insert(0, Span::styled(" ", surface));
                    line
                })
                .collect();
            frame.render_widget(Paragraph::new(padded_lines).style(surface), inner);
        }
        return;
    }

    // Compact 1-row fallback when the pane is too short for a bordered card.
    if bounds.height < 2 {
        return;
    }
    let bar_width = u16::try_from(content_width)
        .unwrap_or(bounds.width)
        .saturating_add(2)
        .min(bounds.width);
    let tooltip_rect = place_tooltip_rect(bounds, anchor_x, screen_y, bar_width, 1);
    frame.render_widget(Clear, tooltip_rect);
    let mut line = wrapped_lines.into_iter().next().unwrap_or_default();
    line.spans.insert(0, Span::styled(" ", surface));
    frame.render_widget(Paragraph::new(line).style(surface), tooltip_rect);
}

fn place_tooltip_rect(bounds: Rect, anchor_x: u16, anchor_y: u16, width: u16, height: u16) -> Rect {
    let width = width.min(bounds.width).max(1);
    let height = height.min(bounds.height).max(1);

    let max_x = bounds.right().saturating_sub(width);
    let x = anchor_x.max(bounds.x).min(max_x);

    let above_space = anchor_y.saturating_sub(bounds.y);
    let below_start = anchor_y.saturating_add(1);
    let below_space = bounds.bottom().saturating_sub(below_start);

    let y = if above_space >= height {
        anchor_y.saturating_sub(height)
    } else if below_space >= height {
        below_start
    } else if above_space >= below_space {
        bounds.y
    } else {
        bounds.bottom().saturating_sub(height)
    };

    Rect::new(x, y, width, height)
}

fn tooltip_surface_style(state: &TuiState) -> Style {
    state
        .theme()
        .bg_color(SemanticColor::Surface)
        .or_else(|| state.theme().bg_color(SemanticColor::SurfaceSubtle))
        .or_else(|| state.theme().bg_color(SemanticColor::HeaderBackground))
        .map_or_else(Style::default, |bg| Style::default().bg(bg))
}

fn build_tooltip_lines(
    state: &TuiState,
    clean_url: &str,
    openable: bool,
    surface: Style,
    max_width: u16,
) -> Vec<Line<'static>> {
    let max_width = usize::from(max_width).max(1);
    let mut styled_chars: Vec<(char, Style)> = Vec::new();

    if openable {
        let icon_style = semantic_style(state, SemanticColor::Status)
            .add_modifier(Modifier::BOLD)
            .patch(surface);
        let host_style = semantic_style(state, SemanticColor::Link)
            .add_modifier(Modifier::BOLD)
            .patch(surface);
        let rest_style = semantic_style(state, SemanticColor::TextSecondary).patch(surface);

        let split_at = split_scheme_and_authority(clean_url);
        styled_chars.extend("↗ ".chars().map(|ch| (ch, icon_style)));
        styled_chars.extend(clean_url[..split_at].chars().map(|ch| (ch, host_style)));
        styled_chars.extend(clean_url[split_at..].chars().map(|ch| (ch, rest_style)));
    } else {
        let warn_style = semantic_style(state, SemanticColor::Warning)
            .add_modifier(Modifier::BOLD)
            .patch(surface);
        let muted_style = semantic_style(state, SemanticColor::Muted).patch(surface);
        styled_chars.extend("⊘ ".chars().map(|ch| (ch, warn_style)));
        styled_chars.extend(clean_url.chars().map(|ch| (ch, warn_style)));
        styled_chars.extend(" (not http/https)".chars().map(|ch| (ch, muted_style)));
    }

    wrap_styled_chars(styled_chars, max_width, MAX_TOOLTIP_LINES, surface)
}

fn split_scheme_and_authority(url: &str) -> usize {
    let Some(after_scheme) = strip_http_scheme(url) else {
        return url.len();
    };
    let scheme_len = url.len() - after_scheme.len();
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    scheme_len + authority_end
}

fn wrap_styled_chars(
    chars: Vec<(char, Style)>,
    max_width: usize,
    max_lines: usize,
    fallback_style: Style,
) -> Vec<Line<'static>> {
    if chars.is_empty() || max_lines == 0 {
        return Vec::new();
    }

    let mut lines: Vec<Vec<(char, Style)>> = vec![Vec::new()];
    let mut current_width = 0usize;
    let mut truncated = false;

    for (ch, style) in chars {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width > 0 && current_width + ch_width > max_width {
            if lines.len() >= max_lines {
                truncated = true;
                break;
            }
            lines.push(Vec::new());
            current_width = 0;
        }
        if let Some(last) = lines.last_mut() {
            last.push((ch, style));
        }
        current_width += ch_width;
    }

    if truncated && let Some(last) = lines.last_mut() {
        let mut width: usize = last
            .iter()
            .map(|(ch, _)| UnicodeWidthChar::width(*ch).unwrap_or(0))
            .sum();
        while !last.is_empty() && width + 1 > max_width {
            if let Some((removed, _)) = last.pop() {
                width = width.saturating_sub(UnicodeWidthChar::width(removed).unwrap_or(0));
            }
        }
        let ellipsis_style = last.last().map_or(fallback_style, |(_, s)| *s);
        last.push(('…', ellipsis_style));
    }

    lines
        .into_iter()
        .map(|row| {
            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut buf = String::new();
            let mut active_style: Option<Style> = None;
            for (ch, style) in row {
                if active_style == Some(style) {
                    buf.push(ch);
                } else {
                    if let Some(prev_style) = active_style.take()
                        && !buf.is_empty()
                    {
                        spans.push(Span::styled(std::mem::take(&mut buf), prev_style));
                    }
                    buf.push(ch);
                    active_style = Some(style);
                }
            }
            if let Some(prev_style) = active_style
                && !buf.is_empty()
            {
                spans.push(Span::styled(buf, prev_style));
            }
            Line::from(spans)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::place_tooltip_rect;
    use ratatui::layout::Rect;

    #[test]
    fn place_tooltip_rect_prefers_above_and_flips_below_when_near_top() {
        let bounds = Rect::new(2, 1, 80, 20);
        // Plenty of room above y=10 -> places above without covering y=10.
        let above = place_tooltip_rect(bounds, 70, 10, 30, 3);
        assert_eq!(above.y, 7);
        assert_eq!(above.bottom(), 10);
        assert_eq!(above.right(), bounds.right());

        // Near top y=2 -> only 1 row above, flips directly below to y=3.
        let below = place_tooltip_rect(bounds, 5, 2, 30, 3);
        assert_eq!(below.y, 3);
        assert_eq!(below.x, 5);
    }
}
