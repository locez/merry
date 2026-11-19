//! Structured renderer and pointer hit-testing for the permission review modal.

use super::{
    click_target::{OverlayPointer, chip_hit},
    overlay::PermissionReviewOverlay,
    overlay_render::{centered_rect, code_surface_style, inset, semantic_style},
    state::TuiState,
    theme::SemanticColor,
};
use crate::permission_prompt::{
    PermissionCapabilityBadge, PermissionFallbackNotice, PermissionReviewView,
};
use merry_runtime::{PathAccess, PermissionReviewRisk};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const MAX_PERMISSION_OVERLAY_WIDTH: u16 = 82;
const PERMISSION_CHIPS: [&str; 2] = ["Allow", "Don't allow"];

struct PermissionSplit {
    region: Rect,
    body: Rect,
    footer: Rect,
}

pub(crate) fn render_permission_review(
    frame: &mut Frame<'_>,
    state: &TuiState,
    review: &PermissionReviewOverlay,
) {
    let area = frame.area();
    let view = review.view();
    let split = permission_split(area, state, view);
    let region = split.region;

    frame.render_widget(Clear, region);

    let border_slot = border_color_for_view(view);
    let mut block = Block::default()
        .title(Line::from(vec![
            Span::styled(
                " M ",
                semantic_style(state, SemanticColor::Status).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " Allow this command? ",
                semantic_style(state, border_slot).add_modifier(Modifier::BOLD),
            ),
        ]))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(semantic_style(state, border_slot).add_modifier(Modifier::BOLD))
        .style(code_surface_style(state));

    if let Some(right_title) = title_badges_line(state, view) {
        block = block.title(right_title.alignment(Alignment::Right));
    }
    frame.render_widget(block, region);

    let body_width = usize::from(split.body.width).max(1);
    let lines = build_body_lines(state, view, body_width);
    let total_lines = lines.len();
    let visible_lines = usize::from(split.body.height).max(1);
    let max_scroll = total_lines.saturating_sub(visible_lines);
    let scroll = usize::from(review.scroll()).min(max_scroll);

    frame.render_widget(
        Paragraph::new(lines).scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
        split.body,
    );

    render_footer(frame, state, split.footer, max_scroll > 0);
}

pub(crate) fn permission_review_pointer(
    state: &TuiState,
    area: Rect,
    review: &PermissionReviewOverlay,
    position: Position,
) -> Option<OverlayPointer> {
    let footer = permission_split(area, state, review.view()).footer;
    let chip_row = footer.bottom().saturating_sub(1);
    match chip_hit(
        Position::new(footer.x, chip_row),
        &PERMISSION_CHIPS,
        position,
    ) {
        Some(0) => Some(OverlayPointer::PermissionAllow),
        Some(1) => Some(OverlayPointer::PermissionReject),
        _ => None,
    }
}

fn permission_split(area: Rect, state: &TuiState, view: &PermissionReviewView) -> PermissionSplit {
    let desired_width = area
        .width
        .saturating_sub(4)
        .min(MAX_PERMISSION_OVERLAY_WIDTH)
        .max(area.width.min(24));
    let content_width = usize::from(desired_width.saturating_sub(4)).max(1);
    let body_lines = build_body_lines(state, view, content_width).len();

    let available_height = area.height.saturating_sub(2).max(6);
    let footer_height = if available_height >= 8 { 2 } else { 1 };
    let max_height = available_height.min(30);
    let min_height = available_height.min(10);
    let desired_height = u16::try_from(body_lines.saturating_add(usize::from(footer_height) + 4))
        .unwrap_or(max_height)
        .clamp(min_height, max_height);

    let region = centered_rect(area, desired_width, desired_height);
    let inner = inset(region, 2, 1);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(footer_height)])
        .split(inner);
    PermissionSplit {
        region,
        body: rows[0],
        footer: rows[1],
    }
}

fn border_color_for_view(view: &PermissionReviewView) -> SemanticColor {
    match view.fallback {
        Some(PermissionFallbackNotice::ModelDenied {
            risk: PermissionReviewRisk::Critical | PermissionReviewRisk::High,
            ..
        }) => SemanticColor::Error,
        _ => SemanticColor::Warning,
    }
}

fn title_badges_line(state: &TuiState, view: &PermissionReviewView) -> Option<Line<'static>> {
    let mut spans = Vec::new();
    if let Some(PermissionFallbackNotice::ModelDenied { risk, .. }) = &view.fallback {
        let (label, slot) = risk_badge(*risk);
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("[{label}]"),
            semantic_style(state, slot).add_modifier(Modifier::BOLD),
        ));
    }
    if view.queue_total > 1 {
        if !spans.is_empty() {
            spans.push(Span::styled(
                " · ",
                semantic_style(state, SemanticColor::Muted),
            ));
        } else {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            format!("1/{}", view.queue_total),
            semantic_style(state, SemanticColor::Muted).add_modifier(Modifier::BOLD),
        ));
    }
    if spans.is_empty() {
        return None;
    }
    spans.push(Span::raw(" "));
    Some(Line::from(spans))
}

fn render_footer(frame: &mut Frame<'_>, state: &TuiState, footer: Rect, scrollable: bool) {
    if footer.width == 0 || footer.height == 0 {
        return;
    }
    let chip_row = footer.bottom().saturating_sub(1);
    if footer.height >= 2 {
        let divider = "─".repeat(usize::from(footer.width));
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                divider,
                semantic_style(state, SemanticColor::Muted).add_modifier(Modifier::DIM),
            ))),
            Rect::new(footer.x, footer.y, footer.width, 1),
        );
    }

    let allow_style = semantic_style(state, SemanticColor::Success)
        .add_modifier(Modifier::BOLD)
        .add_modifier(Modifier::UNDERLINED);
    let reject_style = semantic_style(state, SemanticColor::Error)
        .add_modifier(Modifier::BOLD)
        .add_modifier(Modifier::UNDERLINED);
    let mut spans = vec![
        Span::styled(format!("[{}]", PERMISSION_CHIPS[0]), allow_style),
        Span::raw("  "),
        Span::styled(format!("[{}]", PERMISSION_CHIPS[1]), reject_style),
    ];

    let chips_width = UnicodeWidthStr::width("[Allow]  [Don't allow]");
    let hint = if scrollable {
        "Enter/Y allow · Esc/N deny · ↑↓ scroll"
    } else {
        "Enter/Y allow · Esc/N deny"
    };
    let short_hint = "Y/N";
    let avail = usize::from(footer.width);
    let chosen_hint = if chips_width + 2 + UnicodeWidthStr::width(hint) <= avail {
        Some(hint)
    } else if chips_width + 2 + UnicodeWidthStr::width(short_hint) <= avail {
        Some(short_hint)
    } else {
        None
    };
    if let Some(hint_text) = chosen_hint {
        let pad = avail.saturating_sub(chips_width + UnicodeWidthStr::width(hint_text));
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(
            hint_text,
            semantic_style(state, SemanticColor::Muted),
        ));
    }

    frame.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect::new(footer.x, chip_row, footer.width, 1),
    );
}

fn build_body_lines(
    state: &TuiState,
    view: &PermissionReviewView,
    width: usize,
) -> Vec<Line<'static>> {
    if let Some(plain_body) = &view.plain_body {
        let text_style = semantic_style(state, SemanticColor::Assistant);
        return wrap_plain_text(plain_body, width)
            .into_iter()
            .map(|line| Line::from(Span::styled(line, text_style)))
            .collect();
    }

    let mut lines = Vec::new();
    let label_style = semantic_style(state, SemanticColor::Muted).add_modifier(Modifier::BOLD);
    let muted_style = semantic_style(state, SemanticColor::Muted);
    let text_style = semantic_style(state, SemanticColor::Assistant);
    let cmd_style = semantic_style(state, SemanticColor::Command).add_modifier(Modifier::BOLD);
    let prompt_style =
        semantic_style(state, SemanticColor::ToolKeyword).add_modifier(Modifier::BOLD);

    // 1. COMMAND + CWD
    lines.push(Line::from(Span::styled("COMMAND", label_style)));
    let cmd_width = width.saturating_sub(2).max(1);
    for (index, wrapped) in wrap_plain_text(&view.command, cmd_width)
        .into_iter()
        .enumerate()
    {
        if index == 0 {
            lines.push(Line::from(vec![
                Span::styled("$ ", prompt_style),
                Span::styled(wrapped, cmd_style),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(wrapped, cmd_style),
            ]));
        }
    }
    lines.push(Line::from(vec![
        Span::styled("cwd  ", muted_style),
        Span::styled(view.cwd.clone(), text_style),
    ]));

    // 2. CAPABILITIES (pills)
    if !view.capabilities.is_empty() || view.review_only {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled("CAPABILITIES", label_style)));
        let pills: Vec<(String, Style)> = if !view.capabilities.is_empty() {
            view.capabilities
                .iter()
                .map(|cap| {
                    (
                        format!("[{}]", cap.chip_label()),
                        capability_style(state, cap),
                    )
                })
                .collect()
        } else {
            vec![(
                "[action review]".to_owned(),
                semantic_style(state, SemanticColor::Warning).add_modifier(Modifier::BOLD),
            )]
        };
        push_wrapped_pills(&mut lines, pills, width);
    }

    // 3. AI REVIEW / FALLBACK
    if let Some(fallback) = &view.fallback {
        lines.push(Line::default());
        match fallback {
            PermissionFallbackNotice::ModelDenied {
                risk,
                user_authorization,
                rationale,
            } => {
                let (risk_label, risk_slot) = risk_badge(*risk);
                lines.push(Line::from(vec![
                    Span::styled("AI REVIEW", label_style),
                    Span::raw("  "),
                    Span::styled(
                        format!("[{risk_label}]"),
                        semantic_style(state, risk_slot).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  auth: {}", user_authorization.as_str()),
                        muted_style,
                    ),
                ]));
                for line in wrap_plain_text(rationale, width) {
                    lines.push(Line::from(Span::styled(line, text_style)));
                }
            }
            PermissionFallbackNotice::ReviewFailed { message } => {
                lines.push(Line::from(Span::styled(
                    "AI REVIEW UNAVAILABLE",
                    semantic_style(state, SemanticColor::Warning).add_modifier(Modifier::BOLD),
                )));
                for line in wrap_plain_text(message, width) {
                    lines.push(Line::from(Span::styled(line, text_style)));
                }
            }
            PermissionFallbackNotice::ReviewModelUnavailable => {
                lines.push(Line::from(Span::styled(
                    "AI REVIEW UNAVAILABLE",
                    semantic_style(state, SemanticColor::Warning).add_modifier(Modifier::BOLD),
                )));
                for line in wrap_plain_text("No approval-review model is configured.", width) {
                    lines.push(Line::from(Span::styled(line, text_style)));
                }
            }
        }
    }

    // 4. AGENT REASON / ACTION NOTE
    if let Some(reason) = &view.agent_reason {
        lines.push(Line::default());
        let heading = if view.review_only { "NOTE" } else { "REASON" };
        lines.push(Line::from(Span::styled(heading, label_style)));
        for line in wrap_plain_text(reason, width) {
            lines.push(Line::from(Span::styled(line, text_style)));
        }
    }

    lines
}

fn capability_style(state: &TuiState, cap: &PermissionCapabilityBadge) -> Style {
    let slot = match cap {
        PermissionCapabilityBadge::Network => SemanticColor::Warning,
        PermissionCapabilityBadge::Path {
            access: PathAccess::ReadOnly,
            ..
        } => SemanticColor::ToolKeyword,
        PermissionCapabilityBadge::Path { .. } => SemanticColor::Warning,
        PermissionCapabilityBadge::HostIntegration(_) => SemanticColor::Risk,
    };
    semantic_style(state, slot).add_modifier(Modifier::BOLD)
}

fn risk_badge(risk: PermissionReviewRisk) -> (&'static str, SemanticColor) {
    match risk {
        PermissionReviewRisk::Low => ("LOW RISK", SemanticColor::Success),
        PermissionReviewRisk::Medium => ("MEDIUM RISK", SemanticColor::Warning),
        PermissionReviewRisk::High => ("HIGH RISK", SemanticColor::Error),
        PermissionReviewRisk::Critical => ("CRITICAL", SemanticColor::Error),
        PermissionReviewRisk::Unknown => ("UNKNOWN RISK", SemanticColor::Muted),
    }
}

fn push_wrapped_pills(
    lines: &mut Vec<Line<'static>>,
    pills: Vec<(String, Style)>,
    max_width: usize,
) {
    let mut row_spans = Vec::new();
    let mut row_width = 0usize;
    for (text, style) in pills {
        let item_width = UnicodeWidthStr::width(text.as_str());
        let sep_width = if row_spans.is_empty() { 0 } else { 2 };
        if !row_spans.is_empty() && row_width + sep_width + item_width > max_width {
            lines.push(Line::from(std::mem::take(&mut row_spans)));
            row_width = 0;
        }
        if !row_spans.is_empty() {
            row_spans.push(Span::raw("  "));
            row_width += 2;
        }
        row_spans.push(Span::styled(text, style));
        row_width += item_width;
    }
    if !row_spans.is_empty() {
        lines.push(Line::from(row_spans));
    }
}

fn wrap_plain_text(text: &str, max_width: usize) -> Vec<String> {
    let max_width = max_width.max(1);
    let mut output = Vec::new();
    for raw_line in text.split('\n') {
        if raw_line.is_empty() {
            output.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut current_width = 0usize;
        for ch in raw_line.chars() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if current_width > 0 && current_width + ch_width > max_width {
                output.push(std::mem::take(&mut current));
                current_width = 0;
            }
            current.push(ch);
            current_width += ch_width;
        }
        if !current.is_empty() {
            output.push(current);
        }
    }
    if output.is_empty() {
        output.push(String::new());
    }
    output
}
