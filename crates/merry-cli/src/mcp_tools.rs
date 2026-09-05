use crate::{cli_error::CliError, config::MerryConfig};
use merry_core::SessionToolCatalog;
use merry_mcp::{McpFailureKind, McpServerDiagnostic, McpServerIssue};
use merry_runtime::RegisteredTool;
use tokio::io::{AsyncWrite, AsyncWriteExt};

pub(crate) enum McpSession<'a> {
    New,
    Resumed { catalog: &'a SessionToolCatalog },
}

pub(crate) struct ConfiguredMcpTools {
    pub(crate) tools: Vec<RegisteredTool>,
    pub(crate) warnings: Vec<McpServerDiagnostic>,
}

impl ConfiguredMcpTools {
    fn new(tools: Vec<RegisteredTool>, warnings: Vec<McpServerDiagnostic>) -> Self {
        Self { tools, warnings }
    }
}

/// Discovers trusted MCP tools from user config and returns runtime tools.
pub(crate) async fn discover_configured_mcp_tools(
    config: Option<&MerryConfig>,
    session: McpSession<'_>,
) -> Result<ConfiguredMcpTools, CliError> {
    let servers = config
        .map(MerryConfig::mcp_servers)
        .transpose()
        .map_err(crate::cli_error::unexpected)?
        .unwrap_or_default();
    let discovery = match session {
        McpSession::New => merry_mcp::discover_mcp_tools(&servers).await,
        McpSession::Resumed { catalog } => merry_mcp::restore_mcp_tools(&servers, catalog).await,
    }
    .map_err(crate::cli_error::unexpected)?;
    let (registrations, warnings) = discovery.into_parts();
    for warning in &warnings {
        tracing::warn!(event = "mcp.startup.degraded", diagnostic = %format_mcp_warning(warning), "MCP startup requires attention");
    }
    Ok(ConfiguredMcpTools::new(
        registrations
            .into_iter()
            .map(|registration| registration.tool)
            .collect(),
        warnings,
    ))
}

pub(crate) fn format_mcp_warning(diagnostic: &McpServerDiagnostic) -> String {
    let detail = match diagnostic.issue() {
        McpServerIssue::Unavailable { stage, failure } => {
            format!("{stage:?}: {}", format_mcp_failure(*failure))
        }
        McpServerIssue::NotConfigured => {
            "server is no longer configured; execution disabled".to_owned()
        }
        McpServerIssue::EndpointChanged => {
            "endpoint identity changed; execution disabled".to_owned()
        }
        McpServerIssue::ToolsDisallowed => "current allowlist disables saved tools".to_owned(),
        McpServerIssue::CatalogChanged => {
            "catalog changed; incompatible tools disabled and new tools omitted".to_owned()
        }
        McpServerIssue::NotInSessionCatalog => {
            "server omitted from this session's frozen catalog".to_owned()
        }
    };
    let suffix = if diagnostic.retained_tools() == 0 {
        "Merry continues; no tools loaded from this server. Start a new session after resolving the issue.".to_owned()
    } else {
        format!(
            "Merry continues; {} saved tool definitions retained, affected tools unavailable. Definitions will not change on reconnect.",
            diagnostic.retained_tools()
        )
    };
    format!("MCP {}: {detail}. {suffix}", diagnostic.server_id())
}

fn format_mcp_failure(failure: McpFailureKind) -> String {
    match failure {
        McpFailureKind::Timeout => "request timed out".to_owned(),
        McpFailureKind::Connection => "could not connect".to_owned(),
        McpFailureKind::SessionExpired => "MCP session expired".to_owned(),
        McpFailureKind::Authentication => {
            "authentication or authorization rejected; check credentials".to_owned()
        }
        McpFailureKind::Http(status) => format!("HTTP {status}"),
        McpFailureKind::Protocol => "invalid or unsupported MCP response".to_owned(),
        McpFailureKind::InvalidToolDefinition => "invalid tool definition".to_owned(),
        McpFailureKind::ResponseTooLarge => "response exceeded the size limit".to_owned(),
        McpFailureKind::Transport => "transport failed".to_owned(),
    }
}

pub(crate) async fn write_startup_warnings(
    writer: &mut (impl AsyncWrite + Unpin),
    warnings: &[McpServerDiagnostic],
) -> std::io::Result<()> {
    for warning in warnings {
        writer
            .write_all(format!("Warning: {}\n", format_mcp_warning(warning)).as_bytes())
            .await?;
    }
    writer.flush().await
}

#[cfg(test)]
mod tests;
