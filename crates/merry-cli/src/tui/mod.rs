use crate::cli_error::CliError;
use crate::config::MerryConfig;
use crate::sandbox::ChildHandoff as SandboxChildHandoff;
use keymap::Keymap;
use state::TuiState;
use terminal::TerminalSession;
use theme::TuiTheme;

mod controller;
mod input;
pub(crate) mod keymap;
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
    let session = runtime::start_tui_runtime_session(sandbox_child_handoff, merry_config).await?;
    let state = TuiState::new(
        session.workspace_root.clone(),
        session.model_label.clone(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let terminal = TerminalSession::enter().map_err(crate::cli_error::unexpected)?;
    controller::run_controller(terminal, session, state).await
}
