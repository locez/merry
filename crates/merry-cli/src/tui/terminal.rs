use crossterm::{
    cursor::{Hide, Show},
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyEvent, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::StreamExt;
use ratatui::{Frame, Terminal, backend::CrosstermBackend};
use std::io::{self, Stdout, stdout};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TerminalEvent {
    Key(KeyEvent),
    MouseScrollUp,
    MouseScrollDown,
    Paste(String),
    Resize,
}

pub(crate) struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    events: EventStream,
}

impl TerminalSession {
    pub(crate) fn enter() -> io::Result<Self> {
        enable_raw_mode()?;

        let mut stdout = stdout();
        if let Err(err) = execute!(
            stdout,
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture,
            Hide
        ) {
            restore_terminal();
            return Err(err);
        }

        let backend = CrosstermBackend::new(stdout);
        let terminal = match Terminal::new(backend) {
            Ok(terminal) => terminal,
            Err(err) => {
                restore_terminal();
                return Err(err);
            }
        };

        Ok(Self {
            terminal,
            events: EventStream::new(),
        })
    }

    pub(crate) async fn next_event(&mut self) -> io::Result<Option<TerminalEvent>> {
        loop {
            let Some(event) = self.events.next().await.transpose()? else {
                return Ok(None);
            };

            match event {
                Event::Key(key) => return Ok(Some(TerminalEvent::Key(key))),
                Event::Mouse(mouse) => {
                    if let Some(event) = mouse_scroll_event(mouse) {
                        return Ok(Some(event));
                    }
                }
                Event::Paste(text) => return Ok(Some(TerminalEvent::Paste(text))),
                Event::Resize(_, _) => return Ok(Some(TerminalEvent::Resize)),
                _ => {}
            }
        }
    }

    pub(crate) fn draw(&mut self, draw: impl FnOnce(&mut Frame<'_>)) -> io::Result<()> {
        self.terminal.draw(draw).map(|_| ())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            Show,
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
    }
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(
        stdout(),
        Show,
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen
    );
}

fn mouse_scroll_event(mouse: MouseEvent) -> Option<TerminalEvent> {
    match mouse.kind {
        MouseEventKind::ScrollUp => Some(TerminalEvent::MouseScrollUp),
        MouseEventKind::ScrollDown => Some(TerminalEvent::MouseScrollDown),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{TerminalEvent, mouse_scroll_event};
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    fn mouse(kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn maps_mouse_wheel_to_timeline_scroll_events() {
        assert_eq!(
            mouse_scroll_event(mouse(MouseEventKind::ScrollUp)),
            Some(TerminalEvent::MouseScrollUp)
        );
        assert_eq!(
            mouse_scroll_event(mouse(MouseEventKind::ScrollDown)),
            Some(TerminalEvent::MouseScrollDown)
        );
        assert_eq!(
            mouse_scroll_event(mouse(MouseEventKind::Down(MouseButton::Left))),
            None
        );
        assert_eq!(
            mouse_scroll_event(mouse(MouseEventKind::Drag(MouseButton::Left))),
            None
        );
        assert_eq!(mouse_scroll_event(mouse(MouseEventKind::Moved)), None);
        assert_eq!(
            mouse_scroll_event(mouse(MouseEventKind::Up(MouseButton::Left))),
            None
        );
        assert_eq!(mouse_scroll_event(mouse(MouseEventKind::ScrollLeft)), None);
        assert_eq!(mouse_scroll_event(mouse(MouseEventKind::ScrollRight)), None);
    }
}
