use super::{
    state::{TimelineItem, TuiState},
    theme::SemanticColor,
};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

const QUEUE_PREVIEW_HEIGHT: u16 = 6;

#[allow(dead_code)]
pub(crate) fn render(frame: &mut Frame<'_>, state: &TuiState) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(QUEUE_PREVIEW_HEIGHT),
            Constraint::Length(3),
        ])
        .split(frame.area());

    frame.render_widget(
        Paragraph::new(state.status_text()).style(semantic_style(state, SemanticColor::Status)),
        root[0],
    );
    frame.render_widget(
        Paragraph::new(timeline_lines(state)).wrap(Wrap { trim: false }),
        root[1],
    );
    frame.render_widget(
        Paragraph::new(queue_lines(state, root[2])).block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(semantic_style(state, SemanticColor::Muted)),
        ),
        root[2],
    );
    frame.render_widget(
        Paragraph::new(state.input_text())
            .style(semantic_style(state, SemanticColor::Focus))
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .title("input")
                    .border_style(semantic_style(state, SemanticColor::Focus))
                    .title_style(semantic_style(state, SemanticColor::Focus)),
            ),
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

fn timeline_lines(state: &TuiState) -> Vec<Line<'static>> {
    state
        .timeline()
        .iter()
        .flat_map(|item| match item {
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
        })
        .collect()
}

fn queue_lines(state: &TuiState, region: Rect) -> Vec<Line<'static>> {
    let queue = state.queue_preview();
    let mut lines = Vec::new();
    if !queue.next.is_empty() {
        lines.push(queue_heading(state, "Next"));
        lines.extend(queue.next.iter().enumerate().map(|(index, item)| {
            let prefix = format!("  {}. ", index + 1);
            queue_item(
                state,
                &prefix,
                &item.display_text(queue_item_width(region.width, prefix.len())),
            )
        }));
    }
    if !queue.suspended.is_empty() {
        lines.push(queue_heading(state, "Suspended"));
        lines.extend(queue.suspended.iter().enumerate().map(|(index, item)| {
            let prefix = format!("  {}. ", index + 1);
            queue_item(
                state,
                &prefix,
                &item.display_text(queue_item_width(region.width, prefix.len())),
            )
        }));
    }
    if !queue.backlog.is_empty() {
        lines.push(queue_heading(state, "Backlog"));
        lines.extend(queue.backlog.iter().enumerate().map(|(index, item)| {
            let prefix = format!("  {}. ", index + 1);
            queue_item(
                state,
                &prefix,
                &item.display_text(queue_item_width(region.width, prefix.len())),
            )
        }));
    }
    lines
}

fn queue_item_width(region_width: u16, prefix_width: usize) -> usize {
    usize::from(region_width).saturating_sub(prefix_width)
}

fn queue_heading(state: &TuiState, label: &'static str) -> Line<'static> {
    Line::from(Span::styled(
        label,
        semantic_style(state, SemanticColor::Focus),
    ))
}

fn queue_item(state: &TuiState, prefix: &str, text: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            prefix.to_owned(),
            semantic_style(state, SemanticColor::Muted),
        ),
        Span::styled(text.to_owned(), semantic_style(state, SemanticColor::Muted)),
    ])
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
