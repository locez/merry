//! Read-only observability and trajectory streaming handles for Merry sessions.

pub use merry_core::{
    SessionId, SubagentActivityPhase, SubagentActivitySnapshot, ToolCallId, ToolOutputPreview,
    TrajectoryEvent, TrajectorySnapshot,
};
pub use merry_runtime::{RuntimeError, SubagentActivityReceiver};
use tokio::sync::broadcast;

/// Read-only handle exposing live trajectory snapshots, event subscriptions,
/// and bounded tool-output previews for one Merry session.
///
/// Surfaces such as the local Web trajectory viewer consume this handle rather
/// than holding a full runtime handle directly.
#[derive(Clone)]
pub struct TrajectoryHandle {
    runtime: merry_runtime::Runtime,
}

impl TrajectoryHandle {
    /// Creates a read-only trajectory handle for a runtime session.
    #[doc(hidden)]
    #[must_use]
    pub fn from_runtime(runtime: merry_runtime::Runtime) -> Self {
        Self { runtime }
    }

    /// Returns the session identifier owned by this handle.
    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        self.runtime.session_id()
    }

    /// Reads the current trajectory snapshot for this session.
    pub async fn trajectory_snapshot(&self) -> Result<TrajectorySnapshot, RuntimeError> {
        self.runtime.trajectory_snapshot().await
    }

    /// Reads the current trajectory snapshot together with a live update
    /// broadcast receiver.
    pub async fn trajectory_subscription(
        &self,
    ) -> Result<(TrajectorySnapshot, broadcast::Receiver<TrajectoryEvent>), RuntimeError> {
        self.runtime.trajectory_subscription().await
    }

    /// Reads a bounded preview of one recorded tool output.
    pub async fn read_tool_output_preview(
        &self,
        call_id: &ToolCallId,
        max_bytes: usize,
    ) -> Result<Option<ToolOutputPreview>, RuntimeError> {
        self.runtime
            .read_tool_output_preview(call_id, max_bytes)
            .await
    }
}
