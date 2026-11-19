//! Runtime-neutral backend contracts for the local Web service.

use futures_util::future::BoxFuture;
use merry_core::{SessionId, ToolCallId, ToolOutputKind, TrajectoryEvent, TrajectorySnapshot};
use serde::{Serialize, Serializer};
use std::fmt::Debug;
use thiserror::Error;

/// Backend failures exposed by the Web adapter without leaking internal state.
#[derive(Debug, Clone, Error)]
pub enum WebBackendError {
    /// The requested session is not owned by this Web service instance.
    #[error("session is not available")]
    SessionUnavailable,
    /// The backend could not provide a read-model response.
    #[error("Web backend is temporarily unavailable")]
    Unavailable,
    /// The subscriber fell behind and must reconnect from a fresh snapshot.
    #[error("trajectory subscriber requires a fresh snapshot")]
    ResyncRequired,
}

/// Bounded tool output returned by the optional inspection endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WebToolOutput {
    /// Stable tool call identifier.
    call_id: ToolCallId,
    /// Provider-neutral tool output kind.
    kind: ToolOutputKind,
    /// Bounded UTF-8 preview for the stored tool output.
    content: String,
    /// Whether the response was bounded by the Web response limit.
    truncated: bool,
    /// Exact byte length of the stored tool output.
    #[serde(serialize_with = "serialize_u64_as_string")]
    byte_length: u64,
}

fn serialize_u64_as_string<S>(value: &u64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.collect_str(value)
}

impl WebToolOutput {
    /// Creates a bounded tool output response for the HTTP adapter.
    #[must_use]
    pub fn new(
        call_id: ToolCallId,
        kind: ToolOutputKind,
        content: String,
        truncated: bool,
        byte_length: u64,
    ) -> Self {
        Self {
            call_id,
            kind,
            content,
            truncated,
            byte_length,
        }
    }

    /// Borrows the stable tool call identifier.
    #[must_use]
    pub fn call_id(&self) -> &ToolCallId {
        &self.call_id
    }

    /// Returns the provider-neutral tool output kind.
    #[must_use]
    pub fn kind(&self) -> ToolOutputKind {
        self.kind
    }

    /// Borrows the bounded UTF-8 tool output content.
    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Returns whether the response was bounded.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Returns the byte length of the stored tool output.
    #[must_use]
    pub fn byte_length(&self) -> u64 {
        self.byte_length
    }
}

/// Runtime-neutral data adapter consumed by the HTTP service.
pub trait WebBackend: Send + Sync + 'static {
    /// Reads the current trajectory snapshot for one session.
    fn trajectory_snapshot(
        &self,
        session_id: &SessionId,
    ) -> BoxFuture<'_, Result<TrajectorySnapshot, WebBackendError>>;

    /// Creates a stream whose first item is a snapshot followed by updates.
    fn trajectory_stream(
        &self,
        session_id: &SessionId,
    ) -> BoxFuture<
        '_,
        Result<
            futures_util::stream::BoxStream<'static, Result<TrajectoryEvent, WebBackendError>>,
            WebBackendError,
        >,
    >;

    /// Reads a bounded session-owned tool output preview for an inspector request.
    fn tool_output(
        &self,
        session_id: &SessionId,
        call_id: &ToolCallId,
    ) -> BoxFuture<'_, Result<Option<WebToolOutput>, WebBackendError>>;
}
