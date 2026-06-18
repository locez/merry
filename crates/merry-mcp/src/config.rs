/// Trusted HTTP MCP server configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerConfig {
    id: String,
    url: String,
    headers: Vec<(String, String)>,
    tools: Option<Vec<String>>,
}

impl McpServerConfig {
    /// Creates a builder for one trusted HTTP MCP server.
    #[must_use]
    pub fn builder(id: impl Into<String>, url: impl Into<String>) -> McpServerConfigBuilder {
        McpServerConfigBuilder {
            id: id.into(),
            url: url.into(),
            headers: Vec::new(),
            tools: None,
        }
    }

    /// Returns the stable config id used for tool-name namespacing.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the Streamable HTTP endpoint URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Returns trusted static HTTP headers.
    #[must_use]
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    /// Returns an optional allowlist of raw MCP tool names.
    #[must_use]
    pub fn tools(&self) -> Option<&[String]> {
        self.tools.as_deref()
    }
}

/// Builder for [`McpServerConfig`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerConfigBuilder {
    id: String,
    url: String,
    headers: Vec<(String, String)>,
    tools: Option<Vec<String>>,
}

impl McpServerConfigBuilder {
    /// Adds one trusted static HTTP header.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Restricts discovery to the provided raw MCP tool names.
    #[must_use]
    pub fn tools(mut self, tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tools = Some(tools.into_iter().map(Into::into).collect());
        self
    }

    /// Builds the server config.
    #[must_use]
    pub fn build(self) -> McpServerConfig {
        McpServerConfig {
            id: self.id,
            url: self.url,
            headers: self.headers,
            tools: self.tools,
        }
    }
}
