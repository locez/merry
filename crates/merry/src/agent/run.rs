//! High-level non-interactive run message and lifecycle contract.

use super::invocation::{
    ToolInvocation, ToolInvocationBatch, ToolInvocationResult, ToolInvocationSubmission,
    order_tool_invocation_results,
};
use crate::{errors::AgentError, run_result::RunResult};
use merry_core::{PendingToolCall, RuntimeEvent, ToolCallBatchId};
use merry_runtime::{AgentRun as RuntimeAgentRun, Runtime};

/// Message returned by the non-interactive agent run.
///
/// A run has one consumption path: callers repeatedly invoke
/// [`AgentRun::next`] for events, and resolve a tool invocation batch before
/// the run can be used again.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AgentRunMessage<'a> {
    /// A durable runtime event.
    Event(Box<RuntimeEvent>),
    /// Ordered host-owned tool invocations that must be executed by the
    /// embedding caller.
    ToolInvocations {
        /// Exclusive batch lease that must be submitted or cancelled before
        /// the run can be used again.
        batch: ToolInvocationBatch<'a>,
    },
}

impl<'a> AgentRunMessage<'a> {
    /// Borrows the runtime event when this message carries one.
    #[must_use]
    pub fn as_event(&self) -> Option<&RuntimeEvent> {
        match self {
            Self::Event(event) => Some(event.as_ref()),
            Self::ToolInvocations { .. } => None,
        }
    }

    /// Borrows the tool invocation batch when this message carries one.
    #[must_use]
    pub fn as_tool_invocations(&self) -> Option<&ToolInvocationBatch<'a>> {
        match self {
            Self::Event(_) => None,
            Self::ToolInvocations { batch } => Some(batch),
        }
    }

    /// Mutably borrows the tool invocation batch when this message carries one.
    #[must_use]
    pub fn as_tool_invocations_mut(&mut self) -> Option<&mut ToolInvocationBatch<'a>> {
        match self {
            Self::Event(_) => None,
            Self::ToolInvocations { batch } => Some(batch),
        }
    }
}

/// Single-consumer handle for one high-level non-interactive run.
///
/// This type intentionally does not implement the futures `Stream` trait. A
/// stream of only runtime events cannot safely represent a tool invocation:
/// consuming the next event could otherwise discard the handoff while the
/// runtime waits for its result. The explicit message method makes the
/// protocol visible to Rust and foreign-language bindings alike.
pub struct AgentRun {
    runtime: Runtime,
    inner: RuntimeAgentRun,
    ended: bool,
    terminal_result: Option<RunResult>,
    terminal_error: Option<AgentError>,
    terminal_error_observed: bool,
    result_consumed: bool,
}

impl AgentRun {
    pub(crate) fn new(runtime: Runtime, inner: RuntimeAgentRun) -> Self {
        Self {
            runtime,
            inner,
            ended: false,
            terminal_result: None,
            terminal_error: None,
            terminal_error_observed: false,
            result_consumed: false,
        }
    }

    pub(crate) fn request_cancel(&mut self) {
        self.inner.request_cancel();
    }

    pub(crate) fn into_owned(self) -> crate::binding::OwnedAgentRun {
        crate::binding::OwnedAgentRun::new(self)
    }

    /// Returns the next event or tool invocation batch.
    ///
    /// `Ok(None)` means the producer reached its terminal boundary. Runtime
    /// method failures are returned as `Err` instead of being hidden behind
    /// end-of-stream. A returned tool invocation batch holds an exclusive
    /// borrow of this run until it is submitted or cancelled.
    pub async fn next_message(&mut self) -> Result<Option<AgentRunMessage<'_>>, AgentError> {
        let Some(message) = self.next_runtime_message().await? else {
            return Ok(None);
        };

        match message {
            merry_runtime::AgentRunMessage::Event(event) => {
                Ok(Some(AgentRunMessage::Event(Box::new(event))))
            }
            merry_runtime::AgentRunMessage::ToolInvocations { batch } => {
                self.tool_invocation_message(batch.id().clone(), batch.calls().to_vec())
            }
            _ => Err(AgentError::AgentRunProtocol {
                message: "runtime emitted an unsupported agent run message",
            }),
        }
    }

    pub(crate) async fn next_owned_message(
        &mut self,
    ) -> Result<Option<crate::binding::OwnedAgentRunMessage>, AgentError> {
        let Some(message) = self.next_runtime_message().await? else {
            return Ok(None);
        };

        match message {
            merry_runtime::AgentRunMessage::Event(event) => Ok(Some(
                crate::binding::OwnedAgentRunMessage::Event(Box::new(event)),
            )),
            merry_runtime::AgentRunMessage::ToolInvocations { batch } => {
                let Some(batch) = crate::binding::OwnedToolInvocationBatch::new(
                    batch.id().clone(),
                    batch.calls().to_vec(),
                ) else {
                    return Err(AgentError::AgentRunProtocol {
                        message: "runtime emitted an empty tool invocation batch",
                    });
                };
                Ok(Some(
                    crate::binding::OwnedAgentRunMessage::ToolInvocations { batch },
                ))
            }
            _ => Err(AgentError::AgentRunProtocol {
                message: "runtime emitted an unsupported agent run message",
            }),
        }
    }

    async fn next_runtime_message(
        &mut self,
    ) -> Result<Option<merry_runtime::AgentRunMessage>, AgentError> {
        if self.ended {
            return Ok(None);
        }

        let Some(message) = self.inner.next_message().await.map_err(AgentError::from)? else {
            self.ended = true;
            match self.finish_inner().await {
                Ok(result) => self.terminal_result = Some(result),
                Err(error) => {
                    self.terminal_error = Some(error);
                    self.terminal_error_observed = true;
                    return match self.terminal_error.take() {
                        Some(error) => Err(error),
                        None => Err(AgentError::AgentRunResultMissing),
                    };
                }
            }
            return Ok(None);
        };
        Ok(Some(message))
    }

    /// Returns the next message using the idiomatic Rust name for this
    /// message-first protocol.
    pub async fn next(&mut self) -> Result<Option<AgentRunMessage<'_>>, AgentError> {
        self.next_message().await
    }

    /// Resolves every invocation in the supplied batch.
    ///
    /// Results may be supplied in any order. The runtime persists them in the
    /// pending-call order for this host wave and only then permits the next
    /// message to be read.
    /// The batch is borrowed so a caller can retry after validation or runtime
    /// rejection.
    pub(crate) async fn submit_tool_invocation_results(
        &mut self,
        batch_id: &ToolCallBatchId,
        invocations: &[ToolInvocation],
        results: Vec<ToolInvocationResult>,
    ) -> Result<ToolInvocationSubmission, AgentError> {
        let ordered_results = order_tool_invocation_results(invocations, results)?;
        match self
            .inner
            .submit_bridge_tool_outcomes(batch_id, ordered_results)
            .await
        {
            Ok(()) => Ok(ToolInvocationSubmission::Accepted),
            Err(merry_runtime::RuntimeError::BridgeToolResultRejected { .. }) => {
                Ok(ToolInvocationSubmission::RejectedAndRecorded)
            }
            Err(error) => Err(AgentError::from(error)),
        }
    }

    /// Returns the terminal result after [`Self::next_message`] returned `Ok(None)`.
    pub async fn result(&mut self) -> Result<RunResult, AgentError> {
        if !self.ended {
            return Err(AgentError::AgentRunNotFinished);
        }
        self.take_terminal_result()
    }

    /// Cancels the run and waits for a durable cancelled terminal result.
    pub async fn cancel(&mut self) -> Result<RunResult, AgentError> {
        if !self.ended {
            self.inner.cancel_and_wait().await;
            self.ended = true;
            match self.finish_inner().await {
                Ok(result) => self.terminal_result = Some(result),
                Err(error) => self.terminal_error = Some(error),
            }
        }
        self.take_terminal_result()
    }

    async fn finish_inner(&mut self) -> Result<RunResult, AgentError> {
        let result = self.inner.result().await.map_err(AgentError::from)?;
        RunResult::from_runtime(&self.runtime, result).await
    }

    fn tool_invocation_message(
        &mut self,
        batch_id: ToolCallBatchId,
        calls: Vec<PendingToolCall>,
    ) -> Result<Option<AgentRunMessage<'_>>, AgentError> {
        let Some(batch) = ToolInvocationBatch::from_batch(self, batch_id, calls) else {
            return Err(AgentError::AgentRunProtocol {
                message: "runtime emitted an empty tool invocation batch",
            });
        };
        Ok(Some(AgentRunMessage::ToolInvocations { batch }))
    }

    fn take_terminal_result(&mut self) -> Result<RunResult, AgentError> {
        if self.result_consumed {
            return Err(AgentError::AgentRunResultConsumed);
        }
        self.result_consumed = true;
        if let Some(result) = self.terminal_result.take() {
            return Ok(result);
        }
        if let Some(error) = self.terminal_error.take() {
            return Err(error);
        }
        if self.terminal_error_observed {
            return Err(AgentError::AgentRunResultConsumed);
        }
        Err(AgentError::AgentRunResultMissing)
    }
}
