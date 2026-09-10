use crate::tui::{
    controller::ControllerEffect,
    overlay::{Overlay, OverlayKeyResult},
    state::{CommandView, TimelineItem, TuiState},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use merry_core::ArtifactId;

pub(crate) use output::CapturedOutput;
pub(crate) use render::{prepare_viewport, render_details};

mod output;
mod render;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandNavigation {
    Latest,
    Previous,
    Next,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DetailsOutput {
    Loading,
    Ready(CapturedOutput),
    Failed(String),
}

/// An on-demand view of one completed command, backed by its runtime artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandDetails {
    timeline_index: usize,
    ordinal: usize,
    total: usize,
    view: CommandView,
    output: DetailsOutput,
    scroll: usize,
    feedback: Option<String>,
    layout: Option<render::DetailsLayout>,
}

impl CommandDetails {
    fn new(timeline_index: usize, ordinal: usize, total: usize, view: CommandView) -> Self {
        Self {
            timeline_index,
            ordinal,
            total,
            view,
            output: DetailsOutput::Loading,
            scroll: 0,
            feedback: None,
            layout: None,
        }
    }

    pub(crate) fn artifact_id(&self) -> Option<&ArtifactId> {
        match &self.view {
            CommandView::Finished { artifact, .. } => Some(artifact.id()),
            CommandView::Running { .. } => None,
        }
    }

    pub(crate) fn set_feedback(&mut self, feedback: String) {
        self.feedback = Some(feedback);
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> OverlayKeyResult {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return OverlayKeyResult::Consumed;
        }
        match key.code {
            KeyCode::Esc => OverlayKeyResult::Close,
            KeyCode::Left | KeyCode::Char('[') => OverlayKeyResult::PreviousCommand,
            KeyCode::Right | KeyCode::Char(']') => OverlayKeyResult::NextCommand,
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
            KeyCode::Up => {
                self.scroll = self.scroll.saturating_sub(1);
                OverlayKeyResult::Consumed
            }
            KeyCode::Down => {
                self.scroll = self.scroll.saturating_add(1);
                OverlayKeyResult::Consumed
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(10);
                OverlayKeyResult::Consumed
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_add(10);
                OverlayKeyResult::Consumed
            }
            KeyCode::Home => {
                self.scroll = 0;
                OverlayKeyResult::Consumed
            }
            KeyCode::End => {
                self.scroll = usize::MAX;
                OverlayKeyResult::Consumed
            }
            _ => OverlayKeyResult::Consumed,
        }
    }
}

/// Selects a completed command without changing the draft, runtime, or timeline viewport.
pub(crate) fn inspect_command(
    state: &mut TuiState,
    direction: CommandNavigation,
) -> ControllerEffect {
    let indices = state
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
        .collect::<Vec<_>>();
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
    let details = CommandDetails::new(index, position + 1, indices.len(), view.clone());
    let Some(artifact_id) = details.artifact_id().cloned() else {
        return ControllerEffect::None;
    };
    state.open_command_details(details);
    ControllerEffect::LoadCommandOutput(artifact_id)
}

/// Ignores stale loads after closing the viewer or selecting a different command.
pub(crate) fn apply_output(
    state: &mut TuiState,
    artifact_id: &ArtifactId,
    result: Result<CapturedOutput, String>,
) {
    if let Some(Overlay::CommandDetails(details)) = state.overlay_mut()
        && details.artifact_id() == Some(artifact_id)
    {
        details.layout = None;
        details.feedback = None;
        details.output = match result {
            Ok(output) => DetailsOutput::Ready(output),
            Err(error) => DetailsOutput::Failed(error),
        };
    }
}

/// Reads only the selected artifact, with a bounded wait and no execution side effects.
pub(crate) async fn load_output(
    runtime: &merry_runtime::Runtime,
    artifact_id: &ArtifactId,
) -> Result<CapturedOutput, String> {
    let content = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runtime.read_artifact_content(artifact_id),
    )
    .await
    .map_err(|_| "Reading the command output timed out".to_owned())?
    .map_err(|error| error.to_string())?;
    CapturedOutput::from_artifact(content)
}
