//! Event-only Rust SDK streams.
//!
//! Host-owned tool handoff remains available through [`crate::__internal::AgentRun`]. The
//! normal facade stream is intentionally narrower: runtime executes ordinary
//! Rust tools and this module exposes only durable runtime events.

use crate::{
    agent::{AgentRun, AgentRunMessage, RunResult},
    errors::AgentError,
};
use merry_core::RuntimeEvent;
use serde::de::DeserializeOwned;
use std::marker::PhantomData;

/// Single-consumer event-only stream for a non-interactive agent run.
pub struct AgentEventStream {
    inner: AgentRun,
}

impl AgentEventStream {
    pub(crate) fn new(inner: AgentRun) -> Self {
        Self { inner }
    }

    /// Returns the next durable runtime event.
    ///
    /// Ordinary Rust tools are executed inside runtime and do not appear in
    /// this stream. An unexpected host handoff returns an error and requests
    /// cancellation; use `Agent::stream_with_tool_handoff` for that protocol.
    pub async fn next(&mut self) -> Result<Option<RuntimeEvent>, AgentError> {
        match self.inner.next_message().await? {
            None => Ok(None),
            Some(AgentRunMessage::Event(event)) => Ok(Some(*event)),
            Some(AgentRunMessage::ToolInvocations { batch }) => {
                drop(batch);
                Err(AgentError::ToolHandoffRequired)
            }
        }
    }

    /// Alias for [`Self::next`] that emphasizes the event-only contract.
    pub async fn next_event(&mut self) -> Result<Option<RuntimeEvent>, AgentError> {
        self.next().await
    }

    /// Returns the terminal result after [`Self::next`] returned `Ok(None)`.
    pub async fn result(&mut self) -> Result<RunResult, AgentError> {
        self.inner.result().await
    }

    /// Cancels the run and waits for its durable terminal result.
    pub async fn cancel(&mut self) -> Result<RunResult, AgentError> {
        self.inner.cancel().await
    }
}

/// Structured-output variant of the explicit host-tool handoff stream.
pub struct StructuredAgentRun<T> {
    inner: AgentRun,
    marker: PhantomData<fn() -> T>,
}

impl<T> StructuredAgentRun<T> {
    pub(crate) fn new(inner: AgentRun) -> Self {
        Self {
            inner,
            marker: PhantomData,
        }
    }

    /// Returns the next runtime event or host tool invocation batch.
    pub async fn next_message(&mut self) -> Result<Option<AgentRunMessage<'_>>, AgentError> {
        self.inner.next_message().await
    }

    /// Returns the next message using the idiomatic Rust name.
    pub async fn next(&mut self) -> Result<Option<AgentRunMessage<'_>>, AgentError> {
        self.next_message().await
    }

    /// Returns the terminal result and decodes its structured output.
    pub async fn result(&mut self) -> Result<StructuredRunResult<T>, AgentError>
    where
        T: DeserializeOwned,
    {
        StructuredRunResult::from_run(self.inner.result().await?)
    }

    /// Cancels the producer and decodes its structured terminal result.
    pub async fn cancel(&mut self) -> Result<StructuredRunResult<T>, AgentError>
    where
        T: DeserializeOwned,
    {
        StructuredRunResult::from_run(self.inner.cancel().await?)
    }
}

/// Structured-output variant of [`AgentEventStream`].
pub struct StructuredAgentEventStream<T> {
    inner: AgentEventStream,
    marker: PhantomData<fn() -> T>,
}

impl<T> StructuredAgentEventStream<T> {
    pub(crate) fn new(inner: AgentEventStream) -> Self {
        Self {
            inner,
            marker: PhantomData,
        }
    }

    /// Returns the next durable runtime event.
    pub async fn next(&mut self) -> Result<Option<RuntimeEvent>, AgentError> {
        self.inner.next().await
    }

    /// Alias for [`Self::next`] that emphasizes the event-only contract.
    pub async fn next_event(&mut self) -> Result<Option<RuntimeEvent>, AgentError> {
        self.next().await
    }

    /// Returns and decodes the terminal structured result after EOF.
    pub async fn result(&mut self) -> Result<StructuredRunResult<T>, AgentError>
    where
        T: DeserializeOwned,
    {
        StructuredRunResult::from_run(self.inner.result().await?)
    }

    /// Cancels the run and decodes its structured terminal result.
    pub async fn cancel(&mut self) -> Result<StructuredRunResult<T>, AgentError>
    where
        T: DeserializeOwned,
    {
        StructuredRunResult::from_run(self.inner.cancel().await?)
    }
}

/// Structured-output variant of [`RunResult`].
#[derive(Debug)]
pub struct StructuredRunResult<T> {
    run: RunResult,
    output: T,
}

impl<T> StructuredRunResult<T>
where
    T: DeserializeOwned,
{
    pub(crate) fn from_run(run: RunResult) -> Result<Self, AgentError> {
        let Some(final_output_json) = run
            .final_output_json()
            .map(|final_output| final_output.json().to_owned())
        else {
            return Err(AgentError::StructuredOutputNotRecorded { run: Box::new(run) });
        };
        let output = match serde_json::from_str(&final_output_json) {
            Ok(output) => output,
            Err(source) => {
                return Err(AgentError::StructuredOutputDecode {
                    run: Box::new(run),
                    source,
                });
            }
        };
        Ok(Self { run, output })
    }

    /// Borrows the decoded structured value.
    #[must_use]
    pub fn output(&self) -> &T {
        &self.output
    }

    /// Borrows the underlying high-level run result.
    #[must_use]
    pub fn run(&self) -> &RunResult {
        &self.run
    }

    /// Consumes the wrapper and returns the run result and decoded value.
    #[must_use]
    pub fn into_parts(self) -> (RunResult, T) {
        (self.run, self.output)
    }
}
