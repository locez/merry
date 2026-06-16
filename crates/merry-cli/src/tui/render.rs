use super::state::{TimelineItem, TuiState};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
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

    frame.render_widget(Paragraph::new(state.status_text()), root[0]);
    frame.render_widget(
        Paragraph::new(timeline_lines(state)).wrap(Wrap { trim: false }),
        root[1],
    );
    frame.render_widget(
        Paragraph::new(queue_lines(state, root[2])).block(Block::default().borders(Borders::TOP)),
        root[2],
    );
    frame.render_widget(
        Paragraph::new(state.input_text())
            .block(Block::default().borders(Borders::TOP).title("input")),
        root[3],
    );
}

#[cfg(test)]
pub(crate) fn render_to_text(state: &TuiState, width: u16, height: u16) -> String {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal should build");
    terminal
        .draw(|frame| render(frame, state))
        .expect("test render should draw");

    let buffer = terminal.backend().buffer();
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

fn timeline_lines(state: &TuiState) -> Vec<Line<'static>> {
    state
        .timeline()
        .iter()
        .flat_map(|item| match item {
            TimelineItem::Assistant { text } => vec![Line::from(text.clone())],
            TimelineItem::Muted { title, detail } => vec![Line::from(vec![
                Span::raw(title.clone()),
                Span::raw(": "),
                Span::raw(detail.clone()),
            ])],
            TimelineItem::Expanded { title, body } | TimelineItem::Diagnostic { title, body } => {
                let mut lines = vec![Line::from(title.clone())];
                lines.extend(body.lines().map(|line| Line::from(line.to_owned())));
                lines
            }
        })
        .collect()
}

fn queue_lines(state: &TuiState, region: Rect) -> Vec<Line<'static>> {
    let queue = state.queue_preview();
    let mut lines = Vec::new();
    if !queue.next.is_empty() {
        lines.push(Line::from("Next"));
        lines.extend(queue.next.iter().enumerate().map(|(index, item)| {
            let prefix = format!("  {}. ", index + 1);
            Line::from(format!(
                "{prefix}{}",
                item.display_text(queue_item_width(region.width, prefix.len()))
            ))
        }));
    }
    if !queue.suspended.is_empty() {
        lines.push(Line::from("Suspended"));
        lines.extend(queue.suspended.iter().enumerate().map(|(index, item)| {
            let prefix = format!("  {}. ", index + 1);
            Line::from(format!(
                "{prefix}{}",
                item.display_text(queue_item_width(region.width, prefix.len()))
            ))
        }));
    }
    if !queue.backlog.is_empty() {
        lines.push(Line::from("Backlog"));
        lines.extend(queue.backlog.iter().enumerate().map(|(index, item)| {
            let prefix = format!("  {}. ", index + 1);
            Line::from(format!(
                "{prefix}{}",
                item.display_text(queue_item_width(region.width, prefix.len()))
            ))
        }));
    }
    lines
}

fn queue_item_width(region_width: u16, prefix_width: usize) -> usize {
    usize::from(region_width).saturating_sub(prefix_width)
}
