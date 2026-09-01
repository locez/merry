//! Owned, provider-neutral contracts for foreign-language bindings.
//!
//! This module contains no PyO3 types. It adapts the Rust facade's
//! lifetime-bearing host handoff into an owned protocol while keeping the
//! pending batch lease, ordering checks, artifact ownership, and terminal
//! lifecycle in the facade/runtime layers.

pub use crate::agent::{
    ToolInvocation, ToolInvocationContent, ToolInvocationContentError, ToolInvocationResult,
    ToolInvocationSubmission,
};
use crate::{AgentError, RunResult, RuntimeEvent, agent::AgentRun};
use merry_core::{PendingToolCall, ToolCallBatchId};

/// An owned message returned by a foreign-language agent run.
#[non_exhaustive]
#[derive(Debug, PartialEq, Eq)]
pub enum OwnedAgentRunMessage {
    /// A durable runtime event.
    Event(Box<RuntimeEvent>),
    /// An ordered host-owned tool invocation batch.
    ToolInvocations {
        /// The batch must be submitted or cancelled before the run advances.
        batch: OwnedToolInvocationBatch,
    },
}

impl OwnedAgentRunMessage {
    /// Borrows the runtime event when this is an event message.
    #[must_use]
    pub fn as_event(&self) -> Option<&RuntimeEvent> {
        match self {
            Self::Event(event) => Some(event.as_ref()),
            Self::ToolInvocations { .. } => None,
        }
    }

    /// Borrows the tool invocation batch when this is a handoff message.
    #[must_use]
    pub fn as_tool_invocations(&self) -> Option<&OwnedToolInvocationBatch> {
        match self {
            Self::Event(_) => None,
            Self::ToolInvocations { batch } => Some(batch),
        }
    }
}

/// An owned ordered batch of host-executed tool invocations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedToolInvocationBatch {
    batch_id: ToolCallBatchId,
    invocations: Vec<ToolInvocation>,
}

impl OwnedToolInvocationBatch {
    pub(crate) fn new(batch_id: ToolCallBatchId, calls: Vec<PendingToolCall>) -> Option<Self> {
        if calls.is_empty() {
            return None;
        }
        Some(Self {
            batch_id,
            invocations: calls.into_iter().map(ToolInvocation::new).collect(),
        })
    }

    /// Returns the runtime-owned batch identifier.
    #[must_use]
    pub fn id(&self) -> &ToolCallBatchId {
        &self.batch_id
    }

    /// Returns invocations in model order.
    #[must_use]
    pub fn invocations(&self) -> &[ToolInvocation] {
        &self.invocations
    }

    /// Returns the number of invocations in this batch.
    #[must_use]
    pub fn len(&self) -> usize {
        self.invocations.len()
    }

    /// Returns whether this batch contains no invocations.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.invocations.is_empty()
    }
}

/// Owned single-consumer run handle intended for foreign-language bindings.
///
/// The handle exposes no runtime storage. It only owns the facade run and the
/// current batch view needed to bridge a language boundary without Rust
/// lifetimes. Rust continues to validate every submitted call and owns the
/// durable terminal result.
pub struct OwnedAgentRun {
    inner: AgentRun,
    pending_batch: Option<OwnedToolInvocationBatch>,
    terminal_result: Option<RunResult>,
}

impl OwnedAgentRun {
    pub(crate) fn new(inner: AgentRun) -> Self {
        Self {
            inner,
            pending_batch: None,
            terminal_result: None,
        }
    }

    /// Returns the next event or host-tool invocation batch.
    pub async fn next_message(&mut self) -> Result<Option<OwnedAgentRunMessage>, AgentError> {
        if self.pending_batch.is_some() {
            return Err(AgentError::ToolInvocationBatchPending);
        }

        let message = self.inner.next_owned_message().await?;
        if let Some(OwnedAgentRunMessage::ToolInvocations { batch }) = &message {
            self.pending_batch = Some(batch.clone());
        }
        Ok(message)
    }

    /// Alias for [`Self::next_message`].
    pub async fn next(&mut self) -> Result<Option<OwnedAgentRunMessage>, AgentError> {
        self.next_message().await
    }

    /// Submits the complete result set for the active batch.
    ///
    /// The batch id and every call id are checked by the Rust facade. A
    /// correctable validation error leaves the batch active for retry.
    pub async fn submit_tool_invocation_results(
        &mut self,
        batch_id: &ToolCallBatchId,
        results: Vec<ToolInvocationResult>,
    ) -> Result<ToolInvocationSubmission, AgentError> {
        let Some(batch) = self.pending_batch.as_ref() else {
            return Err(AgentError::ToolInvocationBatchNotPending);
        };
        let invocations = batch.invocations.clone();
        let result = self
            .inner
            .submit_tool_invocation_results(batch_id, &invocations, results)
            .await;
        if result.is_ok() {
            self.pending_batch = None;
        }
        result
    }

    /// Returns the terminal result after the run reaches EOF.
    ///
    /// A successful result is retained and returned again on repeated calls so
    /// a binding can recover after cancellation of a concurrent operation.
    pub async fn result(&mut self) -> Result<RunResult, AgentError> {
        if let Some(result) = self.terminal_result.as_ref() {
            return Ok(result.clone());
        }
        let result = self.inner.result().await?;
        self.terminal_result = Some(result.clone());
        Ok(result)
    }

    /// Cancels the run and waits for its durable terminal result.
    ///
    /// A successful result is retained and returned again on repeated calls.
    pub async fn cancel(&mut self) -> Result<RunResult, AgentError> {
        if let Some(result) = self.terminal_result.as_ref() {
            return Ok(result.clone());
        }
        self.pending_batch = None;
        let result = self.inner.cancel().await?;
        self.terminal_result = Some(result.clone());
        Ok(result)
    }
}

impl From<AgentRun> for OwnedAgentRun {
    fn from(run: AgentRun) -> Self {
        run.into_owned()
    }
}
