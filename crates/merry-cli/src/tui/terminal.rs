use crossterm::{
    cursor::{Hide, Show},
    event::{DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyEvent},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::StreamExt;
use ratatui::{Frame, Terminal, backend::CrosstermBackend};
use std::io::{self, Stdout, stdout};

pub(crate) enum TerminalEvent {
    Key(KeyEvent),
    Resize,
}

#[allow(dead_code)]
pub(crate) struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    events: EventStream,
}

impl TerminalSession {
    #[allow(dead_code)]
    pub(crate) fn enter() -> io::Result<Self> {
        enable_raw_mode()?;

        let mut stdout = stdout();
        if let Err(err) = execute!(stdout, EnterAlternateScreen, EnableBracketedPaste, Hide) {
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
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
    }
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(stdout(), Show, DisableBracketedPaste, LeaveAlternateScreen);
}
