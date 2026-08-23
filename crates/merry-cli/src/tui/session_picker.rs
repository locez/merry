use super::{
    session_list::{TuiSessionMetadata, TuiSessionStore},
    terminal::{TerminalEvent, TerminalSession},
};
use crate::cli_error::{CliError, unexpected};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph},
};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionPickerSelection {
    New,
    Resume(TuiSessionMetadata),
    Quit,
}

#[derive(Debug, Clone)]
pub(crate) struct SessionPickerState {
    sessions: Vec<TuiSessionMetadata>,
    selected: usize,
}

impl SessionPickerState {
    pub(crate) fn new(sessions: Vec<TuiSessionMetadata>) -> Self {
        Self {
            sessions,
            selected: 0,
        }
    }

    pub(crate) fn select_next(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        self.selected = (self.selected + 1).min(self.sessions.len().saturating_sub(1));
    }

    pub(crate) fn select_previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub(crate) fn accept(&self) -> SessionPickerSelection {
        self.sessions
            .get(self.selected)
            .cloned()
            .map(SessionPickerSelection::Resume)
            .unwrap_or(SessionPickerSelection::New)
    }
}

pub(crate) fn handle_picker_key(
    key: KeyEvent,
    state: &mut SessionPickerState,
) -> Option<SessionPickerSelection> {
    match key.code {
        KeyCode::Enter => Some(state.accept()),
        KeyCode::Char('n') | KeyCode::Char('N') => Some(SessionPickerSelection::New),
        KeyCode::Char('q') | KeyCode::Esc => Some(SessionPickerSelection::Quit),
        KeyCode::Down => {
            state.select_next();
            None
        }
        KeyCode::Up => {
            state.select_previous();
            None
        }
        _ => None,
    }
}

pub(crate) async fn pick_session(
    terminal: &mut TerminalSession,
    store: &TuiSessionStore,
    workspace_root: &Path,
) -> Result<SessionPickerSelection, CliError> {
    let sessions = store
        .sessions_for_workspace(workspace_root)
        .map_err(unexpected)?;
    let mut state = SessionPickerState::new(sessions);

    terminal
        .draw(|frame| render_session_picker(frame, &state, workspace_root))
        .map_err(unexpected)?;
    loop {
        let Some(event) = terminal.next_event().await.map_err(unexpected)? else {
            return Ok(SessionPickerSelection::Quit);
        };
        match event {
            TerminalEvent::Key(key) => {
                if let Some(selection) = handle_picker_key(key, &mut state) {
                    return Ok(selection);
                }
                terminal
                    .draw(|frame| render_session_picker(frame, &state, workspace_root))
                    .map_err(unexpected)?;
            }
            TerminalEvent::Resize => {
                terminal
                    .draw(|frame| render_session_picker(frame, &state, workspace_root))
                    .map_err(unexpected)?;
            }
            TerminalEvent::MouseScrollDown(_) => {
                state.select_next();
                terminal
                    .draw(|frame| render_session_picker(frame, &state, workspace_root))
                    .map_err(unexpected)?;
            }
            TerminalEvent::MouseScrollUp(_) => {
                state.select_previous();
                terminal
                    .draw(|frame| render_session_picker(frame, &state, workspace_root))
                    .map_err(unexpected)?;
            }
            TerminalEvent::Paste(_) => {}
        }
    }
}

fn render_session_picker(frame: &mut Frame<'_>, state: &SessionPickerState, workspace_root: &Path) {
    let area = frame.area();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::LightMagenta))
        .title("Merry Sessions")
        .title_style(
            Style::default()
                .fg(Color::LightMagenta)
                .add_modifier(Modifier::BOLD),
        );

    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [header, list_area, footer] = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Length(3),
        ratatui::layout::Constraint::Min(1),
        ratatui::layout::Constraint::Length(2),
    ])
    .areas(inner);

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled("Workspace ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    workspace_root.display().to_string(),
                    Style::default().fg(Color::White),
                ),
            ]),
            Line::from("Enter resume  n new  q quit"),
        ])
        .style(Style::default().fg(Color::LightMagenta)),
        header,
    );

    let items = if state.sessions.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "No saved sessions for this directory. Press n or Enter to start a new one.",
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
        state
            .sessions
            .iter()
            .map(session_line)
            .map(ListItem::new)
            .collect()
    };
    let mut list_state = ListState::default();
    if !state.sessions.is_empty() {
        list_state.select(Some(state.selected));
    }
    frame.render_stateful_widget(
        List::new(items).highlight_symbol("> ").highlight_style(
            Style::default()
                .fg(Color::LightMagenta)
                .add_modifier(Modifier::BOLD),
        ),
        list_area,
        &mut list_state,
    );

    frame.render_widget(
        Paragraph::new("Sessions are saved only on clean Ctrl-C Ctrl-C exit in this MVP.")
            .style(Style::default().fg(Color::DarkGray)),
        footer,
    );
}

fn session_line(session: &TuiSessionMetadata) -> Line<'static> {
    let title = session
        .title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or("Untitled session");
    let model = session.model.as_deref().unwrap_or("model -");
    let effort = session.reasoning_effort.as_deref().unwrap_or("-");
    let origin = if session.headless { "headless" } else { "TUI" };
    Line::from(vec![
        Span::styled(title.to_owned(), Style::default().fg(Color::White)),
        Span::raw("  "),
        Span::styled(
            session.session_id.as_str().to_owned(),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw("  "),
        Span::styled(model.to_owned(), Style::default().fg(Color::LightMagenta)),
        Span::raw(" "),
        Span::styled(effort.to_owned(), Style::default().fg(Color::LightMagenta)),
        Span::raw("  "),
        Span::styled(origin.to_owned(), Style::default().fg(Color::DarkGray)),
    ])
}

#[cfg(test)]
pub(crate) fn render_picker_to_text(
    state: &SessionPickerState,
    workspace_root: &Path,
    width: u16,
    height: u16,
) -> String {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal should build");
    terminal
        .draw(|frame| render_session_picker(frame, state, workspace_root))
        .expect("picker should render");
    let buffer = terminal.backend().buffer().clone();
    let mut text = String::new();
    for y in 0..height {
        for x in 0..width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use merry_core::SessionId;
    use std::path::PathBuf;

    fn metadata(id: &str, active: u128) -> TuiSessionMetadata {
        let mut metadata = TuiSessionMetadata::new(
            SessionId::new(id).expect("valid session id"),
            PathBuf::from("/repo"),
            active,
        );
        metadata.title = Some(format!("session {id}"));
        metadata.model = Some("gpt-test".to_owned());
        metadata.reasoning_effort = Some("medium".to_owned());
        metadata
    }

    #[test]
    fn picker_accepts_selected_session_and_allows_new() {
        let mut state =
            SessionPickerState::new(vec![metadata("session-a", 10), metadata("session-b", 20)]);

        assert_eq!(
            state.accept(),
            SessionPickerSelection::Resume(metadata("session-a", 10))
        );
        state.select_next();
        assert_eq!(
            state.accept(),
            SessionPickerSelection::Resume(metadata("session-b", 20))
        );
        let mut empty = SessionPickerState::new(Vec::new());
        assert_eq!(empty.accept(), SessionPickerSelection::New);
        assert_eq!(
            handle_picker_key(
                KeyEvent::new(KeyCode::Char('n'), crossterm::event::KeyModifiers::NONE),
                &mut empty
            ),
            Some(SessionPickerSelection::New)
        );
    }

    #[test]
    fn picker_render_shows_sessions_even_when_only_one_exists() {
        let state = SessionPickerState::new(vec![metadata("session-a", 10)]);

        let text = render_picker_to_text(&state, Path::new("/repo"), 80, 12);

        assert!(text.contains("Merry Sessions"));
        assert!(text.contains("session session-a"));
        assert!(text.contains("Enter resume"));
    }

    #[test]
    fn picker_marks_headless_sessions_separately_from_tui_sessions() {
        let mut headless = metadata("headless-session", 10);
        headless.headless = true;
        let text = render_picker_to_text(
            &SessionPickerState::new(vec![headless]),
            Path::new("/repo"),
            100,
            12,
        );

        assert!(text.contains("headless"));
    }
}
