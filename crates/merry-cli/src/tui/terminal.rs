use crate::tui::{
    copy_controls::MAX_CLIPBOARD_BYTES, text_interaction::MouseInput, theme::quantize_for_256,
};
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
use ratatui::{
    Frame, Terminal,
    backend::{Backend, ClearType, CrosstermBackend, WindowSize},
    buffer::Cell,
    layout::{Position, Size},
};
use std::{
    io::{self, Stdout, Write, stdout},
    time::Duration,
};

/// Color depth supported by the attached terminal emulator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColorSupport {
    /// Terminal supports 24-bit RGB SGR sequences (`38;2;r;g;b` / `48;2;r;g;b`).
    TrueColor,
    /// Terminal supports at most 256-color indexed SGR sequences (`38;5;n` / `48;5;n`).
    Ansi256,
}

impl ColorSupport {
    /// Detects terminal color capability from the current process environment.
    pub(crate) fn detect_from_env() -> Self {
        Self::from_env_vars(
            std::env::var("COLORTERM").ok().as_deref(),
            std::env::var("TERM_PROGRAM").ok().as_deref(),
            std::env::var("TERM").ok().as_deref(),
            std::env::var("KITTY_WINDOW_ID").ok().as_deref(),
            std::env::var("WT_SESSION").ok().as_deref(),
        )
    }

    /// Resolves terminal color support from explicit environment values.
    ///
    /// Checks explicit truecolor markers (`COLORTERM=truecolor|24bit`) first so
    /// newer terminals (including Apple Terminal on macOS Tahoe) keep 24-bit
    /// color, while older Apple Terminal (`TERM_PROGRAM=Apple_Terminal` without
    /// `COLORTERM`) and other 256-color terminals safely quantize RGB cells.
    pub(crate) fn from_env_vars(
        colorterm: Option<&str>,
        term_program: Option<&str>,
        term: Option<&str>,
        kitty_window_id: Option<&str>,
        wt_session: Option<&str>,
    ) -> Self {
        if let Some(colorterm) = colorterm {
            let lower = colorterm.to_ascii_lowercase();
            if lower.contains("truecolor") || lower.contains("24bit") {
                return Self::TrueColor;
            }
        }
        if kitty_window_id.is_some_and(|value| !value.is_empty())
            || wt_session.is_some_and(|value| !value.is_empty())
        {
            return Self::TrueColor;
        }
        if let Some(program) = term_program {
            let lower = program.to_ascii_lowercase();
            if lower == "apple_terminal" {
                return Self::Ansi256;
            }
            if matches!(
                lower.as_str(),
                "iterm.app"
                    | "ghostty"
                    | "wezterm"
                    | "hyper"
                    | "vscode"
                    | "warpterminal"
                    | "rio"
                    | "tabby"
                    | "contour"
                    | "zed"
            ) {
                return Self::TrueColor;
            }
        }
        if let Some(term) = term {
            let lower = term.to_ascii_lowercase();
            if lower.contains("direct")
                || lower.contains("truecolor")
                || lower.contains("24bit")
                || lower.contains("kitty")
                || lower.contains("ghostty")
                || lower.contains("wezterm")
                || lower.contains("alacritty")
            {
                return Self::TrueColor;
            }
        }
        Self::Ansi256
    }
}

/// Ratatui backend wrapper around [`CrosstermBackend`] that quantizes 24-bit
/// `Color::Rgb` cells to xterm-256 palette indices when the terminal does not
/// advertise TrueColor support.
pub(crate) struct TerminalBackend<W: Write> {
    inner: CrosstermBackend<W>,
    color_support: ColorSupport,
    quantized_scratch: Vec<(u16, u16, Cell)>,
}

impl<W: Write> TerminalBackend<W> {
    pub(crate) fn new(writer: W, color_support: ColorSupport) -> Self {
        Self {
            inner: CrosstermBackend::new(writer),
            color_support,
            quantized_scratch: Vec::new(),
        }
    }
}

impl<W: Write> Write for TerminalBackend<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Write::flush(&mut self.inner)
    }
}

impl<W: Write> Backend for TerminalBackend<W> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        match self.color_support {
            ColorSupport::TrueColor => self.inner.draw(content),
            ColorSupport::Ansi256 => {
                self.quantized_scratch.clear();
                self.quantized_scratch.extend(content.map(|(x, y, cell)| {
                    let mut quantized = cell.clone();
                    quantized.fg = quantize_for_256(quantized.fg);
                    quantized.bg = quantize_for_256(quantized.bg);
                    (x, y, quantized)
                }));
                let result = self.inner.draw(
                    self.quantized_scratch
                        .iter()
                        .map(|(x, y, cell)| (*x, *y, cell)),
                );
                self.quantized_scratch.clear();
                result
            }
        }
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        self.inner.size()
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.inner)
    }
}

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
    terminal: Terminal<TerminalBackend<Stdout>>,
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

        let backend = TerminalBackend::new(stdout, ColorSupport::detect_from_env());
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
    terminal: &mut Terminal<TerminalBackend<W>>,
    draw: impl FnOnce(&mut Frame<'_>),
) -> io::Result<()> {
    queue!(terminal.backend_mut(), BeginSynchronizedUpdate)?;
    let draw_result = terminal.draw(draw).map(|_| ());
    let end_result = queue!(terminal.backend_mut(), EndSynchronizedUpdate)
        .and_then(|()| Write::flush(terminal.backend_mut()));
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
        MouseEventKind::Moved => Some(TerminalEvent::Mouse(MouseInput::Move(position))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ColorSupport, TerminalBackend, TerminalEvent, draw_synchronized, mouse_capture_enabled,
        mouse_event,
    };
    use crate::tui::text_interaction::MouseInput;
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{
        Terminal,
        layout::{Position, Rect},
        style::{Color, Style},
        widgets::Paragraph,
    };

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

    impl SharedBuf {
        fn take_utf8(&self) -> String {
            let mut bytes = self.0.lock().expect("lock");
            let taken = std::mem::take(&mut *bytes);
            String::from_utf8(taken).expect("utf-8")
        }
    }

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
    fn maps_pointer_motion_for_hover_and_ignores_non_left_drag_events() {
        assert_eq!(
            mouse_event(mouse(MouseEventKind::Moved)),
            Some(TerminalEvent::Mouse(MouseInput::Move(Position::new(7, 9))))
        );
        assert_eq!(
            mouse_event(mouse(MouseEventKind::Drag(MouseButton::Right))),
            None
        );
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
        let shared = SharedBuf::default();
        let backend = TerminalBackend::new(shared.clone(), ColorSupport::TrueColor);
        let options = ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(Rect::new(0, 0, 20, 4)),
        };
        let mut terminal = Terminal::with_options(backend, options).expect("terminal");
        draw_synchronized(&mut terminal, |frame| {
            frame.render_widget(Paragraph::new("merry"), frame.area());
        })
        .expect("synchronized draw");

        let written = shared.take_utf8();
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

    #[test]
    fn detects_apple_terminal_as_ansi256_and_truecolor_terminals_accurately() {
        // Legacy Apple Terminal (pre-Tahoe) sets TERM_PROGRAM=Apple_Terminal without COLORTERM.
        assert_eq!(
            ColorSupport::from_env_vars(
                None,
                Some("Apple_Terminal"),
                Some("xterm-256color"),
                None,
                None
            ),
            ColorSupport::Ansi256
        );
        // Newer Apple Terminal advertising COLORTERM=truecolor keeps 24-bit color.
        assert_eq!(
            ColorSupport::from_env_vars(
                Some("truecolor"),
                Some("Apple_Terminal"),
                Some("xterm-256color"),
                None,
                None
            ),
            ColorSupport::TrueColor
        );
        assert_eq!(
            ColorSupport::from_env_vars(Some("24bit"), None, Some("xterm-256color"), None, None),
            ColorSupport::TrueColor
        );
        assert_eq!(
            ColorSupport::from_env_vars(
                None,
                Some("iTerm.app"),
                Some("xterm-256color"),
                None,
                None
            ),
            ColorSupport::TrueColor
        );
        assert_eq!(
            ColorSupport::from_env_vars(None, Some("ghostty"), Some("xterm-256color"), None, None),
            ColorSupport::TrueColor
        );
        assert_eq!(
            ColorSupport::from_env_vars(None, None, Some("xterm-direct"), None, None),
            ColorSupport::TrueColor
        );
        assert_eq!(
            ColorSupport::from_env_vars(None, None, Some("xterm-256color"), Some("1"), None),
            ColorSupport::TrueColor
        );
        assert_eq!(
            ColorSupport::from_env_vars(None, None, Some("xterm-256color"), None, Some("wt-1")),
            ColorSupport::TrueColor
        );
        // Plain 256-color terminals and SSH sessions without COLORTERM safely degrade to 256-color.
        assert_eq!(
            ColorSupport::from_env_vars(None, None, Some("xterm-256color"), None, None),
            ColorSupport::Ansi256
        );
        assert_eq!(
            ColorSupport::from_env_vars(None, Some("tmux"), Some("tmux-256color"), None, None),
            ColorSupport::Ansi256
        );
    }

    #[test]
    fn ansi256_backend_quantizes_rgb_cells_and_never_emits_truecolor_sgr() {
        let shared = SharedBuf::default();
        let backend = TerminalBackend::new(shared.clone(), ColorSupport::Ansi256);
        let options = ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(Rect::new(0, 0, 20, 2)),
        };
        let mut terminal = Terminal::with_options(backend, options).expect("terminal");

        let style = Style::default()
            .fg(Color::Rgb(255, 122, 200))
            .bg(Color::Rgb(35, 38, 39));
        draw_synchronized(&mut terminal, |frame| {
            frame.render_widget(Paragraph::new("merry").style(style), frame.area());
        })
        .expect("ansi256 draw");

        let frame1 = shared.take_utf8();
        assert!(
            !frame1.contains("38;2;") && !frame1.contains("48;2;"),
            "Ansi256 backend must never emit 24-bit RGB SGR codes: {frame1:?}"
        );
        assert!(
            frame1.contains("38;5;") && frame1.contains("48;5;235"),
            "expected quantized 256-color foreground and background SGR codes, got {frame1:?}"
        );

        // Second diff frame with updated RGB colors also emits only 256-color SGR codes.
        let style2 = Style::default()
            .fg(Color::Rgb(216, 180, 255))
            .bg(Color::Rgb(54, 30, 48));
        draw_synchronized(&mut terminal, |frame| {
            frame.render_widget(Paragraph::new("hello").style(style2), frame.area());
        })
        .expect("second ansi256 draw");

        let frame2 = shared.take_utf8();
        assert!(
            !frame2.contains("38;2;") && !frame2.contains("48;2;"),
            "Ansi256 diff frame must never emit 24-bit RGB SGR codes: {frame2:?}"
        );
        assert!(
            frame2.contains("hello"),
            "expected updated text in diff frame"
        );
    }

    #[test]
    fn truecolor_backend_preserves_24bit_rgb_sgr_sequences() {
        let shared = SharedBuf::default();
        let backend = TerminalBackend::new(shared.clone(), ColorSupport::TrueColor);
        let options = ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(Rect::new(0, 0, 20, 2)),
        };
        let mut terminal = Terminal::with_options(backend, options).expect("terminal");

        let style = Style::default()
            .fg(Color::Rgb(255, 122, 200))
            .bg(Color::Rgb(35, 38, 39));
        draw_synchronized(&mut terminal, |frame| {
            frame.render_widget(Paragraph::new("merry").style(style), frame.area());
        })
        .expect("truecolor draw");

        let written = shared.take_utf8();
        assert!(
            written.contains("38;2;255;122;200") && written.contains("48;2;35;38;39"),
            "TrueColor backend should preserve 24-bit RGB SGR sequences, got {written:?}"
        );
    }

    #[test]
    fn full_sakura_tui_frame_under_ansi256_emits_zero_truecolor_sequences() {
        let mut state = crate::tui::state::TuiState::new(
            "/Users/demo/ask".into(),
            "deepseek-flash".to_owned(),
            crate::tui::keymap::Keymap::default(),
            crate::tui::theme::TuiTheme::default(),
        );
        state.push_timeline_item(crate::tui::state::TimelineItem::User {
            text: "merry resume".to_owned(),
        });
        state.push_timeline_item(crate::tui::state::TimelineItem::Assistant {
            text: "Use `merry resume`:\n```rust\nfn main() { println!(\"ok\"); }\n```".to_owned(),
        });

        let shared = SharedBuf::default();
        let backend = TerminalBackend::new(shared.clone(), ColorSupport::Ansi256);
        let options = ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(Rect::new(0, 0, 80, 24)),
        };
        let mut terminal = Terminal::with_options(backend, options).expect("terminal");
        draw_synchronized(&mut terminal, |frame| {
            crate::tui::render::render(frame, &state);
        })
        .expect("full TUI ansi256 draw");

        let written = shared.take_utf8();
        assert!(
            !written.contains("38;2;") && !written.contains("48;2;"),
            "full TUI frame under Ansi256 must not emit any 24-bit RGB SGR codes"
        );
        assert!(
            written.contains("38;5;") && written.contains("48;5;"),
            "expected quantized 256-color SGR codes in full TUI frame"
        );
        assert!(written.contains("deepseek-flash"));
        assert!(written.contains("merry resume"));
    }
}
