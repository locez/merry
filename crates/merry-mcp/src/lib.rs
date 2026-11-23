//! Dynamic HTTP MCP tool integration for Merry.
//!
//! This crate adapts trusted, user-configured MCP servers into a pair of fixed
//! provider-visible meta-tools ([`MCP_SEARCH_TOOL`] and [`MCP_CALL_TOOL`]).
//! Keeping the provider-visible tool surface constant preserves prompt/KV-cache
//! prefix stability while allowing MCP servers to connect, reconnect, add
//! tools, or update schemas dynamically within a single session.

mod adapter;
mod catalog;
mod client;
mod config;
mod connection;
mod diagnostics;
mod discovery;
mod error;
mod registry;

pub use adapter::{MCP_CALL_TOOL, MCP_SEARCH_TOOL, McpToolRegistration};
pub use config::{McpServerConfig, McpServerConfigBuilder};
pub use diagnostics::{McpDiscoveryStage, McpFailureKind, McpServerDiagnostic, McpServerIssue};
pub use discovery::{McpDiscovery, discover_mcp_tools};
pub use error::{McpError, McpResult};

#[cfg(test)]
mod tests;
