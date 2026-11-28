//! Dynamic HTTP Model Context Protocol (MCP) client discovery and tool
//! integration.

pub use merry_mcp::{
    MCP_CALL_TOOL, MCP_SEARCH_TOOL, McpDiscovery, McpDiscoveryStage, McpError, McpFailureKind,
    McpResult, McpServerConfig, McpServerConfigBuilder, McpServerDiagnostic, McpServerIssue,
    McpToolRegistration, discover_mcp_tools,
};
use merry_runtime::RegisteredTool;

/// Discovered MCP meta-tools and non-fatal startup diagnostics ready to attach
/// to a Merry coding session or runtime.
#[derive(Default)]
pub struct McpToolset {
    tools: Vec<RegisteredTool>,
    warnings: Vec<McpServerDiagnostic>,
}

impl McpToolset {
    /// Discovers tools from the supplied trusted MCP server configurations.
    pub async fn discover(servers: &[McpServerConfig]) -> McpResult<Self> {
        let discovery = discover_mcp_tools(servers).await?;
        Ok(Self::from_discovery(discovery))
    }

    /// Wraps an already-completed [`McpDiscovery`] result.
    #[must_use]
    pub fn from_discovery(discovery: McpDiscovery) -> Self {
        let (registrations, warnings) = discovery.into_parts();
        Self {
            tools: registrations
                .into_iter()
                .map(|registration| registration.tool)
                .collect(),
            warnings,
        }
    }

    /// Returns the number of discovered MCP meta-tools in this toolset.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Returns `true` when no MCP meta-tools were discovered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Returns non-fatal diagnostics for servers that could not initialize.
    #[must_use]
    pub fn warnings(&self) -> &[McpServerDiagnostic] {
        &self.warnings
    }

    /// Returns the runtime tools without exposing [`RegisteredTool`] to callers
    /// outside the facade.
    #[doc(hidden)]
    #[must_use]
    pub fn into_parts(self) -> (Vec<RegisteredTool>, Vec<McpServerDiagnostic>) {
        (self.tools, self.warnings)
    }
}
