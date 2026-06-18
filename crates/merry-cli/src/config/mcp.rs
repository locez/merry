use super::{ConfigError, MerryConfig};
use merry_mcp::McpServerConfig;
use serde::Deserialize;
use std::collections::BTreeMap;

impl MerryConfig {
    /// Returns trusted MCP servers configured by the user.
    pub fn mcp_servers(&self) -> Result<Vec<McpServerConfig>, ConfigError> {
        let Some(mcp) = self.raw.mcp.as_ref() else {
            return Ok(Vec::new());
        };

        let mut servers = Vec::with_capacity(mcp.servers.len());
        for (id, server) in &mcp.servers {
            if id.trim().is_empty() {
                return Err(ConfigError::Invalid(
                    "MCP server ids must not be blank".to_owned(),
                ));
            }
            if server.url.trim().is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "mcp.{id}.url must not be blank"
                )));
            }

            let mut builder = McpServerConfig::builder(id.clone(), server.url.clone());
            for (name, value) in &server.headers {
                if name.trim().is_empty() {
                    return Err(ConfigError::Invalid(format!(
                        "mcp.{id}.headers contains a blank header name"
                    )));
                }
                builder = builder.header(name.clone(), value.clone());
            }
            if let Some(tools) = server.tools.as_ref() {
                if tools.iter().any(|tool| tool.trim().is_empty()) {
                    return Err(ConfigError::Invalid(format!(
                        "mcp.{id}.tools entries must not be blank"
                    )));
                }
                builder = builder.tools(tools.clone());
            }
            servers.push(builder.build());
        }
        Ok(servers)
    }
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(super) struct McpToml {
    #[serde(flatten)]
    servers: BTreeMap<String, McpServerToml>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct McpServerToml {
    url: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    tools: Option<Vec<String>>,
}
