//! Pure `chars()` state-machine ANSI SGR escape parser for command output.

use crate::tui::text_wrap::{StyledTextPart, wrap_styled_parts_preserving_leading_whitespace};
use ratatui::{
    style::{Color, Modifier, Style},
    text::Line,
};

const MAX_CSI_PARAM_LEN: usize = 64;

enum EscapeState {
    Normal,
    Esc,
    Csi(String),
    Osc { saw_esc: bool },
}

/// Appends ANSI-colored text as wrapped ratatui lines while stripping non-SGR CSI/OSC sequences.
pub(super) fn append_ansi_output(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    base_style: Style,
    width: u16,
) {
    let mut style = base_style;
    for raw_line in text.split('\n') {
        let parts = parse_ansi_line(raw_line, base_style, &mut style);
        if parts.is_empty() {
            lines.push(Line::default());
        } else {
            lines.extend(wrap_styled_parts_preserving_leading_whitespace(
                parts, width,
            ));
        }
    }
}

fn parse_ansi_line(
    line: &str,
    base_style: Style,
    current_style: &mut Style,
) -> Vec<StyledTextPart> {
    let mut parts = Vec::new();
    let mut buffer = String::new();
    let mut state = EscapeState::Normal;

    for ch in line.chars() {
        state = match state {
            EscapeState::Normal => match ch {
                '\x1b' => {
                    if !buffer.is_empty() {
                        parts.push(StyledTextPart::new(
                            std::mem::take(&mut buffer),
                            *current_style,
                            false,
                        ));
                    }
                    EscapeState::Esc
                }
                '\t' => {
                    buffer.push_str("    ");
                    EscapeState::Normal
                }
                ch if !ch.is_control() => {
                    buffer.push(ch);
                    EscapeState::Normal
                }
                _ => EscapeState::Normal,
            },
            EscapeState::Esc => match ch {
                '[' => EscapeState::Csi(String::new()),
                ']' => EscapeState::Osc { saw_esc: false },
                _ => EscapeState::Normal,
            },
            EscapeState::Csi(mut params) => {
                if ('\x40'..='\x7e').contains(&ch) {
                    if ch == 'm' {
                        apply_sgr_params(&params, base_style, current_style);
                    }
                    EscapeState::Normal
                } else {
                    if params.len() < MAX_CSI_PARAM_LEN {
                        params.push(ch);
                    }
                    EscapeState::Csi(params)
                }
            }
            EscapeState::Osc { saw_esc } => match ch {
                '\x07' => EscapeState::Normal,
                '\\' if saw_esc => EscapeState::Normal,
                '\x1b' => EscapeState::Osc { saw_esc: true },
                _ => EscapeState::Osc { saw_esc: false },
            },
        };
    }

    if !buffer.is_empty() {
        parts.push(StyledTextPart::new(buffer, *current_style, false));
    }
    parts
}

fn apply_sgr_params(params: &str, base_style: Style, style: &mut Style) {
    if params.is_empty() {
        *style = base_style;
        return;
    }
    // `:` is the ISO subparameter separator (`38:2:255:0:0`). An unparsable
    // piece is skipped so it does not become reset (`0`).
    let mut codes = params
        .split([';', ':'])
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<u16>().ok());
    while let Some(code) = codes.next() {
        match code {
            0 => *style = base_style,
            1 => *style = style.add_modifier(Modifier::BOLD),
            2 => *style = style.add_modifier(Modifier::DIM),
            3 => *style = style.add_modifier(Modifier::ITALIC),
            4 => *style = style.add_modifier(Modifier::UNDERLINED),
            22 => *style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => *style = style.remove_modifier(Modifier::ITALIC),
            24 => *style = style.remove_modifier(Modifier::UNDERLINED),
            30..=37 => *style = style.fg(ansi_standard_color((code - 30) as u8, false)),
            39 => style.fg = base_style.fg,
            40..=47 => *style = style.bg(ansi_standard_color((code - 40) as u8, false)),
            49 => style.bg = base_style.bg,
            90..=97 => *style = style.fg(ansi_standard_color((code - 90) as u8, true)),
            100..=107 => *style = style.bg(ansi_standard_color((code - 100) as u8, true)),
            38 | 48 => {
                if let Some(color) = parse_extended_sgr_color(&mut codes) {
                    *style = if code == 38 {
                        style.fg(color)
                    } else {
                        style.bg(color)
                    };
                }
            }
            _ => {}
        }
    }
}

fn parse_extended_sgr_color(codes: &mut impl Iterator<Item = u16>) -> Option<Color> {
    match codes.next()? {
        5 => Some(Color::Indexed(u8::try_from(codes.next()?).ok()?)),
        2 => Some(Color::Rgb(
            u8::try_from(codes.next()?).ok()?,
            u8::try_from(codes.next()?).ok()?,
            u8::try_from(codes.next()?).ok()?,
        )),
        _ => None,
    }
}

fn ansi_standard_color(index: u8, bright: bool) -> Color {
    match (index, bright) {
        (0, false) => Color::Black,
        (1, false) => Color::Red,
        (2, false) => Color::Green,
        (3, false) => Color::Yellow,
        (4, false) => Color::Blue,
        (5, false) => Color::Magenta,
        (6, false) => Color::Cyan,
        (7, false) => Color::Gray,
        (0, true) => Color::DarkGray,
        (1, true) => Color::LightRed,
        (2, true) => Color::LightGreen,
        (3, true) => Color::LightYellow,
        (4, true) => Color::LightBlue,
        (5, true) => Color::LightMagenta,
        (6, true) => Color::LightCyan,
        _ => Color::White,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn span<'a>(lines: &'a [Line<'a>], text: &str) -> &'a ratatui::text::Span<'a> {
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content == text)
            .unwrap_or_else(|| panic!("missing span {text}"))
    }

    #[test]
    fn colon_sgr_sets_rgb_and_a_bad_parameter_does_not_reset() {
        let mut lines = Vec::new();
        append_ansi_output(
            &mut lines,
            "\x1b[1;38:2:10:20:30mRGB\x1b[38:2:12mSTILL\x1b[32mGREEN",
            Style::default(),
            40,
        );
        let rgb = span(&lines, "RGB");
        assert_eq!(rgb.style.fg, Some(Color::Rgb(10, 20, 30)));
        assert!(rgb.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(span(&lines, "STILL").style.fg, Some(Color::Rgb(10, 20, 30)));
        assert_eq!(span(&lines, "GREEN").style.fg, Some(Color::Green));
    }

    #[test]
    fn empty_colorspace_id_still_reads_rgb() {
        let mut lines = Vec::new();
        append_ansi_output(
            &mut lines,
            "\x1b[38:2::255:80:0mORANGE",
            Style::default(),
            20,
        );
        assert_eq!(
            span(&lines, "ORANGE").style.fg,
            Some(Color::Rgb(255, 80, 0))
        );
    }
}
