//! High-level interactive SDK handles.
//!
//! Runtime owns the session, pending-call ledger, and lifecycle. This module
//! only translates runtime messages into the same semantic invocation/result
//! types used by the non-interactive facade.

use crate::{
    agent::{
        ToolInvocation, ToolInvocationResult, ToolInvocationSubmission,
        order_tool_invocation_results,
    },
    errors::AgentError,
};
use merry_core::{RuntimeEvent, ToolCallBatchId};
use merry_runtime::{
    AgentLoopControl, AgentLoopInput, InteractiveAgentRun as RuntimeInteractiveRun,
    InteractiveInputItem as RuntimeInteractiveInputItem,
    InteractiveInputSnapshot as RuntimeInteractiveInputSnapshot,
    InteractiveRunEventStream as RuntimeInteractiveStream,
    InteractiveRunMessage as RuntimeInteractiveMessage,
};
use std::fmt;

/// Interactive run handle composed from runtime-owned input, control, and
/// output capabilities.
pub struct InteractiveRun {
    inner: RuntimeInteractiveRun,
}

impl InteractiveRun {
    pub(crate) fn new(inner: RuntimeInteractiveRun) -> Self {
        Self { inner }
    }

    /// Splits the run into its single-consumer output stream and shared input
    /// and control handles.
    #[must_use]
    pub fn split(self) -> (InteractiveEventStream, InteractiveInput, InteractiveControl) {
        let (stream, input, control) = self.inner.split();
        (InteractiveEventStream { inner: stream }, input, control)
    }
}

/// Single-consumer interactive output stream.
pub struct InteractiveEventStream {
    inner: RuntimeInteractiveStream,
}

impl InteractiveEventStream {
    /// Returns the next runtime event or host tool invocation batch.
    ///
    /// Use this explicit message path only for a host-owned tool handoff. A
    /// returned batch holds an exclusive mutable borrow of this stream until
    /// it is submitted or cancelled.
    #[doc(hidden)]
    pub async fn next_message(&mut self) -> Result<Option<InteractiveMessage<'_>>, AgentError> {
        let message = self.inner.next_message().await.map_err(AgentError::from)?;
        match message {
            Some(RuntimeInteractiveMessage::Event(event)) => {
                Ok(Some(InteractiveMessage::Event(Box::new(event))))
            }
            Some(RuntimeInteractiveMessage::ToolInvocations { batch }) => {
                let invocations = batch
                    .calls()
                    .iter()
                    .cloned()
                    .map(ToolInvocation::new)
                    .collect();
                Ok(Some(InteractiveMessage::ToolInvocations {
                    batch: InteractiveToolInvocationBatch {
                        invocations,
                        batch_id: batch.id().clone(),
                        stream: self,
                        resolved: false,
                    },
                }))
            }
            None => Ok(None),
            _ => Err(AgentError::InteractiveProtocol {
                message: "runtime emitted an unsupported interactive message",
            }),
        }
    }

    /// Returns the next durable runtime event.
    ///
    /// Ordinary Rust tools are executed by runtime and do not appear in this
    /// stream. An unexpected host handoff is reported as an error; use
    /// [`Self::next_message`] for the explicit host protocol.
    pub async fn next(&mut self) -> Result<Option<RuntimeEvent>, AgentError> {
        self.next_event().await
    }

    /// Returns the next event when the run has no host tool handoff.
    pub async fn next_event(&mut self) -> Result<Option<RuntimeEvent>, AgentError> {
        self.inner.next_event().await.map_err(AgentError::from)
    }

    /// Drains the stream through its terminal closed event.
    pub async fn wait_until_closed(&mut self) -> Result<(), AgentError> {
        self.inner
            .wait_until_closed()
            .await
            .map_err(AgentError::from)
    }
}

/// Message returned by an interactive facade stream.
#[non_exhaustive]
pub enum InteractiveMessage<'a> {
    /// A durable runtime event.
    Event(Box<RuntimeEvent>),
    /// Ordered host-owned tool calls that must be resolved as one batch.
    ToolInvocations {
        /// Exclusive invocation batch borrowed from the output stream.
        batch: InteractiveToolInvocationBatch<'a>,
    },
}

impl InteractiveMessage<'_> {
    /// Borrows the event when this is an event message.
    #[must_use]
    pub fn as_event(&self) -> Option<&RuntimeEvent> {
        match self {
            Self::Event(event) => Some(event.as_ref()),
            Self::ToolInvocations { .. } => None,
        }
    }
}

/// Stable semantic invocation type used by interactive and non-interactive
/// host handoffs.
pub type InteractiveToolInvocation = ToolInvocation;

/// Exclusive batch lease for interactive host tool calls.
pub struct InteractiveToolInvocationBatch<'a> {
    invocations: Vec<InteractiveToolInvocation>,
    batch_id: ToolCallBatchId,
    stream: &'a mut InteractiveEventStream,
    resolved: bool,
}

impl InteractiveToolInvocationBatch<'_> {
    /// Returns invocations in model order.
    #[must_use]
    pub fn invocations(&self) -> &[InteractiveToolInvocation] {
        &self.invocations
    }

    /// Returns the number of calls in the batch.
    #[must_use]
    pub fn len(&self) -> usize {
        self.invocations.len()
    }

    /// Returns whether the batch contains no calls.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.invocations.is_empty()
    }

    /// Submits the complete host result set.
    ///
    /// Results may be supplied in any order. Runtime validates and persists
    /// them in model order. A correctable rejected submission leaves this lease
    /// active so the caller can fix and retry it. If runtime has already
    /// recorded the calls as failed, this returns
    /// [`ToolInvocationSubmission::RejectedAndRecorded`] and releases the
    /// lease.
    pub async fn submit(
        &mut self,
        results: Vec<ToolInvocationResult>,
    ) -> Result<ToolInvocationSubmission, AgentError> {
        if self.resolved {
            return Err(AgentError::ToolInvocationBatchResolved);
        }

        let ordered_results = order_tool_invocation_results(&self.invocations, results)?;

        let result = self
            .stream
            .inner
            .submit_tool_invocation_outcomes(&self.batch_id, ordered_results)
            .await
            .map_err(AgentError::from);
        match result {
            Ok(()) => {
                self.resolved = true;
                Ok(ToolInvocationSubmission::Accepted)
            }
            Err(AgentError::Runtime {
                source: merry_runtime::RuntimeError::BridgeToolResultRejected { .. },
            }) => {
                self.resolved = true;
                Ok(ToolInvocationSubmission::RejectedAndRecorded)
            }
            Err(error) => Err(error),
        }
    }
}

impl fmt::Debug for InteractiveToolInvocationBatch<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InteractiveToolInvocationBatch")
            .field("invocations", &self.invocations)
            .finish()
    }
}

impl Drop for InteractiveToolInvocationBatch<'_> {
    fn drop(&mut self) {
        if !self.resolved {
            self.stream.inner.request_cancel();
        }
    }
}

/// Stable facade alias for the interactive input handle.
pub type InteractiveInput = AgentLoopInput;
/// Stable facade alias for the interactive control handle.
pub type InteractiveControl = AgentLoopControl;
/// Stable facade alias for an accepted interactive input item.
pub type InteractiveInputItem = RuntimeInteractiveInputItem;
/// Stable facade alias for an interactive input snapshot.
pub type InteractiveInputSnapshot = RuntimeInteractiveInputSnapshot;
