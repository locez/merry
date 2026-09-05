/// Trusted HTTP MCP server configuration.
#[derive(Clone, PartialEq, Eq)]
pub struct McpServerConfig {
    id: String,
    url: String,
    headers: Vec<(String, String)>,
    tools: Option<Vec<String>>,
}

impl McpServerConfig {
    pub(crate) fn validated_identity(
        &self,
    ) -> Result<(merry_core::ToolSourceId, merry_core::ToolSourceFingerprint), crate::McpError>
    {
        use reqwest::header::{HeaderName, HeaderValue};
        use sha2::{Digest, Sha256};

        let source = merry_core::ToolSourceId::new(&self.id)?;
        crate::map_mcp_tool_names(&self.id, &[])?;
        let url =
            reqwest::Url::parse(&self.url).map_err(|_| crate::McpError::InvalidConfiguration {
                reason: "MCP endpoint must be a valid absolute HTTP(S) URL",
            })?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(crate::McpError::InvalidConfiguration {
                reason: "MCP endpoint must use HTTP(S), without userinfo or fragments; use headers for authentication",
            });
        }
        for (name, value) in &self.headers {
            let header = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
                crate::McpError::InvalidConfiguration {
                    reason: "invalid MCP HTTP header name",
                }
            })?;
            HeaderValue::from_str(value).map_err(|_| crate::McpError::InvalidConfiguration {
                reason: "invalid MCP HTTP header value",
            })?;
            if matches!(
                header.as_str(),
                "host"
                    | "content-length"
                    | "transfer-encoding"
                    | "mcp-session-id"
                    | "mcp-protocol-version"
            ) {
                return Err(crate::McpError::InvalidConfiguration {
                    reason: "MCP headers must not override transport-owned headers",
                });
            }
        }
        if let Some(tools) = &self.tools {
            for name in tools {
                merry_core::ToolBindingName::new(name)?;
            }
        }
        let fingerprint = merry_core::ToolSourceFingerprint::new(&format!(
            "{:x}",
            Sha256::digest(url.as_str().as_bytes())
        ))?;
        Ok((source, fingerprint))
    }

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
#[derive(Clone, PartialEq, Eq)]
pub struct McpServerConfigBuilder {
    id: String,
    url: String,
    headers: Vec<(String, String)>,
    tools: Option<Vec<String>>,
}

impl std::fmt::Debug for McpServerConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpServerConfig")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for McpServerConfigBuilder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpServerConfigBuilder")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
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
