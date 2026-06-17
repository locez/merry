use super::{
    state::{PatchChangeView, PatchLineView, TimelineItem, TuiState},
    theme::SemanticColor,
};
use merry_core::QueuedInputLane;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
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
        Paragraph::new(state.input_text())
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

fn timeline_lines(state: &TuiState, region_height: u16) -> Vec<Line<'static>> {
    let lines = state
        .timeline()
        .iter()
        .flat_map(|item| match item {
            TimelineItem::User { text, lane } => user_lines(state, text, *lane),
            TimelineItem::Assistant { text } => vec![Line::from(text.clone())],
            TimelineItem::Muted { title, detail } => vec![Line::from(vec![
                Span::styled(title.clone(), semantic_style(state, SemanticColor::Muted)),
                Span::styled(": ", semantic_style(state, SemanticColor::Muted)),
                Span::styled(detail.clone(), semantic_style(state, SemanticColor::Muted)),
            ])],
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
                    Line::from(Span::styled(
                        line.to_owned(),
                        timeline_body_style(state, item, line),
                    ))
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
    vec![Line::from(vec![
        Span::styled(
            label.to_owned(),
            semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
        ),
        Span::styled(": ", semantic_style(state, SemanticColor::Muted)),
        Span::styled(text.to_owned(), semantic_style(state, SemanticColor::Focus)),
    ])]
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
            lines.push(match line {
                PatchLineView::Context(text) => Line::from(Span::styled(
                    format!(" {text}"),
                    semantic_style(state, SemanticColor::Focus),
                )),
                PatchLineView::Remove(text) => Line::from(Span::styled(
                    format!("-{text}"),
                    semantic_style(state, SemanticColor::DiffDelete),
                )),
                PatchLineView::Add(text) => Line::from(Span::styled(
                    format!("+{text}"),
                    semantic_style(state, SemanticColor::DiffAdd),
                )),
            });
        }
    }
    lines
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
