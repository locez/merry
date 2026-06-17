use super::{
    state::{PatchChangeView, PatchLineView, TimelineItem, TuiState},
    theme::SemanticColor,
};
use merry_core::QueuedInputLane;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};

const QUEUE_PREVIEW_HEIGHT: u16 = 5;

#[allow(dead_code)]
pub(crate) fn render(frame: &mut Frame<'_>, state: &TuiState) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(QUEUE_PREVIEW_HEIGHT),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(frame.area());
    let input_inner = bordered_inner(root[2]);
    let input_viewport = state.input_viewport(usize::from(input_inner.width));

    frame.render_widget(
        Paragraph::new(timeline_lines(state, root[0].height))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_type(BorderType::Plain)
                    .border_style(semantic_style(state, SemanticColor::Muted))
                    .title_style(semantic_style(state, SemanticColor::Muted)),
            ),
        root[0],
    );
    frame.render_widget(
        Paragraph::new(queue_lines(state, root[1])).block(
            Block::default()
                .title("queue")
                .border_style(semantic_style(state, SemanticColor::Muted)),
        ),
        root[1],
    );
    frame.render_widget(
        Paragraph::new(input_viewport.text)
            .style(semantic_style(state, SemanticColor::Focus))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Plain)
                    .title("input")
                    .border_style(semantic_style(state, SemanticColor::Focus))
                    .title_style(semantic_style(state, SemanticColor::Focus)),
            ),
        root[2],
    );
    set_input_cursor(frame, input_inner, input_viewport.cursor_column);
    frame.render_widget(
        Paragraph::new(state.status_text()).style(semantic_style(state, SemanticColor::Status)),
        root[3],
    );
}

#[cfg(test)]
pub(crate) fn render_to_text(state: &TuiState, width: u16, height: u16) -> String {
    let buffer = render_to_buffer(state, width, height);
    let area = buffer.area;
    let mut text = String::new();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

#[cfg(test)]
pub(crate) fn render_to_buffer(
    state: &TuiState,
    width: u16,
    height: u16,
) -> ratatui::buffer::Buffer {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal should build");
    terminal
        .draw(|frame| render(frame, state))
        .expect("test render should draw");

    terminal.backend().buffer().clone()
}

#[cfg(test)]
pub(crate) fn render_to_buffer_and_cursor(
    state: &TuiState,
    width: u16,
    height: u16,
) -> (ratatui::buffer::Buffer, Position) {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal should build");
    terminal
        .draw(|frame| render(frame, state))
        .expect("test render should draw");

    (
        terminal.backend().buffer().clone(),
        terminal.backend().cursor_position(),
    )
}

fn bordered_inner(region: Rect) -> Rect {
    Rect {
        x: region.x.saturating_add(1),
        y: region.y.saturating_add(1),
        width: region.width.saturating_sub(2),
        height: region.height.saturating_sub(2),
    }
}

fn set_input_cursor(frame: &mut Frame<'_>, region: Rect, cursor_column: usize) {
    if region.width == 0 || region.height == 0 {
        return;
    }
    let cursor_column = u16::try_from(cursor_column).unwrap_or(u16::MAX);
    let max_x = region.x.saturating_add(region.width.saturating_sub(1));
    frame.set_cursor_position(Position {
        x: region.x.saturating_add(cursor_column).min(max_x),
        y: region.y,
    });
}

fn timeline_lines(state: &TuiState, region_height: u16) -> Vec<Line<'static>> {
    let lines = state
        .timeline()
        .iter()
        .flat_map(|item| match item {
            TimelineItem::User { text, lane } => user_lines(state, text, *lane),
            TimelineItem::Assistant { text } => vec![inline_code_line(
                state,
                text,
                semantic_style(state, SemanticColor::Focus),
            )],
            TimelineItem::Muted { title, detail } => {
                let mut spans = vec![
                    Span::styled(title.clone(), semantic_style(state, SemanticColor::Muted)),
                    Span::styled(": ", semantic_style(state, SemanticColor::Muted)),
                ];
                spans.extend(inline_code_spans(
                    state,
                    detail,
                    semantic_style(state, SemanticColor::Muted),
                ));
                vec![Line::from(spans)]
            }
            TimelineItem::Expanded { title, body } | TimelineItem::Diagnostic { title, body } => {
                let title_slot = match item {
                    TimelineItem::Diagnostic { .. } => SemanticColor::Error,
                    _ => SemanticColor::Focus,
                };
                let mut lines = vec![Line::from(Span::styled(
                    title.clone(),
                    semantic_style(state, title_slot),
                ))];
                lines.extend(body.lines().map(|line| {
                    inline_code_line(state, line, timeline_body_style(state, item, line))
                }));
                lines
            }
            TimelineItem::Patch { changes } => patch_lines(state, changes),
        })
        .collect::<Vec<_>>();
    let visible_height = usize::from(region_height).saturating_sub(1).max(1);
    let end = lines
        .len()
        .saturating_sub(state.timeline_scroll_offset())
        .max(visible_height)
        .min(lines.len());
    let start = end.saturating_sub(visible_height);
    lines.into_iter().skip(start).take(end - start).collect()
}

fn user_lines(state: &TuiState, text: &str, lane: QueuedInputLane) -> Vec<Line<'static>> {
    let label = match lane {
        QueuedInputLane::Next => "user",
        QueuedInputLane::Suspended => "user suspended",
        QueuedInputLane::Backlog => "user backlog",
    };
    let mut spans = vec![
        Span::styled(
            label.to_owned(),
            semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
        ),
        Span::styled(": ", semantic_style(state, SemanticColor::Muted)),
    ];
    spans.extend(inline_code_spans(
        state,
        text,
        semantic_style(state, SemanticColor::Focus),
    ));
    vec![Line::from(spans)]
}

fn patch_lines(state: &TuiState, changes: &[PatchChangeView]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for change in changes {
        lines.push(Line::from(Span::styled(
            format!(
                "Edited {} (+{} -{})",
                change.path, change.added, change.removed
            ),
            semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            format!(
                "  {} hunk(s), {} -> {} bytes",
                change.hunks,
                change
                    .bytes_before
                    .map_or_else(|| "-".to_owned(), |bytes| bytes.to_string()),
                change
                    .bytes_after
                    .map_or_else(|| "-".to_owned(), |bytes| bytes.to_string())
            ),
            semantic_style(state, SemanticColor::Muted),
        )));
        for line in &change.lines {
            lines.push(patch_line(state, line));
        }
    }
    lines
}

fn patch_line(state: &TuiState, line: &PatchLineView) -> Line<'static> {
    let (marker, style) = match line.kind {
        super::state::PatchLineKind::Context => (
            " ",
            semantic_style(state, SemanticColor::Focus).bg(Color::Reset),
        ),
        super::state::PatchLineKind::Add => ("+", diff_line_style(state, SemanticColor::DiffAdd)),
        super::state::PatchLineKind::Remove => {
            ("-", diff_line_style(state, SemanticColor::DiffDelete))
        }
    };

    Line::from(vec![
        Span::styled(
            format_line_number(patch_display_line(line)),
            patch_gutter_style(state),
        ),
        Span::styled(" ", patch_gutter_style(state)),
        Span::styled(marker.to_owned(), style),
        Span::styled(line.text.clone(), style),
    ])
}

fn patch_display_line(line: &PatchLineView) -> Option<usize> {
    match line.kind {
        super::state::PatchLineKind::Context | super::state::PatchLineKind::Add => line.new_line,
        super::state::PatchLineKind::Remove => line.old_line,
    }
}

fn format_line_number(line: Option<usize>) -> String {
    line.map_or_else(|| "    ".to_owned(), |line| format!("{line:>4}"))
}

fn patch_gutter_style(state: &TuiState) -> Style {
    semantic_style(state, SemanticColor::Muted)
}

fn diff_line_style(state: &TuiState, slot: SemanticColor) -> Style {
    let foreground = state.theme().color(slot);
    let background = state.theme().color(slot).map(dim_color);
    match (foreground, background) {
        (Some(foreground), Some(background)) => Style::default().fg(foreground).bg(background),
        (Some(foreground), None) => Style::default().fg(foreground),
        (None, Some(background)) => Style::default().bg(background),
        (None, None) => Style::default(),
    }
}

fn dim_color(color: Color) -> Color {
    match color {
        Color::Black => Color::Black,
        Color::Red => Color::Rgb(45, 16, 24),
        Color::Green => Color::Rgb(18, 42, 28),
        Color::Yellow => Color::Rgb(48, 40, 18),
        Color::Blue => Color::Rgb(18, 30, 48),
        Color::Magenta => Color::Rgb(42, 20, 44),
        Color::Cyan => Color::Rgb(16, 42, 45),
        Color::LightRed => Color::Rgb(58, 20, 28),
        Color::LightGreen => Color::Rgb(22, 54, 34),
        Color::LightYellow => Color::Rgb(62, 52, 22),
        Color::LightBlue => Color::Rgb(22, 38, 62),
        Color::LightMagenta => Color::Rgb(54, 26, 58),
        Color::LightCyan => Color::Rgb(20, 54, 58),
        Color::Gray => Color::DarkGray,
        Color::DarkGray => Color::Black,
        Color::White => Color::DarkGray,
        Color::Rgb(red, green, blue) => Color::Rgb(red / 4, green / 4, blue / 4),
        Color::Indexed(index) => Color::Indexed(index),
        Color::Reset => Color::Reset,
    }
}

fn queue_lines(state: &TuiState, region: Rect) -> Vec<Line<'static>> {
    let queue = state.queue_preview();
    vec![
        queue_lane(state, "Next", &queue.next, region.width),
        queue_lane(state, "Suspended", &queue.suspended, region.width),
        queue_lane(state, "Backlog", &queue.backlog, region.width),
    ]
}

fn queue_lane(
    state: &TuiState,
    label: &'static str,
    items: &[super::state::QueuePreviewItem],
    region_width: u16,
) -> Line<'static> {
    let label_text = format!("{label:<10} ");
    let content_width = usize::from(region_width).saturating_sub(label_text.chars().count());
    let content = if items.is_empty() {
        "--".to_owned()
    } else {
        items
            .iter()
            .enumerate()
            .map(|(index, item)| format!("{}. {}", index + 1, item.text))
            .collect::<Vec<_>>()
            .join(" | ")
    };
    Line::from(vec![
        Span::styled(
            label_text,
            semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            truncate_chars(&content, content_width),
            semantic_style(state, SemanticColor::Muted),
        ),
    ])
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    text.chars().take(max_chars - 3).collect::<String>() + "..."
}

fn inline_code_line(state: &TuiState, text: &str, base_style: Style) -> Line<'static> {
    Line::from(inline_code_spans(state, text, base_style))
}

fn inline_code_spans(state: &TuiState, text: &str, base_style: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut remainder = text;
    loop {
        let Some(start) = remainder.find('`') else {
            if !remainder.is_empty() {
                spans.push(Span::styled(remainder.to_owned(), base_style));
            }
            return spans;
        };
        let before = &remainder[..start];
        if !before.is_empty() {
            spans.push(Span::styled(before.to_owned(), base_style));
        }
        let after_start = &remainder[start + 1..];
        let Some(end) = after_start.find('`') else {
            spans.push(Span::styled(remainder[start..].to_owned(), base_style));
            return spans;
        };
        let code = &after_start[..end];
        spans.push(Span::styled(
            format!(" {code} "),
            inline_code_style(state, base_style),
        ));
        remainder = &after_start[end + 1..];
    }
}

fn inline_code_style(state: &TuiState, base_style: Style) -> Style {
    let foreground = state.theme().color(SemanticColor::Focus);
    let background = state.theme().color(SemanticColor::Selection).map(dim_color);
    let mut style = base_style.add_modifier(Modifier::BOLD);
    if let Some(foreground) = foreground {
        style = style.fg(foreground);
    }
    if let Some(background) = background {
        style = style.bg(background);
    }
    style
}

fn timeline_body_style(state: &TuiState, item: &TimelineItem, line: &str) -> Style {
    if matches!(item, TimelineItem::Diagnostic { .. }) {
        return semantic_style(state, SemanticColor::Error);
    }
    if line.starts_with('+') {
        return semantic_style(state, SemanticColor::DiffAdd);
    }
    if line.starts_with('-') {
        return semantic_style(state, SemanticColor::DiffDelete);
    }
    semantic_style(state, SemanticColor::Focus)
}

fn semantic_style(state: &TuiState, slot: SemanticColor) -> Style {
    state
        .theme()
        .color(slot)
        .map_or_else(Style::default, |color| Style::default().fg(color))
}
