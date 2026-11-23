use crate::{
    McpError, McpResult, McpServerConfig,
    adapter::{McpToolRegistration, meta_tool_registrations},
    connection::{McpConnection, STARTUP_TIMEOUT},
    diagnostics::{McpServerDiagnostic, McpServerIssue},
    registry::McpRegistry,
};
use futures_util::{StreamExt, stream};
use std::{collections::BTreeMap, sync::Arc};
use tokio::time::Instant;

/// Initial MCP discovery results.
///
/// Network and remote-protocol failures are reported as non-fatal diagnostics;
/// failed servers remain registered in the dynamic registry so `mcp_search` and
/// `mcp_call` can connect later in the same session once they come online.
#[derive(Debug)]
pub struct McpDiscovery {
    registrations: Vec<McpToolRegistration>,
    diagnostics: Vec<McpServerDiagnostic>,
}

impl McpDiscovery {
    fn new(registrations: Vec<McpToolRegistration>, diagnostics: Vec<McpServerDiagnostic>) -> Self {
        Self {
            registrations,
            diagnostics,
        }
    }

    /// Returns fixed MCP meta-tool registrations (`mcp_search`, `mcp_call`), or
    /// empty when no MCP servers are configured.
    #[must_use]
    pub fn registrations(&self) -> &[McpToolRegistration] {
        &self.registrations
    }

    /// Returns safe diagnostics which the caller should present to the user.
    #[must_use]
    pub fn diagnostics(&self) -> &[McpServerDiagnostic] {
        &self.diagnostics
    }

    /// Transfers tool registrations and diagnostics to the application surface.
    #[must_use]
    pub fn into_parts(self) -> (Vec<McpToolRegistration>, Vec<McpServerDiagnostic>) {
        (self.registrations, self.diagnostics)
    }
}

/// Validates configured MCP servers, runs bounded initial discovery, and
/// returns fixed dynamic meta-tools (`mcp_search` and `mcp_call`).
///
/// Local configuration errors are fatal before network IO. Remote failures emit
/// non-fatal diagnostics while keeping every configured server available for
/// dynamic reconnection and refresh during the session. Dropping this future
/// cancels startup discovery; no background task is spawned.
pub async fn discover_mcp_tools(servers: &[McpServerConfig]) -> McpResult<McpDiscovery> {
    if servers.is_empty() {
        return Ok(McpDiscovery::new(Vec::new(), Vec::new()));
    }

    let mut connections = BTreeMap::new();
    for config in servers {
        let connection = Arc::new(McpConnection::new(config.clone())?);
        if connections
            .insert(connection.source().clone(), connection)
            .is_some()
        {
            return Err(McpError::InvalidConfiguration {
                reason: "duplicate MCP server ids",
            });
        }
    }

    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let results = stream::iter(connections.values().cloned())
        .map(|connection| async move {
            (
                connection.source().clone(),
                connection.discover(deadline).await,
            )
        })
        .buffer_unordered(4)
        .collect::<BTreeMap<_, _>>()
        .await;

    let mut diagnostics = Vec::new();
    for (source, result) in results {
        if let Err(failure) = result {
            diagnostics.push(McpServerDiagnostic::new(
                source,
                McpServerIssue::Unavailable {
                    stage: failure.stage,
                    failure: failure.kind,
                },
            ));
        }
    }

    let registry = Arc::new(McpRegistry::new(connections));
    let registrations = meta_tool_registrations(registry)?;
    Ok(McpDiscovery::new(registrations, diagnostics))
}
