use crate::McpError;
use merry_core::ToolSourceId;
use std::fmt;

/// Discovery phase reported without exposing requests, credentials, or response bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpDiscoveryStage {
    /// Initialize and acknowledge the transport session.
    Initialize,
    /// Retrieve and validate the complete tool catalog.
    ListTools,
}

/// Safe failure categories for user-visible MCP diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpFailureKind {
    /// A bounded operation exceeded its deadline.
    Timeout,
    /// A connection could not be established.
    Connection,
    /// The remote endpoint no longer recognizes the MCP transport session.
    SessionExpired,
    /// The endpoint rejected authentication or authorization.
    Authentication,
    /// The endpoint returned an unsuccessful HTTP status.
    Http(u16),
    /// The endpoint did not return a valid supported MCP response.
    Protocol,
    /// One or more tool definitions are invalid.
    InvalidToolDefinition,
    /// A response exceeded the adapter's size limit.
    ResponseTooLarge,
    /// Transport failed without a more specific safe classification.
    Transport,
}

impl McpFailureKind {
    pub(crate) fn from_error(error: &McpError) -> Self {
        match error {
            McpError::Http { source, .. } if source.is_timeout() => Self::Timeout,
            McpError::Http { source, .. } if source.is_connect() => Self::Connection,
            McpError::Http { source, .. } => match source.status().map(|status| status.as_u16()) {
                Some(401 | 403) => Self::Authentication,
                Some(status) => Self::Http(status),
                None if source.is_decode() => Self::Protocol,
                None => Self::Transport,
            },
            McpError::SessionExpired { .. } => Self::SessionExpired,
            McpError::InvalidToolSchema { .. }
            | McpError::InvalidToolName { .. }
            | McpError::Core(_) => Self::InvalidToolDefinition,
            McpError::ResponseTooLarge { .. } => Self::ResponseTooLarge,
            McpError::TransportState { .. } => Self::Transport,
            _ => Self::Protocol,
        }
    }

    pub(crate) fn retryable(self) -> bool {
        matches!(
            self,
            Self::Timeout
                | Self::Connection
                | Self::SessionExpired
                | Self::Transport
                | Self::Http(429 | 500..=599)
        )
    }
}

impl fmt::Display for McpFailureKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => formatter.write_str("request timed out"),
            Self::Connection => formatter.write_str("could not connect"),
            Self::SessionExpired => formatter.write_str("MCP session expired"),
            Self::Authentication => {
                formatter.write_str("authentication or authorization rejected; check credentials")
            }
            Self::Http(status) => write!(formatter, "HTTP {status}"),
            Self::Protocol => formatter.write_str("invalid or unsupported MCP response"),
            Self::InvalidToolDefinition => formatter.write_str("invalid tool definition"),
            Self::ResponseTooLarge => formatter.write_str("response exceeded the size limit"),
            Self::Transport => formatter.write_str("transport failed"),
        }
    }
}

/// A server-level condition which does not prevent the application from starting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpServerIssue {
    /// Startup discovery failed at the given stage.
    Unavailable {
        stage: McpDiscoveryStage,
        failure: McpFailureKind,
    },
    /// The source was removed from the current configuration.
    NotConfigured,
    /// The saved source identity no longer matches the configured endpoint.
    EndpointChanged,
    /// Current configuration revoked access to saved tools.
    ToolsDisallowed,
    /// Existing definitions changed or tools were added; the saved surface stays fixed.
    CatalogChanged,
    /// A configured source has no definitions in this session's catalog.
    NotInSessionCatalog,
}

/// A retained, safe startup diagnostic; tool counts distinguish missing and frozen definitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerDiagnostic {
    server_id: ToolSourceId,
    issue: McpServerIssue,
    retained_tools: usize,
}

impl McpServerDiagnostic {
    /// Creates a safe diagnostic from a validated identity and structured failure.
    #[must_use]
    pub fn new(server_id: ToolSourceId, issue: McpServerIssue, retained_tools: usize) -> Self {
        Self {
            server_id,
            issue,
            retained_tools,
        }
    }

    /// Returns the non-secret configured server identity.
    #[must_use]
    pub fn server_id(&self) -> &ToolSourceId {
        &self.server_id
    }

    /// Returns the condition that requires attention.
    #[must_use]
    pub fn issue(&self) -> &McpServerIssue {
        &self.issue
    }

    /// Returns how many provider-visible definitions were retained for this server.
    #[must_use]
    pub fn retained_tools(&self) -> usize {
        self.retained_tools
    }
}
