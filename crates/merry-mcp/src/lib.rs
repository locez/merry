//! HTTP MCP tool integration for Merry.
//!
//! This crate adapts trusted, user-configured MCP servers into runtime-owned
//! Merry tools. It does not change provider boundaries: providers still see
//! only Merry [`merry_core::ToolSpec`] values.

mod adapter;
mod catalog;
mod client;
mod config;
mod connection;
mod diagnostics;
mod discovery;
mod error;
mod name_map;
mod protocol;

pub use adapter::McpToolRegistration;
pub use config::{McpServerConfig, McpServerConfigBuilder};
pub use diagnostics::{McpDiscoveryStage, McpFailureKind, McpServerDiagnostic, McpServerIssue};
pub use discovery::{McpDiscovery, discover_mcp_tools, restore_mcp_tools};
pub use error::{McpError, McpResult};
pub use name_map::{McpToolNameMapping, map_mcp_tool_names};

#[cfg(test)]
mod tests;
