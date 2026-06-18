use thiserror::Error;

/// Result alias for MCP integration operations.
pub type McpResult<T> = Result<T, McpError>;

/// Errors produced by MCP discovery and tool execution.
#[derive(Debug, Error)]
pub enum McpError {
    /// The configured server id could not be represented in a Merry tool name.
    #[error("invalid MCP server id `{id}`: {message}")]
    InvalidServerId { id: String, message: String },
    /// A raw MCP tool name could not be mapped into Merry's tool-name grammar.
    #[error("invalid MCP tool name `{name}` from server `{server_id}`: {message}")]
    InvalidToolName {
        server_id: String,
        name: String,
        message: String,
    },
}
