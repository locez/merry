use crate::cli_error::CliError;
use crate::config::MerryConfig;
use crate::sandbox::ChildHandoff as SandboxChildHandoff;
use keymap::Keymap;
use state::TuiState;
use terminal::TerminalSession;
use theme::TuiTheme;

mod completion;
mod controller;
mod input;
pub(crate) mod keymap;
mod layout;
mod projector;
mod render;
mod runtime;
mod state;
mod terminal;
pub(crate) mod theme;

#[cfg(test)]
mod tests;

pub(crate) async fn run(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    merry_config: Option<&MerryConfig>,
) -> Result<(), CliError> {
    let tui_config = merry_config
        .map(MerryConfig::tui_config)
        .transpose()
        .map_err(crate::cli_error::unexpected)?
        .unwrap_or_default();
    let keymap = Keymap::from_config(&tui_config.keymap).map_err(crate::cli_error::unexpected)?;
    let theme = TuiTheme::from_config(&tui_config.theme).map_err(crate::cli_error::unexpected)?;
    let session = runtime::start_tui_runtime_session(sandbox_child_handoff, merry_config).await?;
    let mut state = TuiState::new(
        session.workspace_root.clone(),
        session.model_label.clone(),
        keymap,
        theme,
    );
    state.set_reasoning_effort_label(Some(session.reasoning_effort_label.clone()));
    state.set_completion_skills(session.skills.clone());
    let terminal = TerminalSession::enter().map_err(crate::cli_error::unexpected)?;
    controller::run_controller(terminal, session, state).await
}
