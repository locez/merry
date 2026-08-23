//! Runtime-neutral backend contracts for the local Web service.

use futures_util::future::BoxFuture;
use merry_core::{ArtifactId, SessionId, TrajectoryEvent, TrajectorySnapshot};
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

/// Exact artifact content returned by the optional inspection endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WebArtifactContent {
    /// Stable artifact identifier.
    artifact_id: ArtifactId,
    /// Provider-neutral artifact kind.
    kind: WebArtifactKind,
    /// Bounded UTF-8 preview for text and JSON artifacts.
    content: Option<String>,
    /// Whether the response was bounded by the Web response limit.
    truncated: bool,
    /// Exact byte length of the stored artifact.
    #[serde(serialize_with = "serialize_u64_as_string")]
    byte_length: u64,
}

fn serialize_u64_as_string<S>(value: &u64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.collect_str(value)
}

impl WebArtifactContent {
    /// Creates a bounded artifact response for the HTTP adapter.
    #[must_use]
    pub fn new(
        artifact_id: ArtifactId,
        kind: WebArtifactKind,
        content: Option<String>,
        truncated: bool,
        byte_length: u64,
    ) -> Self {
        Self {
            artifact_id,
            kind,
            content,
            truncated,
            byte_length,
        }
    }

    /// Borrows the stable artifact identifier.
    #[must_use]
    pub fn artifact_id(&self) -> &ArtifactId {
        &self.artifact_id
    }

    /// Returns the provider-neutral artifact kind.
    #[must_use]
    pub fn kind(&self) -> WebArtifactKind {
        self.kind
    }

    /// Borrows exact text when the artifact is textual.
    #[must_use]
    pub fn content(&self) -> Option<&str> {
        self.content.as_deref()
    }

    /// Returns whether the response was bounded.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Returns the byte length of the stored artifact.
    #[must_use]
    pub fn byte_length(&self) -> u64 {
        self.byte_length
    }
}

/// Artifact categories exposed by the Web inspection boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebArtifactKind {
    /// UTF-8 text.
    Text,
    /// Serialized JSON text.
    Json,
    /// Binary bytes.
    Binary,
    /// Image bytes.
    Image,
    /// Other opaque bytes.
    Other,
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

    /// Reads a bounded session-owned artifact preview for an inspector request.
    fn artifact_content(
        &self,
        session_id: &SessionId,
        artifact_id: &ArtifactId,
    ) -> BoxFuture<'_, Result<WebArtifactContent, WebBackendError>>;
}
