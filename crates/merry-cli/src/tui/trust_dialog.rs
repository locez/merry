//! Centered first-open dialog for the workspace sandbox.
//!
//! The rows say what they do: read-write, read-only, whether that choice is
//! saved, and exit. The highlight starts on read-only, and "Don't ask again"
//! starts at no.

use crate::cli_error::{CliError, unexpected};
use crate::workspace_trust::{PromptedChoice, prompt_unavailable};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};
use std::path::Path;
use unicode_width::UnicodeWidthStr;

use super::{
    terminal::{TerminalEvent, TerminalSession},
    theme::{SemanticColor, TuiTheme},
};

const ROWS: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrustDialog {
    path: String,
    scope: String,
    selected: usize,
    remember: bool,
    theme: TuiTheme,
}

impl TrustDialog {
    #[cfg(test)]
    pub(crate) fn new(root: &Path) -> Self {
        Self::with_theme(root, TuiTheme::default())
    }

    pub(crate) fn with_theme(root: &Path, theme: TuiTheme) -> Self {
        let git = root.join(".git");
        let scope = if git.is_dir() || git.is_file() {
            "Git repository root. This choice covers the whole repository."
        } else {
            "This directory is not inside a Git repository."
        };
        Self {
            path: root.display().to_string(),
            scope: scope.to_owned(),
            selected: 1,
            remember: false,
            theme,
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Option<PromptedChoice> {
        if key.kind != KeyEventKind::Press {
            return None;
        }
        match key.code {
            KeyCode::Up => {
                self.selected = self.selected.checked_sub(1).unwrap_or(ROWS - 1);
                None
            }
            KeyCode::Down => {
                self.selected = (self.selected + 1) % ROWS;
                None
            }
            KeyCode::Char(' ') if self.selected == 2 => {
                self.remember = !self.remember;
                None
            }
            KeyCode::Enter if self.selected == 2 => {
                self.remember = !self.remember;
                None
            }
            KeyCode::Enter => Some(self.confirm()),
            KeyCode::Esc => Some(PromptedChoice::Exit),
            _ => None,
        }
    }

    pub(crate) fn render(&self, frame: &mut Frame<'_>) {
        let surface = self
            .theme
            .color(SemanticColor::Surface)
            .unwrap_or(Color::Rgb(36, 32, 41));
        let focus = self
            .theme
            .color(SemanticColor::BorderFocus)
            .or_else(|| self.theme.color(SemanticColor::Focus))
            .unwrap_or(Color::LightMagenta);
        let text = self
            .theme
            .color(SemanticColor::Assistant)
            .unwrap_or(Color::White);
        let warning = self
            .theme
            .color(SemanticColor::Warning)
            .unwrap_or(Color::LightYellow);
        let muted = self
            .theme
            .color(SemanticColor::Muted)
            .unwrap_or(Color::DarkGray);

        let area = frame.area();
        frame.render_widget(Clear, area);
        frame.render_widget(Block::default().style(Style::default().bg(surface)), area);

        let region = centered(area, 68, 16);
        frame.render_widget(Clear, region);
        frame.render_widget(
            Block::default()
                .title(" M  Trust this directory? ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(focus).add_modifier(Modifier::BOLD))
                .style(Style::default().bg(surface)),
            region,
        );

        let inner = inset(region, 2, 1);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(4),
                Constraint::Length(1),
            ])
            .split(inner);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(self.path.clone(), Style::default().fg(text))),
                Line::from(""),
                Line::from(Span::styled(
                    "Only continue if you trust the files in this directory.",
                    Style::default().fg(warning),
                )),
                Line::from(Span::styled(self.scope.clone(), Style::default().fg(muted))),
            ])
            .style(Style::default().fg(text).bg(surface))
            .wrap(Wrap { trim: false }),
            rows[0],
        );
        let width = usize::from(rows[1].width);
        let remember = if self.remember { "yes" } else { "no" };
        let choices = [
            ("Read-write", "can edit files here"),
            ("Read-only", "cannot edit files"),
            ("Don't ask again", remember),
            ("Exit", ""),
        ];
        let lines = choices
            .into_iter()
            .enumerate()
            .map(|(index, (label, note))| {
                option_line(
                    label,
                    note,
                    index == self.selected,
                    width,
                    surface,
                    focus,
                    text,
                    muted,
                )
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines), rows[1]);
        frame.render_widget(
            Paragraph::new("↑ ↓ move    Enter confirm    Space toggles don't ask again")
                .alignment(Alignment::Center)
                .style(Style::default().fg(muted).bg(surface)),
            rows[2],
        );
    }

    fn confirm(&self) -> PromptedChoice {
        match self.selected {
            0 => PromptedChoice::Open {
                writable: true,
                remember: self.remember,
            },
            1 => PromptedChoice::Open {
                writable: false,
                remember: self.remember,
            },
            _ => PromptedChoice::Exit,
        }
    }
}

pub(crate) async fn ask(
    terminal: &mut TerminalSession,
    root: &Path,
    theme: &TuiTheme,
) -> Result<PromptedChoice, CliError> {
    let mut dialog = TrustDialog::with_theme(root, theme.clone());
    loop {
        terminal
            .draw(|frame| dialog.render(frame))
            .map_err(unexpected)?;
        let Some(event) = terminal.next_event().await.map_err(unexpected)? else {
            return Ok(PromptedChoice::Exit);
        };
        let TerminalEvent::Key(key) = event else {
            continue;
        };
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(PromptedChoice::Exit);
        }
        if let Some(choice) = dialog.handle_key(key) {
            return Ok(choice);
        }
    }
}

/// Draws the dialog on its own screen, then restores the terminal.
pub(crate) async fn prompt_owned(root: &Path) -> Result<PromptedChoice, CliError> {
    let mut terminal = TerminalSession::enter().map_err(|_| prompt_unavailable(root))?;
    let theme = TuiTheme::default();
    ask(&mut terminal, root, &theme).await
}

#[allow(clippy::too_many_arguments)]
fn option_line(
    label: &str,
    note: &str,
    selected: bool,
    width: usize,
    surface: Color,
    focus: Color,
    text: Color,
    muted: Color,
) -> Line<'static> {
    const LABEL_COLUMN: usize = 15;
    let mark = if selected { "▸ " } else { "  " };
    let label_pad = LABEL_COLUMN.saturating_sub(UnicodeWidthStr::width(label));
    let mut head = format!("{mark}{label}{}", " ".repeat(label_pad));
    let note = if note.is_empty() {
        String::new()
    } else {
        format!("  {note}")
    };
    let used = UnicodeWidthStr::width(head.as_str()) + UnicodeWidthStr::width(note.as_str());
    let tail = " ".repeat(width.saturating_sub(used));
    if selected {
        head.push_str(&note);
        head.push_str(&tail);
        return Line::from(Span::styled(
            head,
            Style::default()
                .fg(Color::Black)
                .bg(focus)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let plain = Style::default().fg(text).bg(surface);
    Line::from(vec![
        Span::styled(head, plain),
        Span::styled(note, Style::default().fg(muted).bg(surface)),
        Span::styled(tail, plain),
    ])
}

fn centered(area: Rect, max_width: u16, desired_height: u16) -> Rect {
    let width = area.width.saturating_sub(4).min(max_width).max(1);
    let height = area.height.saturating_sub(2).min(desired_height).max(1);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn inset(region: Rect, horizontal: u16, vertical: u16) -> Rect {
    Rect::new(
        region.x.saturating_add(horizontal),
        region.y.saturating_add(vertical),
        region.width.saturating_sub(horizontal.saturating_mul(2)),
        region.height.saturating_sub(vertical.saturating_mul(2)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn render(dialog: &TrustDialog) -> (String, ratatui::buffer::Buffer) {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| dialog.render(frame)).expect("render");
        let buffer = terminal.backend().buffer().clone();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        (text, buffer)
    }

    #[test]
    fn dialog_lists_the_four_choices_and_starts_on_read_only() {
        let dialog = TrustDialog::new(Path::new("/work/repo"));
        let (text, buffer) = render(&dialog);
        assert!(text.contains("Trust this directory?"), "{text}");
        assert!(
            text.contains("Only continue if you trust the files in this directory."),
            "{text}"
        );
        assert!(text.contains("/work/repo"), "{text}");
        assert!(text.contains("▸ Read-only"), "{text}");
        assert!(text.contains("can edit files here"), "{text}");
        assert!(text.contains("cannot edit files"), "{text}");
        assert!(!text.contains("AGENTS.md"), "{text}");
        assert!(text.contains("Read-write"), "{text}");
        assert!(text.contains("Exit"), "{text}");
        assert!(!text.contains("▸ Read-write"), "{text}");
        assert!(!text.contains("keeps the sandbox"), "{text}");
        let again = text
            .lines()
            .find(|line| line.contains("Don't ask again"))
            .expect("remember row");
        assert!(again.contains("no"), "{again}");
        assert!(!again.contains("yes"), "{again}");
        let marker = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
            .find(|(x, y)| buffer[(*x, *y)].symbol() == "▸")
            .expect("selected marker");
        let style = buffer[marker].style();
        assert_eq!(style.bg, Some(Color::LightMagenta));
        assert_eq!(style.fg, Some(Color::Black));
    }

    #[test]
    fn enter_on_read_only_does_not_remember_until_the_toggle_is_on() {
        let mut dialog = TrustDialog::new(Path::new("/work/repo"));
        assert_eq!(
            dialog.handle_key(key(KeyCode::Enter)),
            Some(PromptedChoice::Open {
                writable: false,
                remember: false,
            })
        );

        let mut dialog = TrustDialog::new(Path::new("/work/repo"));
        assert_eq!(dialog.handle_key(key(KeyCode::Down)), None);
        assert_eq!(dialog.handle_key(key(KeyCode::Enter)), None);
        assert_eq!(dialog.handle_key(key(KeyCode::Up)), None);
        assert_eq!(
            dialog.handle_key(key(KeyCode::Enter)),
            Some(PromptedChoice::Open {
                writable: false,
                remember: true,
            })
        );
    }

    #[test]
    fn read_write_exit_and_escape_are_explicit() {
        let mut dialog = TrustDialog::new(Path::new("/work/repo"));
        assert_eq!(dialog.handle_key(key(KeyCode::Up)), None);
        assert_eq!(
            dialog.handle_key(key(KeyCode::Enter)),
            Some(PromptedChoice::Open {
                writable: true,
                remember: false,
            })
        );

        let mut dialog = TrustDialog::new(Path::new("/work/repo"));
        assert_eq!(dialog.handle_key(key(KeyCode::Up)), None);
        assert_eq!(dialog.handle_key(key(KeyCode::Up)), None);
        assert_eq!(
            dialog.handle_key(key(KeyCode::Enter)),
            Some(PromptedChoice::Exit)
        );
        assert_eq!(
            TrustDialog::new(Path::new("/work/repo")).handle_key(key(KeyCode::Esc)),
            Some(PromptedChoice::Exit)
        );
    }
}
