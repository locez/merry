use crate::tui::{
    controller::ControllerEffect,
    overlay::{Overlay, OverlayKeyResult},
    state::{CommandView, TimelineItem, TuiState},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use merry_core::ToolCallId;

pub(crate) use layout::{details_pointer, handle_mouse_scroll, prepare_viewport};
pub(crate) use output::CapturedOutput;
pub(crate) use render::render_details;

mod ansi;
mod highlight;
mod layout;
mod output;
mod render;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandNavigation {
    Latest,
    Previous,
    Next,
    Index(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamPane {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DetailsOutput {
    Loading,
    Ready(CapturedOutput),
    Failed(String),
}

/// Where the open command sits in the live finished-command list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HistoryPlace {
    /// 0-based index. Absent when this command is no longer finished.
    index: Option<usize>,
    total: usize,
}

impl HistoryPlace {
    pub(super) fn from_indices(indices: &[usize], timeline_index: usize) -> Self {
        Self {
            index: indices.iter().position(|index| *index == timeline_index),
            total: indices.len(),
        }
    }

    pub(super) fn resolve(state: &TuiState, timeline_index: usize) -> Self {
        Self::from_indices(&finished_command_indices(state), timeline_index)
    }

    pub(super) fn show_navigation(self) -> bool {
        self.total > 1
    }

    pub(super) fn has_previous(self) -> bool {
        self.index.is_some_and(|index| index > 0)
    }

    pub(super) fn has_next(self) -> bool {
        self.index.is_some_and(|index| index + 1 < self.total)
    }

    /// Centers the history rail. A missing command does not claim the last row.
    pub(super) fn rail_index(self) -> Option<usize> {
        self.index
    }

    pub(super) fn label(self) -> String {
        match self.index {
            Some(index) => format!("{}/{}", index + 1, self.total),
            None => format!("?/{}", self.total),
        }
    }
}

/// An on-demand view of one completed command, backed by its runtime tool output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandDetails {
    timeline_index: usize,
    view: CommandView,
    output: DetailsOutput,
    stdout_scroll: usize,
    stderr_scroll: usize,
    active_pane: StreamPane,
    feedback: Option<String>,
    layout: Option<layout::DetailsLayout>,
}

impl CommandDetails {
    fn new(timeline_index: usize, view: CommandView) -> Self {
        Self {
            timeline_index,
            view,
            output: DetailsOutput::Loading,
            stdout_scroll: 0,
            stderr_scroll: 0,
            active_pane: StreamPane::Stdout,
            feedback: None,
            layout: None,
        }
    }

    pub(crate) fn call_id(&self) -> Option<&ToolCallId> {
        match &self.view {
            CommandView::Finished { call_id, .. } => Some(call_id),
            CommandView::Running { .. } => None,
        }
    }

    pub(crate) fn set_feedback(&mut self, feedback: String) {
        self.feedback = Some(feedback);
    }

    pub(crate) fn focus_pane(&mut self, pane: StreamPane) {
        self.active_pane = pane;
    }

    fn has_split_streams(&self) -> bool {
        matches!(
            &self.output,
            DetailsOutput::Ready(CapturedOutput::Process { stdout, stderr })
                if stream_has_body(stdout) && stream_has_body(stderr)
        )
    }

    fn toggle_pane(&mut self) {
        if self.has_split_streams() {
            self.active_pane = match self.active_pane {
                StreamPane::Stdout => StreamPane::Stderr,
                StreamPane::Stderr => StreamPane::Stdout,
            };
        }
    }

    fn active_scroll_mut(&mut self) -> &mut usize {
        match shown_pane(&self.output, self.active_pane) {
            StreamPane::Stdout => &mut self.stdout_scroll,
            StreamPane::Stderr => &mut self.stderr_scroll,
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> OverlayKeyResult {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return OverlayKeyResult::Consumed;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q' | 'Q') => OverlayKeyResult::Close,
            KeyCode::Left | KeyCode::Char('[' | 'a' | 'A' | 'h' | 'H') | KeyCode::BackTab => {
                OverlayKeyResult::PreviousCommand
            }
            KeyCode::Right | KeyCode::Char(']' | 'd' | 'D' | 'l' | 'L') | KeyCode::Tab => {
                OverlayKeyResult::NextCommand
            }
            KeyCode::Char('e' | 'E') => {
                self.toggle_pane();
                OverlayKeyResult::Consumed
            }
            KeyCode::Char('1') => {
                if pane_shown(&self.output, StreamPane::Stdout) {
                    self.focus_pane(StreamPane::Stdout);
                }
                OverlayKeyResult::Consumed
            }
            KeyCode::Char('2') => {
                if pane_shown(&self.output, StreamPane::Stderr) {
                    self.focus_pane(StreamPane::Stderr);
                }
                OverlayKeyResult::Consumed
            }
            KeyCode::Char('c' | 'C') => {
                if let CommandView::Finished { command, .. } = &self.view {
                    OverlayKeyResult::CopyText(command.clone())
                } else {
                    OverlayKeyResult::Consumed
                }
            }
            KeyCode::Char('y' | 'Y') => match &self.output {
                DetailsOutput::Ready(output) => OverlayKeyResult::CopyText(output.copy_text()),
                _ => {
                    self.set_feedback("Output is not available yet".into());
                    OverlayKeyResult::Consumed
                }
            },
            KeyCode::Up | KeyCode::Char('w' | 'W' | 'k' | 'K') => {
                let scroll = self.active_scroll_mut();
                *scroll = scroll.saturating_sub(1);
                OverlayKeyResult::Consumed
            }
            KeyCode::Down | KeyCode::Char('s' | 'S' | 'j' | 'J') => {
                let scroll = self.active_scroll_mut();
                *scroll = scroll.saturating_add(1);
                OverlayKeyResult::Consumed
            }
            KeyCode::PageUp | KeyCode::Backspace => {
                let scroll = self.active_scroll_mut();
                *scroll = scroll.saturating_sub(10);
                OverlayKeyResult::Consumed
            }
            KeyCode::PageDown | KeyCode::Char(' ') => {
                let scroll = self.active_scroll_mut();
                *scroll = scroll.saturating_add(10);
                OverlayKeyResult::Consumed
            }
            KeyCode::Home | KeyCode::Char('g') => {
                *self.active_scroll_mut() = 0;
                OverlayKeyResult::Consumed
            }
            KeyCode::End | KeyCode::Char('G') => {
                *self.active_scroll_mut() = usize::MAX;
                OverlayKeyResult::Consumed
            }
            _ => OverlayKeyResult::Consumed,
        }
    }
}

pub(super) fn stream_has_body(stream: &output::CapturedStream) -> bool {
    !stream.text.is_empty() || stream.truncated || stream.utf8 == Some(false)
}

/// Pane that keyboard scrolling and the title percent actually move.
///
/// A single captured buffer, a loading state, and an error are stdout.
/// A process with only one stream ignores a focus key for the empty side.
pub(super) fn shown_pane(output: &DetailsOutput, requested: StreamPane) -> StreamPane {
    match output {
        DetailsOutput::Ready(CapturedOutput::Process { stdout, stderr }) => {
            let stdout_body = stream_has_body(stdout);
            let stderr_body = stream_has_body(stderr);
            if stdout_body && stderr_body {
                requested
            } else if stderr_body {
                StreamPane::Stderr
            } else {
                StreamPane::Stdout
            }
        }
        _ => StreamPane::Stdout,
    }
}

fn pane_shown(output: &DetailsOutput, pane: StreamPane) -> bool {
    match output {
        DetailsOutput::Ready(CapturedOutput::Process { stdout, stderr }) => match pane {
            StreamPane::Stdout => stream_has_body(stdout),
            StreamPane::Stderr => stream_has_body(stderr),
        },
        _ => pane == StreamPane::Stdout,
    }
}

pub(super) fn finished_command_indices(state: &TuiState) -> Vec<usize> {
    state
        .timeline()
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            matches!(
                item,
                TimelineItem::Command {
                    view: CommandView::Finished { .. }
                }
            )
            .then_some(index)
        })
        .collect()
}

/// Selects a completed command without changing the draft, runtime, or timeline viewport.
pub(crate) fn inspect_command(
    state: &mut TuiState,
    direction: CommandNavigation,
) -> ControllerEffect {
    let indices = finished_command_indices(state);
    if indices.is_empty() {
        state.show_info_dialog(
            "Command output",
            "No completed commands yet. Running commands will be available after completion."
                .into(),
        );
        return ControllerEffect::None;
    }
    let current = match state.overlay() {
        Some(Overlay::CommandDetails(details)) => indices
            .iter()
            .position(|index| *index == details.timeline_index),
        _ => None,
    };
    let position = match (current, direction) {
        (_, CommandNavigation::Index(target)) => target.min(indices.len() - 1),
        (Some(position), CommandNavigation::Next) => {
            position.saturating_add(1).min(indices.len() - 1)
        }
        (Some(position), CommandNavigation::Previous) => position.saturating_sub(1),
        _ => indices.len() - 1,
    };
    if current == Some(position) {
        return ControllerEffect::None;
    }
    let index = indices[position];
    let TimelineItem::Command { view } = &state.timeline()[index] else {
        return ControllerEffect::None;
    };
    let details = CommandDetails::new(index, view.clone());
    let Some(call_id) = details.call_id().cloned() else {
        return ControllerEffect::None;
    };
    state.open_command_details(details);
    ControllerEffect::LoadCommandOutput(call_id)
}

/// Ignores stale loads after closing the viewer or selecting a different command.
pub(crate) fn apply_output(
    state: &mut TuiState,
    call_id: &ToolCallId,
    result: Result<CapturedOutput, String>,
) {
    if let Some(Overlay::CommandDetails(details)) = state.overlay_mut()
        && details.call_id() == Some(call_id)
    {
        details.layout = None;
        details.feedback = None;
        details.stdout_scroll = 0;
        details.stderr_scroll = 0;
        details.active_pane = match &result {
            Ok(CapturedOutput::Process { stdout, stderr })
                if !stream_has_body(stdout) && stream_has_body(stderr) =>
            {
                StreamPane::Stderr
            }
            _ => StreamPane::Stdout,
        };
        details.output = match result {
            Ok(output) => DetailsOutput::Ready(output),
            Err(error) => DetailsOutput::Failed(error),
        };
    }
}

/// Reads only the selected tool call output, with a bounded wait and no execution side effects.
pub(crate) async fn load_output(
    runtime: &merry_runtime::Runtime,
    call_id: &ToolCallId,
) -> Result<CapturedOutput, String> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runtime.read_tool_output(call_id),
    )
    .await
    .map_err(|_| "Reading the command output timed out".to_owned())?
    .map_err(|error| error.to_string())?
    .ok_or_else(|| format!("Tool output for call '{call_id}' is not available."))?;
    Ok(CapturedOutput::from_tool_output(output))
}
