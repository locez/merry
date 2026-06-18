use crate::{McpResult, McpServerConfig};
use merry_runtime::RegisteredTool;

/// A registered MCP tool plus its provider-visible Merry name mapping.
#[derive(Debug)]
pub struct McpToolRegistration {
    pub tool: RegisteredTool,
    pub server_id: String,
    pub raw_tool_name: String,
}

/// Discovers tools exposed by configured MCP servers.
pub async fn discover_mcp_tools(
    _servers: &[McpServerConfig],
) -> McpResult<Vec<McpToolRegistration>> {
    Ok(Vec::new())
}
