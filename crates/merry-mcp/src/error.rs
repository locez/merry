use thiserror::Error;

/// Result alias for MCP integration operations.
pub type McpResult<T> = Result<T, McpError>;

/// Errors produced by MCP discovery and tool execution.
#[derive(Debug, Error)]
pub enum McpError {
    /// HTTP transport failure.
    #[error("MCP HTTP request failed for server {server_id}: {source}")]
    Http {
        server_id: String,
        #[source]
        source: reqwest::Error,
    },
    /// JSON-RPC application-level error.
    #[error("MCP server {server_id} returned JSON-RPC error {code}: {message}")]
    JsonRpc {
        server_id: String,
        code: i64,
        message: String,
        data: Option<String>,
    },
    /// Invalid JSON-RPC response body.
    #[error("MCP server {server_id} returned invalid JSON-RPC response: {message}")]
    InvalidJson { server_id: String, message: String },
    /// Unsupported HTTP response content type.
    #[error("MCP server {server_id} returned unsupported response content type {content_type:?}")]
    UnsupportedContentType {
        server_id: String,
        content_type: Option<String>,
    },
    /// Invalid MCP tool input schema.
    #[error("MCP server {server_id} returned invalid JSON schema for tool {tool_name}: {message}")]
    InvalidToolSchema {
        server_id: String,
        tool_name: String,
        message: String,
    },
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
