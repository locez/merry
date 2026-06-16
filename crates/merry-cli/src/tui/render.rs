use super::state::{TimelineItem, TuiState};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

#[allow(dead_code)]
pub(crate) fn render(frame: &mut Frame<'_>, state: &TuiState) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(queue_height(state)),
            Constraint::Length(3),
        ])
        .split(frame.area());

    frame.render_widget(Paragraph::new(state.status_text()), root[0]);
    frame.render_widget(
        Paragraph::new(timeline_lines(state)).wrap(Wrap { trim: false }),
        root[1],
    );
    frame.render_widget(
        Paragraph::new(queue_lines(state)).block(Block::default().borders(Borders::TOP)),
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

fn queue_lines(state: &TuiState) -> Vec<Line<'static>> {
    let queue = state.queue_preview();
    let mut lines = Vec::new();
    if !queue.next.is_empty() {
        lines.push(Line::from("Next"));
        lines.extend(queue.next.iter().enumerate().map(|(index, item)| {
            Line::from(format!("  {}. {}", index + 1, item.display_text(72)))
        }));
    }
    if !queue.suspended.is_empty() {
        lines.push(Line::from("Suspended"));
        lines.extend(queue.suspended.iter().enumerate().map(|(index, item)| {
            Line::from(format!("  {}. {}", index + 1, item.display_text(72)))
        }));
    }
    if !queue.backlog.is_empty() {
        lines.push(Line::from("Backlog"));
        lines.extend(queue.backlog.iter().enumerate().map(|(index, item)| {
            Line::from(format!("  {}. {}", index + 1, item.display_text(72)))
        }));
    }
    lines
}

fn queue_height(state: &TuiState) -> u16 {
    let queue = state.queue_preview();
    let count = queue.next.len() + queue.suspended.len() + queue.backlog.len();
    if count == 0 {
        1
    } else {
        (count + 4).min(8) as u16
    }
}
