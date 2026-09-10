use crate::tui::{copy_controls::MAX_CLIPBOARD_BYTES, text_interaction::MouseInput};
use crossterm::{
    cursor::{Hide, Show},
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyEvent, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::StreamExt;
use ratatui::layout::Size;
use ratatui::{Frame, Terminal, backend::CrosstermBackend, layout::Position};
use std::io::{self, Stdout, stdout};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TerminalEvent {
    Key(KeyEvent),
    Mouse(MouseInput),
    MouseScrollUp(Position),
    MouseScrollDown(Position),
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
        if let Err(err) = execute!(stdout, EnterAlternateScreen, EnableBracketedPaste, Hide) {
            restore_terminal();
            return Err(err);
        }
        if mouse_capture_enabled()
            && let Err(err) = execute!(stdout, EnableMouseCapture)
        {
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
                    if let Some(event) = mouse_event(mouse) {
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

    pub(crate) fn size(&self) -> io::Result<Size> {
        self.terminal.size()
    }

    /// Requests a clipboard write through the terminal; OSC 52 support is terminal-owned.
    pub(crate) fn copy_text(&mut self, text: &str) -> io::Result<()> {
        write_clipboard_request(self.terminal.backend_mut(), text)
    }
}

pub(crate) fn write_clipboard_request(writer: &mut impl io::Write, text: &str) -> io::Result<()> {
    if text.len() > MAX_CLIPBOARD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Clipboard payload exceeds 1 MiB; inspect the artifact in the trajectory instead",
        ));
    }
    execute!(
        writer,
        crossterm::clipboard::CopyToClipboard::to_clipboard_from(text)
    )
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        if mouse_capture_enabled() {
            let _ = execute!(self.terminal.backend_mut(), DisableMouseCapture);
        }
        let _ = execute!(
            self.terminal.backend_mut(),
            Show,
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
    }
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let mut stdout = stdout();
    if mouse_capture_enabled() {
        let _ = execute!(stdout, DisableMouseCapture);
    }
    let _ = execute!(stdout, Show, DisableBracketedPaste, LeaveAlternateScreen);
}

fn mouse_capture_enabled() -> bool {
    true
}

fn mouse_event(mouse: MouseEvent) -> Option<TerminalEvent> {
    let position = Position::new(mouse.column, mouse.row);
    match mouse.kind {
        MouseEventKind::ScrollUp => Some(TerminalEvent::MouseScrollUp(position)),
        MouseEventKind::ScrollDown => Some(TerminalEvent::MouseScrollDown(position)),
        MouseEventKind::Down(MouseButton::Left) if mouse.modifiers.is_empty() => {
            Some(TerminalEvent::Mouse(MouseInput::Down(position)))
        }
        MouseEventKind::Drag(MouseButton::Left) if mouse.modifiers.is_empty() => {
            Some(TerminalEvent::Mouse(MouseInput::Drag(position)))
        }
        MouseEventKind::Up(MouseButton::Left) if mouse.modifiers.is_empty() => {
            Some(TerminalEvent::Mouse(MouseInput::Up(position)))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{TerminalEvent, mouse_capture_enabled, mouse_event};
    use crate::tui::text_interaction::MouseInput;
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::layout::Position;

    fn mouse(kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column: 7,
            row: 9,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn maps_mouse_wheel_to_timeline_scroll_events() {
        assert_eq!(
            mouse_event(mouse(MouseEventKind::ScrollUp)),
            Some(TerminalEvent::MouseScrollUp(Position::new(7, 9)))
        );
        assert_eq!(
            mouse_event(mouse(MouseEventKind::ScrollDown)),
            Some(TerminalEvent::MouseScrollDown(Position::new(7, 9)))
        );
        assert_eq!(mouse_event(mouse(MouseEventKind::ScrollLeft)), None);
        assert_eq!(mouse_event(mouse(MouseEventKind::ScrollRight)), None);
    }

    #[test]
    fn maps_left_button_presses_to_click_actions() {
        assert_eq!(
            mouse_event(mouse(MouseEventKind::Down(MouseButton::Left))),
            Some(TerminalEvent::Mouse(MouseInput::Down(Position::new(7, 9))))
        );
        assert_eq!(
            mouse_event(mouse(MouseEventKind::Drag(MouseButton::Left))),
            Some(TerminalEvent::Mouse(MouseInput::Drag(Position::new(7, 9))))
        );
        assert_eq!(
            mouse_event(mouse(MouseEventKind::Up(MouseButton::Left))),
            Some(TerminalEvent::Mouse(MouseInput::Up(Position::new(7, 9))))
        );
    }

    #[test]
    fn ignores_drag_release_and_motion_events() {
        for kind in [
            MouseEventKind::Drag(MouseButton::Right),
            MouseEventKind::Moved,
        ] {
            assert_eq!(mouse_event(mouse(kind)), None);
        }
    }

    #[test]
    fn enables_mouse_capture_for_app_owned_timeline_scroll() {
        assert!(mouse_capture_enabled());
    }

    #[test]
    fn modified_and_non_left_mouse_events_do_not_start_application_actions() {
        for modifiers in [
            KeyModifiers::SHIFT,
            KeyModifiers::ALT,
            KeyModifiers::CONTROL,
        ] {
            for kind in [
                MouseEventKind::Down(MouseButton::Left),
                MouseEventKind::Drag(MouseButton::Left),
                MouseEventKind::Up(MouseButton::Left),
            ] {
                let mut event = mouse(kind);
                event.modifiers = modifiers;
                assert_eq!(mouse_event(event), None);
            }
        }
        for kind in [
            MouseEventKind::Down(MouseButton::Right),
            MouseEventKind::Down(MouseButton::Middle),
        ] {
            assert_eq!(mouse_event(mouse(kind)), None);
        }
    }
}
