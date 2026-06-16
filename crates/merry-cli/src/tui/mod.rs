use crate::cli_error::CliError;
use crate::config::MerryConfig;
use crate::sandbox::ChildHandoff as SandboxChildHandoff;

mod input;
pub(crate) mod keymap;
mod projector;
mod runtime;
mod state;
pub(crate) mod theme;

#[cfg(test)]
mod tests;

pub(crate) async fn run(
    _sandbox_child_handoff: Option<SandboxChildHandoff>,
    _merry_config: Option<&MerryConfig>,
) -> Result<(), CliError> {
    Err(CliError::Unexpected(
        "TUI is not implemented yet; use `merry run`, `merry cmd`, or `merry debug`".to_owned(),
    ))
}
