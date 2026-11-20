use crate::tui::{copy_controls::MAX_CLIPBOARD_BYTES, text_interaction::MouseInput};
use crossterm::{
    cursor::{Hide, SetCursorStyle, Show},
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyEvent, KeyEventKind, KeyboardEnhancementFlags, MouseButton,
        MouseEvent, MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute, queue,
    terminal::{
        BeginSynchronizedUpdate, EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
        disable_raw_mode, enable_raw_mode,
    },
};
use futures_util::StreamExt;
use ratatui::layout::Size;
use ratatui::{Frame, Terminal, backend::CrosstermBackend, layout::Position};
use std::{
    io::{self, Stdout, Write, stdout},
    time::Duration,
};

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
        // Best-effort Kitty keyboard protocol so modern terminals report
        // Cmd/Super, Shift+Enter, Ctrl+Shift+Z, and Ctrl+Backspace distinctly.
        let _ = execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
            SetCursorStyle::BlinkingBar
        );

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
            if let Some(mapped) = map_crossterm_event(event) {
                return Ok(Some(mapped));
            }
        }
    }

    /// Drains already-queued terminal events without awaiting I/O and without
    /// polling [`EventStream`] with a noop waker.
    ///
    /// Note: `crossterm::event::EventStream::poll_next` stores the first
    /// `cx.waker()` when `stream_wake_task_executed` transitions from `false` to
    /// `true`. Polling `EventStream` with `now_or_never()` registers a noop
    /// waker that silently swallows the wake for the real Tokio task, freezing
    /// the UI permanently on the next event. Calling `crossterm::event::poll(0)`
    /// together with `read()` directly reads the shared internal reader without
    /// touching `EventStream`'s wake task state.
    pub(crate) fn drain_queued_events(
        &mut self,
        max_events: usize,
    ) -> io::Result<Vec<TerminalEvent>> {
        let mut drained = Vec::new();
        let max_raw_polls = max_events.saturating_mul(4).max(16);
        let mut raw_polls = 0usize;
        while drained.len() < max_events && raw_polls < max_raw_polls {
            raw_polls += 1;
            if !crossterm::event::poll(Duration::ZERO)? {
                break;
            }
            let event = crossterm::event::read()?;
            if let Some(mapped) = map_crossterm_event(event) {
                drained.push(mapped);
            }
        }
        Ok(drained)
    }

    pub(crate) fn draw(&mut self, draw: impl FnOnce(&mut Frame<'_>)) -> io::Result<()> {
        draw_synchronized(&mut self.terminal, draw)
    }

    pub(crate) fn size(&self) -> io::Result<Size> {
        self.terminal.size()
    }

    /// Asks the terminal to copy. The system clipboard is tried first; this remains the remote fallback.
    pub(crate) fn copy_text(&mut self, text: &str) -> io::Result<()> {
        write_clipboard_request(self.terminal.backend_mut(), text)
    }
}

/// Wraps a ratatui frame in DECSET/DECRST 2026 synchronized update markers so
/// terminals present the whole frame atomically without mid-draw tearing.
pub(crate) fn draw_synchronized<W: Write>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    draw: impl FnOnce(&mut Frame<'_>),
) -> io::Result<()> {
    queue!(terminal.backend_mut(), BeginSynchronizedUpdate)?;
    let draw_result = terminal.draw(draw).map(|_| ());
    let end_result = queue!(terminal.backend_mut(), EndSynchronizedUpdate)
        .and_then(|()| terminal.backend_mut().flush());
    draw_result.and(end_result)
}

fn map_crossterm_event(event: Event) -> Option<TerminalEvent> {
    match event {
        Event::Key(key) if key.kind != KeyEventKind::Release => Some(TerminalEvent::Key(
            crate::tui::keymap::normalize_command_key(key),
        )),
        Event::Mouse(mouse) => mouse_event(mouse),
        Event::Paste(text) => Some(TerminalEvent::Paste(text)),
        Event::Resize(_, _) => Some(TerminalEvent::Resize),
        _ => None,
    }
}

pub(crate) fn write_clipboard_request(writer: &mut impl io::Write, text: &str) -> io::Result<()> {
    if text.len() > MAX_CLIPBOARD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Clipboard payload exceeds 1 MiB; inspect the full tool output in the trajectory instead",
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
        let _ = execute!(
            self.terminal.backend_mut(),
            PopKeyboardEnhancementFlags,
            SetCursorStyle::DefaultUserShape
        );
        if mouse_capture_enabled() {
            let _ = execute!(self.terminal.backend_mut(), DisableMouseCapture);
        }
        let _ = execute!(
            self.terminal.backend_mut(),
            EndSynchronizedUpdate,
            Show,
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
    }
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let mut stdout = stdout();
    let _ = execute!(
        stdout,
        PopKeyboardEnhancementFlags,
        SetCursorStyle::DefaultUserShape
    );
    if mouse_capture_enabled() {
        let _ = execute!(stdout, DisableMouseCapture);
    }
    let _ = execute!(
        stdout,
        EndSynchronizedUpdate,
        Show,
        DisableBracketedPaste,
        LeaveAlternateScreen
    );
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
    use super::{TerminalEvent, draw_synchronized, mouse_capture_enabled, mouse_event};
    use crate::tui::text_interaction::MouseInput;
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{
        Terminal,
        backend::CrosstermBackend,
        layout::{Position, Rect},
        widgets::Paragraph,
    };

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

    #[test]
    fn synchronized_draw_wraps_frame_output_in_decset_2026_markers() {
        #[derive(Clone, Default)]
        struct SharedBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

        impl std::io::Write for SharedBuf {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().expect("lock").extend_from_slice(buf);
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let shared = SharedBuf::default();
        let backend = CrosstermBackend::new(shared.clone());
        let options = ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(Rect::new(0, 0, 20, 4)),
        };
        let mut terminal = Terminal::with_options(backend, options).expect("terminal");
        draw_synchronized(&mut terminal, |frame| {
            frame.render_widget(Paragraph::new("merry"), frame.area());
        })
        .expect("synchronized draw");

        let written = String::from_utf8(shared.0.lock().expect("lock").clone()).expect("utf-8");
        assert!(
            written.starts_with("\x1b[?2026h"),
            "expected BeginSynchronizedUpdate prefix, got {written:?}"
        );
        assert!(
            written.ends_with("\x1b[?2026l"),
            "expected EndSynchronizedUpdate suffix, got {written:?}"
        );
        assert!(written.contains("merry"), "expected frame text in output");
    }
}
