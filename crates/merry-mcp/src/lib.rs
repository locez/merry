//! HTTP MCP tool integration for Merry.
//!
//! This crate adapts trusted, user-configured MCP servers into runtime-owned
//! Merry tools. It does not change provider boundaries: providers still see
//! only Merry [`merry_core::ToolSpec`] values.

mod adapter;
mod client;
mod config;
mod error;
mod name_map;
mod protocol;

pub use adapter::{McpToolRegistration, discover_mcp_tools};
pub use config::{McpServerConfig, McpServerConfigBuilder};
pub use error::{McpError, McpResult};
pub use name_map::{McpToolNameMapping, map_mcp_tool_names};
