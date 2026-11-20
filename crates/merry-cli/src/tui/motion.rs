//! Shared TUI motion primitives: constant-velocity horizontal liquid shimmer,
//! continuous multi-stop gradient flows (`Running` purple-pink ribbon and
//! `Merry` blue-purple-pink-white prism), downward optical-pulse vertical
//! rails, and streaming caret breathing.
//!
//! All animations derive from a single injectable [`MotionClock`] so frame
//! rendering stays deterministic under unit tests and honors `[tui] animations`.

use crate::tui::theme::{SemanticColor, TuiTheme, blend_color};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

/// Active-run refresh period (~30 fps) paired with DECSET 2026 sync updates.
pub(crate) const ACTIVE_MOTION_INTERVAL: Duration = Duration::from_millis(33);

/// Luminous pure-white spotlight crest shared by horizontal shimmer waves.
pub(crate) const SHIMMER_HIGHLIGHT: Color = Color::Rgb(255, 255, 255);

/// Gentle pastel lavender ↔ soft Merry rose-pink ↔ warm pearl-white ribbon for
/// active `Running` commands — calm enough to watch during long builds without
/// visual fatigue.
pub(crate) const RUNNING_FLOW_STOPS: &[Color] = &[
    Color::Rgb(180, 158, 232), // soft pastel lavender (#B49EE8)
    Color::Rgb(208, 152, 226), // dusty lilac-rose (#D098E2)
    Color::Rgb(238, 146, 198), // soft Merry rose-pink (#EE92C6)
    Color::Rgb(242, 234, 246), // gentle pearl-white sheen (#F2EAF6)
    Color::Rgb(238, 146, 198), // soft Merry rose-pink (#EE92C6)
    Color::Rgb(196, 156, 230), // calm lavender return (#C49CE6)
];

/// Relaxed horizontal drift speed and wide silk wavelength for `Running` commands.
pub(crate) const RUNNING_FLOW_CHARS_PER_SEC: f32 = 22.0;
pub(crate) const RUNNING_FLOW_WAVELENGTH: f32 = 48.0;

/// Signature Merry brand prism: Blue → Purple → Pink → White → Pink → Purple.
pub(crate) const MERRY_PRISM_STOPS: &[Color] = &[
    Color::Rgb(96, 180, 255),  // sky blue (#60B4FF)
    Color::Rgb(156, 114, 255), // violet-purple (#9C72FF)
    Color::Rgb(255, 118, 206), // Merry pink (#FF76CE)
    Color::Rgb(255, 255, 255), // pure white crest (#FFFFFF)
    Color::Rgb(255, 118, 206), // Merry pink (#FF76CE)
    Color::Rgb(156, 114, 255), // violet-purple (#9C72FF)
];

/// Static left-to-right Blue → Purple → Pink → White stops for idle `Merry`.
const MERRY_IDLE_STOPS: &[Color] = &[
    Color::Rgb(96, 180, 255),
    Color::Rgb(156, 114, 255),
    Color::Rgb(255, 118, 206),
    Color::Rgb(255, 255, 255),
];

/// Horizontal shimmer velocity (chars/sec) for `Merry Working…` and `Thinking…`.
const SHIMMER_CHARS_PER_SEC: f32 = 46.0;
const SHIMMER_BAND_WIDTH: f32 = 6.5;
const SHIMMER_REST_CHARS: f32 = 4.0;
const SHIMMER_DIM_TARGET: Color = Color::Rgb(110, 106, 122);
const BRAND_FLOW_WAVELENGTH: f32 = 24.0;

/// Downward vertical pulse speed (rows/sec) for multi-row live Thinking rails.
/// Fast enough (~12.5 rows/sec) that 30fps luminance interpolation feels fluid
/// rather than stepping cell-by-cell.
const RAIL_ROWS_PER_SEC: f32 = 12.5;
const RAIL_LEAD_ROWS: f32 = 1.1;
const RAIL_TAIL_ROWS: f32 = 2.8;
const RAIL_REST_ROWS: f32 = 0.6;

/// Monotonic millisecond clock powering every animated visual cue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MotionClock {
    millis: u64,
}

impl MotionClock {
    pub(crate) fn now() -> Self {
        static ORIGIN: OnceLock<Instant> = OnceLock::new();
        let elapsed = ORIGIN.get_or_init(Instant::now).elapsed().as_millis();
        Self {
            millis: u64::try_from(elapsed).unwrap_or(u64::MAX),
        }
    }

    #[cfg(test)]
    pub(crate) const fn from_millis(millis: u64) -> Self {
        Self { millis }
    }

    pub(crate) const fn millis(self) -> u64 {
        self.millis
    }

    /// Fractional phase in `[0.0, 1.0)` computed with integer modulo first so
    /// large timestamps never lose precision when converted to `f32`.
    fn phase(self, period_ms: u64) -> f32 {
        let period = period_ms.max(1);
        ((self.millis % period) as f32) / (period as f32)
    }
}

/// Returns the active Braille spinner glyph for `elapsed`.
pub(crate) fn spinner_glyph(elapsed: Duration) -> &'static str {
    const FRAMES: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
    let index = (elapsed.as_millis() / 65) as usize % FRAMES.len();
    FRAMES[index]
}

/// Applies a crisp horizontal flowing spotlight across plain text at the shared
/// constant character velocity.
pub(crate) fn shimmer_text(
    text: &str,
    base_style: Style,
    highlight: Color,
    clock: MotionClock,
    enabled: bool,
) -> Vec<Span<'static>> {
    if !enabled || text.is_empty() {
        return vec![Span::styled(text.to_owned(), base_style)];
    }
    shimmer_styled_spans(
        vec![Span::styled(text.to_owned(), base_style)],
        highlight,
        clock,
        enabled,
    )
}

/// Sweeps a smooth horizontal spotlight across pre-styled spans at a constant
/// character velocity (`SHIMMER_CHARS_PER_SEC`), preserving each span's syntax
/// hue while lifting the traveling crest into luminous white.
pub(crate) fn shimmer_styled_spans(
    spans: Vec<Span<'static>>,
    highlight: Color,
    clock: MotionClock,
    enabled: bool,
) -> Vec<Span<'static>> {
    if !enabled {
        return spans;
    }
    let total_chars: usize = spans.iter().map(|span| span.content.chars().count()).sum();
    if total_chars == 0 {
        return spans;
    }

    let sweep_chars = (total_chars as f32) + SHIMMER_BAND_WIDTH * 2.0;
    let cycle_chars = sweep_chars + SHIMMER_REST_CHARS;
    let period_ms = ((cycle_chars / SHIMMER_CHARS_PER_SEC) * 1_000.0)
        .round()
        .max(1.0) as u64;
    let center = clock.phase(period_ms) * cycle_chars - SHIMMER_BAND_WIDTH;

    let mut out = Vec::with_capacity(total_chars.min(128));
    let mut char_index: usize = 0;

    for span in spans {
        let base_fg = span.style.fg.unwrap_or(Color::White);
        let resting_fg = blend_color(base_fg, SHIMMER_DIM_TARGET, 0.24);
        for ch in span.content.chars() {
            let intensity = shimmer_intensity(char_index as f32, center);
            let style = if intensity > 0.04 {
                let mut lifted =
                    span.style
                        .fg(blend_color(resting_fg, highlight, intensity * 0.92));
                if intensity > 0.45 {
                    lifted = lifted.add_modifier(Modifier::BOLD);
                }
                lifted
            } else {
                span.style.fg(resting_fg)
            };
            out.push(Span::styled(ch.to_string(), style));
            char_index += 1;
        }
    }

    merge_adjacent_spans(out)
}

/// Flows a repeating multi-stop color gradient smoothly left-to-right across
/// wrapped lines at `chars_per_sec`, maintaining continuous phase across line
/// breaks so long commands never stall or speed up abruptly.
pub(crate) fn flow_gradient_lines(
    lines: Vec<Line<'static>>,
    stops: &[Color],
    wavelength: f32,
    chars_per_sec: f32,
    dim_muted: Option<Color>,
    clock: MotionClock,
    enabled: bool,
) -> Vec<Line<'static>> {
    if !enabled || stops.is_empty() {
        return lines;
    }
    let mut offset = 0usize;
    lines
        .into_iter()
        .map(|line| {
            let line_chars: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
            let spans = flow_gradient_spans(
                line.spans,
                stops,
                wavelength,
                chars_per_sec,
                offset,
                dim_muted,
                clock,
                true,
            );
            offset = offset.saturating_add(line_chars);
            Line::from(spans)
        })
        .collect()
}

/// Applies a continuous repeating multi-stop horizontal wave to a slice of
/// spans starting at character offset `start_col`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn flow_gradient_spans(
    spans: Vec<Span<'static>>,
    stops: &[Color],
    wavelength: f32,
    chars_per_sec: f32,
    start_col: usize,
    dim_muted: Option<Color>,
    clock: MotionClock,
    enabled: bool,
) -> Vec<Span<'static>> {
    if !enabled || stops.is_empty() {
        return spans;
    }
    let wave = wavelength.max(1.0);
    let speed = chars_per_sec.max(1.0);
    let period_ms = ((wave / speed) * 1_000.0).round().max(1.0) as u64;
    let shift = clock.phase(period_ms);

    let total_chars: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let mut out = Vec::with_capacity(total_chars.min(128));
    let mut col = start_col;

    for span in spans {
        let is_muted = dim_muted.is_some() && span.style.fg == dim_muted;
        for ch in span.content.chars() {
            let phase = (col as f32 / wave) - shift;
            let wave_color = sample_cyclic_gradient(stops, phase);
            let fg = if let Some(muted) = dim_muted.filter(|_| is_muted) {
                blend_color(muted, wave_color, 0.25)
            } else {
                wave_color
            };
            let style = if ch.is_whitespace() {
                span.style
            } else {
                span.style.fg(fg)
            };
            out.push(Span::styled(ch.to_string(), style));
            col += 1;
        }
    }

    merge_adjacent_spans(out)
}

/// Computes the glyph and color for one cell of a downward-flowing vertical
/// Thinking rail.
///
/// Keeps a uniform thin hairline (`│`) across every row and animates purely
/// through color luminance (`base` → `ToolKeyword` sky-blue → `Focus` pink →
/// luminous white crest).
pub(crate) fn flow_rail_cell(
    row: u16,
    total_rows: u16,
    base: Color,
    accent: Color,
    trail: Color,
    clock: MotionClock,
    enabled: bool,
) -> (&'static str, Style) {
    if !enabled {
        return ("│", Style::default().fg(accent));
    }
    let span = f32::from(total_rows.max(2));
    let cycle_rows = span + RAIL_LEAD_ROWS + RAIL_TAIL_ROWS + RAIL_REST_ROWS;
    let period_ms = ((cycle_rows / RAIL_ROWS_PER_SEC) * 1_000.0)
        .round()
        .max(1.0) as u64;
    let head = clock.phase(period_ms) * cycle_rows - RAIL_LEAD_ROWS;
    // Positive `delta` means `head` has already passed `row` (trailing wake).
    let delta = head - f32::from(row);

    let intensity = if delta < 0.0 {
        let ahead = -delta;
        if ahead >= RAIL_LEAD_ROWS {
            0.0
        } else {
            let ratio = ahead / RAIL_LEAD_ROWS;
            let c = (ratio * std::f32::consts::FRAC_PI_2).cos();
            c * c
        }
    } else if delta < RAIL_TAIL_ROWS {
        let decay = 1.0 - (delta / RAIL_TAIL_ROWS);
        decay * decay
    } else {
        0.0
    };

    let color = if intensity >= 0.72 {
        let t = (intensity - 0.72) / 0.28;
        blend_color(accent, SHIMMER_HIGHLIGHT, 0.35 + t * 0.65)
    } else if intensity >= 0.25 {
        let t = (intensity - 0.25) / 0.47;
        blend_color(trail, accent, t)
    } else if intensity > 0.02 {
        blend_color(base, trail, intensity / 0.25)
    } else {
        base
    };

    ("│", Style::default().fg(color))
}

/// Renders Merry's idle brand mark (`Merry  `) with its signature left-to-right
/// Blue → Purple → Pink → White prism gradient.
pub(crate) fn brand_idle_spans(theme: &TuiTheme) -> Vec<Span<'static>> {
    let bg = theme.bg_color(SemanticColor::StatusBackground);
    let with_bg = |style: Style| bg.map_or(style, |color| style.bg(color));
    let mut spans = static_gradient_text(
        "Merry",
        MERRY_IDLE_STOPS,
        with_bg(Style::default().add_modifier(Modifier::BOLD)),
    );
    spans.push(Span::styled("  ", with_bg(Style::default())));
    spans
}

/// Renders the unified active brand + phase phrase (`Merry Working…`,
/// `Merry Thinking…`, `Merry Responding…`) with a continuous Blue → Purple →
/// Pink → White prism wave gliding left-to-right at the shared horizontal speed.
pub(crate) fn brand_status_spans(
    phase: &str,
    theme: &TuiTheme,
    clock: MotionClock,
    enabled: bool,
) -> Vec<Span<'static>> {
    let focus = theme
        .color(SemanticColor::Focus)
        .unwrap_or(Color::LightMagenta);
    let bg = theme.bg_color(SemanticColor::StatusBackground);
    let with_bg = |style: Style| bg.map_or(style, |color| style.bg(color));

    if !enabled {
        let mut spans = static_gradient_text(
            "Merry",
            MERRY_IDLE_STOPS,
            with_bg(Style::default().add_modifier(Modifier::BOLD)),
        );
        spans.push(Span::styled(" ", with_bg(Style::default())));
        spans.push(Span::styled(
            phase.to_owned(),
            with_bg(Style::default().fg(focus).add_modifier(Modifier::BOLD)),
        ));
        return spans;
    }

    let spans = vec![
        Span::styled(
            "Merry ".to_owned(),
            with_bg(Style::default().add_modifier(Modifier::BOLD)),
        ),
        Span::styled(
            phase.to_owned(),
            with_bg(Style::default().add_modifier(Modifier::BOLD)),
        ),
    ];
    flow_gradient_spans(
        spans,
        MERRY_PRISM_STOPS,
        BRAND_FLOW_WAVELENGTH,
        SHIMMER_CHARS_PER_SEC,
        0,
        None,
        clock,
        true,
    )
}

fn static_gradient_text(text: &str, stops: &[Color], base_style: Style) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let denom = chars.len().saturating_sub(1).max(1) as f32;
    let spans = chars
        .into_iter()
        .enumerate()
        .map(|(index, ch)| {
            let color = sample_linear_gradient(stops, index as f32 / denom);
            Span::styled(ch.to_string(), base_style.fg(color))
        })
        .collect();
    merge_adjacent_spans(spans)
}

fn sample_linear_gradient(stops: &[Color], t: f32) -> Color {
    if stops.is_empty() {
        return Color::White;
    }
    if stops.len() == 1 {
        return stops[0];
    }
    let clamped = t.clamp(0.0, 1.0);
    let scaled = clamped * ((stops.len() - 1) as f32);
    let index = (scaled.floor() as usize).min(stops.len() - 2);
    let frac = scaled - (index as f32);
    blend_color(stops[index], stops[index + 1], frac)
}

fn sample_cyclic_gradient(stops: &[Color], phase: f32) -> Color {
    if stops.is_empty() {
        return Color::White;
    }
    if stops.len() == 1 {
        return stops[0];
    }
    let wrapped = phase.rem_euclid(1.0);
    let scaled = wrapped * (stops.len() as f32);
    let index = (scaled.floor() as usize) % stops.len();
    let next = (index + 1) % stops.len();
    let frac = scaled - scaled.floor();
    let smooth = frac * frac * (3.0 - 2.0 * frac);
    blend_color(stops[index], stops[next], smooth)
}

fn shimmer_intensity(position: f32, center: f32) -> f32 {
    let distance = (position - center).abs();
    if distance >= SHIMMER_BAND_WIDTH {
        return 0.0;
    }
    let ratio = distance / SHIMMER_BAND_WIDTH;
    let cosine = (ratio * std::f32::consts::FRAC_PI_2).cos();
    cosine * cosine
}

fn merge_adjacent_spans(spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let mut merged: Vec<Span<'static>> = Vec::with_capacity(spans.len());
    for span in spans {
        if let Some(last) = merged.last_mut()
            && last.style == span.style
        {
            let mut combined = last.content.clone().into_owned();
            combined.push_str(span.content.as_ref());
            last.content = combined.into();
        } else {
            merged.push(span);
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shimmer_preserves_exact_text_and_varies_across_clock_ticks() {
        let base = Style::default().fg(Color::White);
        let a = shimmer_text(
            "cargo test --workspace",
            base,
            SHIMMER_HIGHLIGHT,
            MotionClock::from_millis(120),
            true,
        );
        let b = shimmer_text(
            "cargo test --workspace",
            base,
            SHIMMER_HIGHLIGHT,
            MotionClock::from_millis(450),
            true,
        );
        let plain_a: String = a.iter().map(|span| span.content.as_ref()).collect();
        let plain_b: String = b.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(plain_a, "cargo test --workspace");
        assert_eq!(plain_b, "cargo test --workspace");
        assert_ne!(a, b);
    }

    #[test]
    fn large_timestamps_keep_subsecond_color_precision_without_f32_stalling() {
        let epoch_ms = 1_795_188_691_681_u64;

        let shimmer_frames: Vec<_> = (0..10)
            .map(|step| {
                shimmer_text(
                    "Running sleep 60",
                    Style::default().fg(Color::LightCyan),
                    SHIMMER_HIGHLIGHT,
                    MotionClock::from_millis(epoch_ms + step * 120),
                    true,
                )
            })
            .collect();
        assert!(
            shimmer_frames
                .iter()
                .any(|frame| frame != &shimmer_frames[0]),
            "shimmer color must advance smoothly even on large u64 clocks"
        );

        let rail_states: Vec<_> = (0..10)
            .map(|step| {
                flow_rail_cell(
                    1,
                    4,
                    Color::DarkGray,
                    Color::LightMagenta,
                    Color::LightCyan,
                    MotionClock::from_millis(epoch_ms + step * 120),
                    true,
                )
            })
            .collect();
        assert!(
            rail_states.iter().all(|(sym, _)| *sym == "│")
                && rail_states
                    .iter()
                    .any(|(_, style)| *style != rail_states[0].1),
            "vertical rail should stay uniform thin │ while animating color smoothly: {rail_states:?}"
        );

        let theme = TuiTheme::default();
        let brand_frames: Vec<_> = (0..10)
            .map(|step| {
                brand_status_spans(
                    "Working…",
                    &theme,
                    MotionClock::from_millis(epoch_ms + step * 120),
                    true,
                )
            })
            .collect();
        assert!(
            brand_frames.iter().any(|frame| frame != &brand_frames[0]),
            "brand Merry Working… shimmer must advance smoothly even on large u64 clocks"
        );
    }

    #[test]
    fn disabled_animations_return_static_spans() {
        let base = Style::default().fg(Color::LightCyan);
        let spans = shimmer_text(
            "Thinking…",
            base,
            SHIMMER_HIGHLIGHT,
            MotionClock::from_millis(400),
            false,
        );
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].style, base);
    }

    #[test]
    fn brand_status_joins_merry_and_phase_into_one_phrase() {
        let theme = TuiTheme::default();
        let idle = brand_idle_spans(&theme);
        let idle_text: String = idle.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(idle_text, "Merry  ");
        assert_eq!(
            idle.first().and_then(|s| s.style.fg),
            Some(MERRY_IDLE_STOPS[0])
        );
        assert_eq!(
            idle.iter()
                .find(|s| s.content.as_ref() == "y")
                .and_then(|s| s.style.fg),
            Some(*MERRY_IDLE_STOPS.last().unwrap())
        );

        let active_a = brand_status_spans("Working…", &theme, MotionClock::from_millis(120), true);
        let active_b = brand_status_spans("Working…", &theme, MotionClock::from_millis(320), true);
        let active_text: String = active_a.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(active_text, "Merry Working…");
        assert_ne!(active_a, active_b);
    }

    #[test]
    fn running_gradient_flow_advances_across_wrapped_lines() {
        let lines = vec![
            Line::from(vec![
                Span::styled("⠋ Running ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw("echo long-command-part-one"),
            ]),
            Line::from(vec![
                Span::raw("          "),
                Span::raw("part-two"),
                Span::styled("  12.0s", Style::default().fg(Color::DarkGray)),
            ]),
        ];
        let a = flow_gradient_lines(
            lines.clone(),
            RUNNING_FLOW_STOPS,
            RUNNING_FLOW_WAVELENGTH,
            RUNNING_FLOW_CHARS_PER_SEC,
            Some(Color::DarkGray),
            MotionClock::from_millis(100),
            true,
        );
        let b = flow_gradient_lines(
            lines,
            RUNNING_FLOW_STOPS,
            RUNNING_FLOW_WAVELENGTH,
            RUNNING_FLOW_CHARS_PER_SEC,
            Some(Color::DarkGray),
            MotionClock::from_millis(260),
            true,
        );
        assert_ne!(a, b);
        let text_a: String = a
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect();
        assert!(text_a.contains("Running echo long-command-part-one"));
        assert!(text_a.contains("part-two  12.0s"));
    }
}
