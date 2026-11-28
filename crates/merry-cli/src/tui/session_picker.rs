use super::{
    runtime::ResumeFailureKind,
    session_list::{TuiSessionMetadata, TuiSessionStore},
    terminal::{TerminalEvent, TerminalSession},
    theme::{SemanticColor, TuiTheme},
};
use crate::cli_error::{CliError, unexpected};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionPickerSelection {
    New,
    Resume(TuiSessionMetadata),
    Quit,
}

/// Modal notice shown over the session picker when a selected session cannot be
/// resumed directly (for example, an incompatible legacy format or active lock).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionPickerNotice {
    pub(crate) kind: ResumeFailureKind,
    pub(crate) session_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct SessionPickerState {
    all_sessions: Vec<TuiSessionMetadata>,
    sessions: Vec<TuiSessionMetadata>,
    selected: usize,
    query: String,
    filtering: bool,
    notice: Option<SessionPickerNotice>,
}

impl SessionPickerState {
    pub(crate) fn new(sessions: Vec<TuiSessionMetadata>) -> Self {
        Self {
            all_sessions: sessions.clone(),
            sessions,
            selected: 0,
            query: String::new(),
            filtering: false,
            notice: None,
        }
    }

    pub(crate) fn with_notice(mut self, notice: Option<SessionPickerNotice>) -> Self {
        self.notice = notice;
        self
    }

    pub(crate) fn has_notice(&self) -> bool {
        self.notice.is_some()
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
    if state.notice.is_some() {
        return match key.code {
            KeyCode::Char('n') | KeyCode::Char('N') => {
                state.notice = None;
                Some(SessionPickerSelection::New)
            }
            KeyCode::Char('q') => Some(SessionPickerSelection::Quit),
            KeyCode::Enter | KeyCode::Esc | KeyCode::Char(' ') => {
                state.notice = None;
                None
            }
            _ => None,
        };
    }
    match key.code {
        KeyCode::Enter => Some(state.accept()),
        KeyCode::Char('/') if !state.filtering => {
            state.filtering = true;
            None
        }
        KeyCode::Char(character) if state.filtering => {
            state.query.push(character);
            None
        }
        KeyCode::Backspace if state.filtering => {
            state.query.pop();
            if state.query.is_empty() {
                state.filtering = false;
            }
            None
        }
        KeyCode::Esc if state.filtering => {
            state.query.clear();
            state.filtering = false;
            None
        }
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

async fn apply_picker_query(state: &mut SessionPickerState, store: &TuiSessionStore) {
    if state.query.is_empty() {
        state.sessions = state.all_sessions.clone();
    } else {
        let matches = store
            .session_state_store()
            .search_session_ids(&state.query)
            .await;
        let allowed = matches
            .into_iter()
            .map(|session_id| session_id.as_str().to_owned())
            .collect::<BTreeSet<_>>();
        state.sessions = state
            .all_sessions
            .iter()
            .filter(|session| allowed.contains(session.session_id.as_str()))
            .cloned()
            .collect();
    }
    if state.sessions.is_empty() {
        state.selected = 0;
    } else if state.selected >= state.sessions.len() {
        state.selected = state.sessions.len() - 1;
    }
}

pub(crate) async fn pick_session(
    terminal: &mut TerminalSession,
    store: &TuiSessionStore,
    workspace_root: &Path,
    theme: &TuiTheme,
) -> Result<SessionPickerSelection, CliError> {
    pick_session_with_notice(terminal, store, workspace_root, theme, None).await
}

pub(crate) async fn pick_session_with_notice(
    terminal: &mut TerminalSession,
    store: &TuiSessionStore,
    workspace_root: &Path,
    theme: &TuiTheme,
    notice: Option<SessionPickerNotice>,
) -> Result<SessionPickerSelection, CliError> {
    let sessions = store
        .sessions_for_workspace(workspace_root)
        .map_err(unexpected)?;
    let mut state = SessionPickerState::new(sessions).with_notice(notice);

    terminal
        .draw(|frame| render_session_picker(frame, &state, workspace_root, theme))
        .map_err(unexpected)?;
    loop {
        let Some(event) = terminal.next_event().await.map_err(unexpected)? else {
            return Ok(SessionPickerSelection::Quit);
        };
        match event {
            TerminalEvent::Key(key) => {
                let previous_query = state.query.clone();
                if let Some(selection) = handle_picker_key(key, &mut state) {
                    return Ok(selection);
                }
                if state.query != previous_query {
                    apply_picker_query(&mut state, store).await;
                }
                terminal
                    .draw(|frame| render_session_picker(frame, &state, workspace_root, theme))
                    .map_err(unexpected)?;
            }
            TerminalEvent::Resize => {
                terminal
                    .draw(|frame| render_session_picker(frame, &state, workspace_root, theme))
                    .map_err(unexpected)?;
            }
            TerminalEvent::MouseScrollDown(_) if !state.has_notice() => {
                state.select_next();
                terminal
                    .draw(|frame| render_session_picker(frame, &state, workspace_root, theme))
                    .map_err(unexpected)?;
            }
            TerminalEvent::MouseScrollUp(_) if !state.has_notice() => {
                state.select_previous();
                terminal
                    .draw(|frame| render_session_picker(frame, &state, workspace_root, theme))
                    .map_err(unexpected)?;
            }
            TerminalEvent::MouseScrollDown(_)
            | TerminalEvent::MouseScrollUp(_)
            | TerminalEvent::Paste(_)
            | TerminalEvent::Mouse(_) => {}
        }
    }
}

fn render_session_picker(
    frame: &mut Frame<'_>,
    state: &SessionPickerState,
    workspace_root: &Path,
    theme: &TuiTheme,
) {
    let surface = theme
        .color(SemanticColor::Surface)
        .unwrap_or(Color::Rgb(36, 32, 41));
    let focus = theme
        .color(SemanticColor::BorderFocus)
        .or_else(|| theme.color(SemanticColor::Focus))
        .unwrap_or(Color::LightMagenta);
    let text = theme
        .color(SemanticColor::Assistant)
        .unwrap_or(Color::White);
    let muted = theme.color(SemanticColor::Muted).unwrap_or(Color::DarkGray);
    let selection_bg = theme
        .color(SemanticColor::SelectionBackground)
        .unwrap_or(Color::Rgb(54, 26, 58));

    let area = frame.area();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(focus))
        .style(Style::default().bg(surface))
        .title(" M  Merry Sessions ")
        .title_style(Style::default().fg(focus).add_modifier(Modifier::BOLD));

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
                Span::styled("Workspace ", Style::default().fg(muted)),
                Span::styled(
                    workspace_root.display().to_string(),
                    Style::default().fg(text),
                ),
            ]),
            Line::from(if state.filtering {
                format!("filter: {query}  Esc clear", query = state.query)
            } else {
                "Enter resume  n new  q quit  / filter".to_owned()
            }),
        ])
        .style(Style::default().fg(focus).bg(surface)),
        header,
    );

    let items = if state.sessions.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "No saved sessions for this directory. Press n or Enter to start a new one.",
            Style::default().fg(muted),
        )))]
    } else {
        state
            .sessions
            .iter()
            .map(|session| session_line(session, theme))
            .map(ListItem::new)
            .collect()
    };
    let mut list_state = ListState::default();
    if !state.sessions.is_empty() {
        list_state.select(Some(state.selected));
    }
    frame.render_stateful_widget(
        List::new(items)
            .style(Style::default().bg(surface))
            .highlight_symbol("▌ ")
            .highlight_style(
                Style::default()
                    .fg(text)
                    .bg(selection_bg)
                    .add_modifier(Modifier::BOLD),
            ),
        list_area,
        &mut list_state,
    );

    frame.render_widget(
        Paragraph::new("Sessions are saved only on clean Ctrl-C Ctrl-C exit in this MVP.")
            .style(Style::default().fg(muted).bg(surface)),
        footer,
    );

    if let Some(notice) = &state.notice {
        render_notice_modal(frame, area, notice, theme);
    }
}

fn render_notice_modal(
    frame: &mut Frame<'_>,
    area: Rect,
    notice: &SessionPickerNotice,
    theme: &TuiTheme,
) {
    let surface = theme
        .color(SemanticColor::Surface)
        .unwrap_or(Color::Rgb(36, 32, 41));
    let warning = theme
        .color(SemanticColor::Warning)
        .unwrap_or(Color::LightYellow);
    let border = theme
        .color(SemanticColor::Border)
        .unwrap_or(Color::DarkGray);
    let text = theme
        .color(SemanticColor::Assistant)
        .unwrap_or(Color::White);
    let accent = theme
        .color(SemanticColor::ToolKeyword)
        .unwrap_or(Color::LightCyan);
    let muted = theme.color(SemanticColor::Muted).unwrap_or(Color::DarkGray);

    let (title, headline, guidance) = match notice.kind {
        ResumeFailureKind::IncompatibleFormat => (
            " Incompatible Session ",
            "This session uses a different format and cannot be resumed directly.",
            "Start a new session and ask Merry to read key context from:",
        ),
        ResumeFailureKind::AlreadyOpen => (
            " Session In Use ",
            "This session is currently open in another Merry window.",
            "Close it there to resume, or start a new session to read from:",
        ),
    };

    let popup = centered_rect(area, 72, 13);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .title(title)
        .title_style(Style::default().fg(warning).add_modifier(Modifier::BOLD))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(warning))
        .style(Style::default().bg(surface));
    let inner = inset_rect(block.inner(popup), 2, 1);
    frame.render_widget(block, popup);

    let [body_area, rule_area, actions_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(inner);

    let path_display = notice.session_dir.display().to_string();
    let lines = vec![
        Line::from(Span::styled(
            headline,
            Style::default().fg(text).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(guidance, Style::default().fg(muted))),
        Line::from(Span::styled(
            format!("  {path_display}"),
            Style::default().fg(accent),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(text).bg(surface))
            .wrap(Wrap { trim: false }),
        body_area,
    );

    let divider = "─".repeat(usize::from(rule_area.width));
    frame.render_widget(
        Paragraph::new(divider).style(Style::default().fg(border).bg(surface)),
        rule_area,
    );

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "n",
                Style::default().fg(warning).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" New session", Style::default().fg(text)),
            Span::styled("   ·   ", Style::default().fg(muted)),
            Span::styled(
                "Enter / Esc",
                Style::default().fg(warning).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Back", Style::default().fg(text)),
            Span::styled("   ·   ", Style::default().fg(muted)),
            Span::styled(
                "q",
                Style::default().fg(warning).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Quit", Style::default().fg(text)),
        ]))
        .style(Style::default().bg(surface)),
        actions_area,
    );
}

fn inset_rect(area: Rect, horizontal: u16, vertical: u16) -> Rect {
    let x = area.x.saturating_add(horizontal.min(area.width / 2));
    let y = area.y.saturating_add(vertical.min(area.height / 2));
    let width = area
        .width
        .saturating_sub(horizontal.saturating_mul(2))
        .max(1)
        .min(area.width);
    let height = area
        .height
        .saturating_sub(vertical.saturating_mul(2))
        .max(1)
        .min(area.height);
    Rect::new(x, y, width, height)
}

fn centered_rect(area: Rect, max_width: u16, max_height: u16) -> Rect {
    let width = area
        .width
        .saturating_sub(4)
        .clamp(1, max_width)
        .min(area.width);
    let height = area
        .height
        .saturating_sub(2)
        .clamp(1, max_height)
        .min(area.height);
    let x = area.x.saturating_add(area.width.saturating_sub(width) / 2);
    let y = area
        .y
        .saturating_add(area.height.saturating_sub(height) / 2);
    Rect::new(x, y, width, height)
}

fn session_line(session: &TuiSessionMetadata, theme: &TuiTheme) -> Line<'static> {
    let text = theme
        .color(SemanticColor::Assistant)
        .unwrap_or(Color::White);
    let muted = theme.color(SemanticColor::Muted).unwrap_or(Color::DarkGray);
    let accent = theme
        .color(SemanticColor::ToolKeyword)
        .unwrap_or(Color::LightCyan);
    let label = session
        .topic
        .as_deref()
        .filter(|topic| !topic.trim().is_empty())
        .unwrap_or("Untitled session");
    let model = session.model.as_deref().unwrap_or("model -");
    let effort = session.reasoning_effort.as_deref().unwrap_or("-");
    let origin = if session.headless { "headless" } else { "TUI" };
    let mut spans = vec![Span::styled(label.to_owned(), Style::default().fg(text))];
    spans.extend([
        Span::raw("  "),
        Span::styled(
            session.session_id.as_str().to_owned(),
            Style::default().fg(muted),
        ),
        Span::raw("  "),
        Span::styled(model.to_owned(), Style::default().fg(accent)),
        Span::raw(" "),
        Span::styled(effort.to_owned(), Style::default().fg(accent)),
        Span::raw("  "),
        Span::styled(origin.to_owned(), Style::default().fg(muted)),
    ]);
    Line::from(spans)
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
    let theme = TuiTheme::default();
    terminal
        .draw(|frame| render_session_picker(frame, state, workspace_root, &theme))
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
    use merry::events::SessionId;
    use std::path::PathBuf;

    fn metadata(id: &str, active: u128) -> TuiSessionMetadata {
        let mut metadata = TuiSessionMetadata::new(
            SessionId::new(id).expect("valid session id"),
            PathBuf::from("/repo"),
            active,
        );
        metadata.topic = Some(format!("session {id}"));
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

    #[test]
    fn incompatible_session_notice_modal_renders_clean_guidance_and_handles_keys() {
        let notice = SessionPickerNotice {
            kind: ResumeFailureKind::IncompatibleFormat,
            session_dir: PathBuf::from("/state/sessions/legacy-session-01"),
        };
        let mut state = SessionPickerState::new(vec![
            metadata("legacy-session-01", 10),
            metadata("session-b", 20),
        ])
        .with_notice(Some(notice));

        let rendered = render_picker_to_text(&state, Path::new("/repo"), 90, 22);
        assert!(rendered.contains("Incompatible Session"));
        assert!(rendered.contains("different format"));
        assert!(rendered.contains("/state/sessions/legacy-session-01"));
        assert!(rendered.contains("New session"));
        assert!(!rendered.contains("Reason:"));

        // Navigation keys are absorbed while the modal is open.
        assert_eq!(
            handle_picker_key(
                KeyEvent::new(KeyCode::Down, crossterm::event::KeyModifiers::NONE),
                &mut state,
            ),
            None
        );
        assert!(state.has_notice());
        assert_eq!(
            state.accept(),
            SessionPickerSelection::Resume(metadata("legacy-session-01", 10))
        );

        // Enter dismisses the notice and returns to the picker list.
        assert_eq!(
            handle_picker_key(
                KeyEvent::new(KeyCode::Enter, crossterm::event::KeyModifiers::NONE),
                &mut state,
            ),
            None
        );
        assert!(!state.has_notice());

        // Pressing 'n' while the notice is open immediately starts a new session.
        let mut state_for_new = SessionPickerState::new(vec![metadata("legacy-session-01", 10)])
            .with_notice(Some(SessionPickerNotice {
                kind: ResumeFailureKind::IncompatibleFormat,
                session_dir: PathBuf::from("/state/sessions/legacy-session-01"),
            }));
        assert_eq!(
            handle_picker_key(
                KeyEvent::new(KeyCode::Char('n'), crossterm::event::KeyModifiers::NONE),
                &mut state_for_new,
            ),
            Some(SessionPickerSelection::New)
        );
    }
}
