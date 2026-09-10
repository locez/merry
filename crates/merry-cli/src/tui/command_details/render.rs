use super::{CapturedOutput, CommandDetails, DetailsOutput, output::CapturedStream};
use crate::tui::{
    keymap::KeyAction,
    overlay::Overlay,
    overlay_render::render_surface,
    state::{CommandFailure, CommandView, TuiState},
    text_wrap::{StyledTextPart, semantic_style, wrap_styled_parts_preserving_leading_whitespace},
    theme::SemanticColor,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};
use std::borrow::Cow;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DetailsLayout {
    width: u16,
    lines: Vec<Line<'static>>,
}

fn regions(
    area: Rect,
    state: &TuiState,
    details: &CommandDetails,
) -> (Rect, Rect, Rect, Paragraph<'static>) {
    let width = area.width.saturating_sub(2).min(120);
    let height = area.height.saturating_sub(2);
    let region = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    let inner = region.inner(ratatui::layout::Margin::new(1, 1));
    let hints = footer_hints(state, details, inner.width);
    let footer_height = u16::try_from(hints.line_count(inner.width))
        .unwrap_or(u16::MAX)
        .min(inner.height.saturating_sub(1));
    let body = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(footer_height),
    );
    let footer = Rect::new(inner.x, body.bottom(), inner.width, footer_height);
    (region, body, footer, hints)
}

pub(crate) fn prepare_viewport(state: &mut TuiState, area: Rect) {
    let Some(Overlay::CommandDetails(details)) = state.overlay() else {
        return;
    };
    let (_, body, _, _) = regions(area, state, details);
    let layout = details
        .layout
        .as_ref()
        .is_none_or(|layout| layout.width != body.width)
        .then(|| DetailsLayout {
            width: body.width,
            lines: content_lines(state, details, body.width),
        });
    if let Some(Overlay::CommandDetails(details)) = state.overlay_mut() {
        if let Some(layout) = layout {
            details.layout = Some(layout);
        }
        if let Some(layout) = &details.layout {
            details.scroll = details
                .scroll
                .min(layout.lines.len().saturating_sub(usize::from(body.height)));
        }
    }
}

pub(crate) fn render_details(frame: &mut Frame<'_>, state: &TuiState, details: &CommandDetails) {
    let (region, body, footer, hints) = regions(frame.area(), state, details);
    if body.is_empty() {
        return;
    }
    render_surface(
        frame,
        state,
        region,
        &format!(" Command {}/{} ", details.ordinal, details.total),
    );
    let lines = details
        .layout
        .as_ref()
        .filter(|layout| layout.width == body.width)
        .map_or_else(
            || Cow::Owned(content_lines(state, details, body.width)),
            |layout| Cow::Borrowed(layout.lines.as_slice()),
        );
    let max_scroll = lines.len().saturating_sub(usize::from(body.height));
    let visible = lines
        .iter()
        .skip(details.scroll.min(max_scroll))
        .take(usize::from(body.height))
        .cloned()
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(visible), body);
    frame.render_widget(hints, footer);
}

fn footer_hints(state: &TuiState, details: &CommandDetails, width: u16) -> Paragraph<'static> {
    let close = state
        .keymap()
        .binding_label_for(KeyAction::OpenCommandDetails)
        .map_or_else(
            || "Esc close".to_owned(),
            |key| format!("{key} / Esc close"),
        );
    let (navigation, copy) = if width < 50 {
        ("←/→ cmd ↑/↓ scroll", "C/Y copy")
    } else {
        (
            "←/→ command · ↑/↓/PgUp/PgDn scroll",
            "C copy command · Y copy output",
        )
    };
    let hints = vec![
        Line::styled(navigation, semantic_style(state, SemanticColor::Muted)),
        Line::styled(
            format!("{close} · {copy}"),
            semantic_style(state, SemanticColor::Muted),
        ),
        Line::styled(
            details.feedback.clone().unwrap_or_default(),
            semantic_style(state, SemanticColor::Warning),
        ),
    ];
    Paragraph::new(hints).wrap(Wrap { trim: false })
}

fn content_lines(state: &TuiState, details: &CommandDetails, width: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let CommandView::Finished {
        detail,
        command,
        cwd,
        exit_code,
        failure,
        elapsed,
        ..
    } = &details.view
    {
        append_text(
            &mut lines,
            state,
            if command.is_empty() { detail } else { command },
            width,
            SemanticColor::Assistant,
        );
        append_text(
            &mut lines,
            state,
            &format!("Directory: {cwd}"),
            width,
            SemanticColor::Muted,
        );
        let mut status = match failure {
            Some(CommandFailure::Cancelled) => "Cancelled".to_owned(),
            Some(CommandFailure::Failed) => "Failed".to_owned(),
            None => exit_code
                .map(|code| format!("Exit: {code}"))
                .unwrap_or_else(|| "Exit: unknown".into()),
        };
        if let Some(elapsed) = elapsed {
            status.push_str(&format!(
                " · Elapsed: {:.1}s (including waiting)",
                elapsed.as_secs_f64()
            ));
        }
        let color = if exit_code.is_some_and(|code| code != 0)
            || *failure == Some(CommandFailure::Failed)
        {
            SemanticColor::Error
        } else {
            SemanticColor::Muted
        };
        append_text(&mut lines, state, &status, width, color);
        lines.push(Line::default());
    }
    match &details.output {
        DetailsOutput::Loading => append_text(
            &mut lines,
            state,
            "Loading captured output…",
            width,
            SemanticColor::Muted,
        ),
        DetailsOutput::Failed(error) => append_text(
            &mut lines,
            state,
            &format!("Could not load output: {error}"),
            width,
            SemanticColor::Error,
        ),
        DetailsOutput::Ready(CapturedOutput::Text(text)) => {
            append_text(&mut lines, state, text, width, SemanticColor::Assistant)
        }
        DetailsOutput::Ready(CapturedOutput::Process { stdout, stderr }) => {
            append_stream(&mut lines, state, "stdout", stdout, width);
            lines.push(Line::default());
            append_stream(&mut lines, state, "stderr", stderr, width);
        }
    }
    lines
}

fn append_stream(
    lines: &mut Vec<Line<'static>>,
    state: &TuiState,
    label: &str,
    stream: &CapturedStream,
    width: u16,
) {
    lines.push(Line::from(Span::styled(
        label.to_owned(),
        semantic_style(state, SemanticColor::Muted).add_modifier(Modifier::BOLD),
    )));
    if stream.text.is_empty() {
        append_text(lines, state, "(empty)", width, SemanticColor::Muted);
    } else {
        append_text(lines, state, &stream.text, width, SemanticColor::Assistant);
    }
    if stream.truncated {
        append_text(
            lines,
            state,
            "… capture truncated; only captured output is available",
            width,
            SemanticColor::Warning,
        );
    }
    if stream.utf8 == Some(false) {
        append_text(
            lines,
            state,
            "Non-UTF-8 output is displayed as text; the artifact retains the original bytes.",
            width,
            SemanticColor::Warning,
        );
    }
}

fn append_text(
    lines: &mut Vec<Line<'static>>,
    state: &TuiState,
    text: &str,
    width: u16,
    color: SemanticColor,
) {
    for line in text.split('\n') {
        let clean = line
            .chars()
            .filter(|character| !character.is_control() || *character == '\t')
            .collect::<String>()
            .replace('\t', "    ");
        if clean.is_empty() {
            lines.push(Line::default());
        } else {
            lines.extend(wrap_styled_parts_preserving_leading_whitespace(
                vec![StyledTextPart {
                    text: clean,
                    style: semantic_style(state, color),
                    atomic: false,
                }],
                width,
            ));
        }
    }
}
