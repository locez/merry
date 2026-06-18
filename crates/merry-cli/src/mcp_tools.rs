use crate::{cli_error::CliError, config::MerryConfig};
use merry_runtime::RegisteredTool;

/// Discovers trusted MCP tools from user config and returns runtime tools.
pub(crate) async fn discover_configured_mcp_tools(
    config: Option<&MerryConfig>,
) -> Result<Vec<RegisteredTool>, CliError> {
    let Some(config) = config else {
        return Ok(Vec::new());
    };
    let servers = config
        .mcp_servers()
        .map_err(|error| CliError::Unexpected(error.to_string()))?;
    if servers.is_empty() {
        return Ok(Vec::new());
    }

    let registrations = merry_mcp::discover_mcp_tools(&servers)
        .await
        .map_err(|error| CliError::Unexpected(error.to_string()))?;
    Ok(registrations
        .into_iter()
        .map(|registration| registration.tool)
        .collect())
}
