//! Host-tool invocation contracts and completion batches.

use super::run::AgentRun;
use crate::{errors::AgentError, run_result::RunResult};
use merry_core::{
    ErrorInfo, PendingToolCall, ToolCallArguments, ToolCallBatchId, ToolCallId, ToolName,
};
use merry_runtime::ToolExecutionOutcome;
use std::{collections::BTreeMap, fmt};
use thiserror::Error;

/// Content returned by an externally executed tool invocation.
///
/// Tool result content is deliberately limited to the content kinds currently
/// accepted by the runtime continuation contract. Binary and image results
/// can be added here only when the runtime can persist and project them as
/// tool results as well.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInvocationContent {
    kind: ToolInvocationContentKind,
    value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolInvocationContentKind {
    Text,
    Json,
}

impl ToolInvocationContent {
    /// Creates a text result.
    #[must_use]
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            kind: ToolInvocationContentKind::Text,
            value: content.into(),
        }
    }

    /// Creates a JSON result after validating the serialized payload.
    pub fn json(content: impl Into<String>) -> Result<Self, ToolInvocationContentError> {
        let content = content.into();
        let _: serde_json::Value = serde_json::from_str(&content)
            .map_err(|source| ToolInvocationContentError::InvalidJson { source })?;
        Ok(Self {
            kind: ToolInvocationContentKind::Json,
            value: content,
        })
    }
}

/// Failure while validating tool invocation result content.
#[derive(Debug, Error)]
pub enum ToolInvocationContentError {
    /// The supplied JSON text is not a valid JSON value.
    #[error("tool invocation JSON content is invalid: {source}")]
    InvalidJson {
        /// Underlying JSON parser failure.
        #[source]
        source: serde_json::Error,
    },
}

/// One domain-level result returned for an externally executed invocation.
///
/// The caller supplies only the call id, content and, for a domain failure, a
/// stable diagnostic. The runtime generates the artifact id and
/// `ToolCallResult`, then records the result before continuing the model loop.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ToolInvocationResult {
    /// The invocation completed successfully.
    Succeeded {
        /// Provider/model-originated call identifier being resolved.
        call_id: ToolCallId,
        /// Exact text or JSON payload returned by the invocation executor.
        content: ToolInvocationContent,
    },
    /// The invocation ran but returned a domain-level failure.
    Failed {
        /// Provider/model-originated call identifier being resolved.
        call_id: ToolCallId,
        /// Exact text or JSON payload returned by the invocation executor.
        content: ToolInvocationContent,
        /// Stable diagnostic for the model-visible failure.
        diagnostic: ErrorInfo,
    },
}

/// Result of submitting one complete host invocation batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ToolInvocationSubmission {
    /// Every supplied outcome was recorded as submitted.
    Accepted,
    /// Runtime recorded the invocation(s) as failed and can continue the run.
    ///
    /// This is a successful lifecycle transition. The original host result
    /// was rejected, but returning it as an error would make callers cancel a
    /// run that runtime has already recovered.
    RejectedAndRecorded,
}

impl ToolInvocationResult {
    /// Creates a successful invocation result.
    #[must_use]
    pub fn succeeded(call_id: ToolCallId, content: ToolInvocationContent) -> Self {
        Self::Succeeded { call_id, content }
    }

    /// Creates a failed invocation result that the model can inspect.
    #[must_use]
    pub fn failed(
        call_id: ToolCallId,
        content: ToolInvocationContent,
        diagnostic: ErrorInfo,
    ) -> Self {
        Self::Failed {
            call_id,
            content,
            diagnostic,
        }
    }

    /// Returns the provider/model-originated call identifier being resolved.
    #[must_use]
    pub fn call_id(&self) -> &ToolCallId {
        match self {
            Self::Succeeded { call_id, .. } | Self::Failed { call_id, .. } => call_id,
        }
    }

    pub(crate) fn into_runtime(self) -> (ToolCallId, ToolExecutionOutcome) {
        match self {
            Self::Succeeded { call_id, content } => (call_id, content.into_success_outcome()),
            Self::Failed {
                call_id,
                content,
                diagnostic,
            } => (call_id, content.into_failure_outcome(diagnostic)),
        }
    }
}

impl ToolInvocationContent {
    fn into_success_outcome(self) -> ToolExecutionOutcome {
        match self.kind {
            ToolInvocationContentKind::Text => ToolExecutionOutcome::succeeded_text(self.value),
            ToolInvocationContentKind::Json => ToolExecutionOutcome::succeeded_json(self.value),
        }
    }

    fn into_failure_outcome(self, diagnostic: ErrorInfo) -> ToolExecutionOutcome {
        match self.kind {
            ToolInvocationContentKind::Text => {
                ToolExecutionOutcome::failed_text(self.value, diagnostic)
            }
            ToolInvocationContentKind::Json => {
                ToolExecutionOutcome::failed_json(self.value, diagnostic)
            }
        }
    }
}

pub(crate) fn order_tool_invocation_results(
    invocations: &[ToolInvocation],
    results: Vec<ToolInvocationResult>,
) -> Result<Vec<(ToolCallId, ToolExecutionOutcome)>, AgentError> {
    let expected_call_ids = invocations
        .iter()
        .map(|invocation| invocation.id().clone())
        .collect::<Vec<_>>();
    let mut received_call_ids = Vec::with_capacity(results.len());
    let mut results_by_call_id = BTreeMap::new();
    for result in results {
        let call_id = result.call_id().clone();
        received_call_ids.push(call_id.clone());
        if results_by_call_id.insert(call_id, result).is_some() {
            return Err(AgentError::ToolInvocationBatchMismatch {
                expected_call_ids,
                received_call_ids,
            });
        }
    }

    let mut expected_set = expected_call_ids.clone();
    expected_set.sort();
    let mut received_set = results_by_call_id.keys().cloned().collect::<Vec<_>>();
    received_set.sort();
    if expected_set != received_set || expected_call_ids.len() != received_call_ids.len() {
        return Err(AgentError::ToolInvocationBatchMismatch {
            expected_call_ids,
            received_call_ids,
        });
    }

    let mut ordered_results = Vec::with_capacity(invocations.len());
    for invocation in invocations {
        let Some(result) = results_by_call_id.remove(invocation.id()) else {
            return Err(AgentError::ToolInvocationBatchMismatch {
                expected_call_ids,
                received_call_ids,
            });
        };
        ordered_results.push(result.into_runtime());
    }
    Ok(ordered_results)
}

/// One externally executed tool invocation yielded by a [`AgentRun`].
///
/// The request contains only provider-neutral call data. It does not expose a
/// runtime artifact id because result artifact ownership remains in Rust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInvocation {
    call: PendingToolCall,
}

impl ToolInvocation {
    pub(crate) fn new(call: PendingToolCall) -> Self {
        Self { call }
    }

    /// Returns the provider/model-originated call identifier.
    #[must_use]
    pub fn id(&self) -> &ToolCallId {
        self.call.id()
    }

    /// Returns the provider-portable tool name.
    #[must_use]
    pub fn name(&self) -> &ToolName {
        self.call.name()
    }

    /// Returns the validated JSON object arguments.
    #[must_use]
    pub fn arguments(&self) -> &ToolCallArguments {
        self.call.arguments()
    }
}

/// Ordered externally executed tool invocations yielded for one host execution
/// wave.
///
/// A single invocation is represented as a batch of length one so bindings do
/// not need separate single-call and multi-call state machines. Invocation order
/// is the model order. The batch is only a delivery and completion boundary;
/// hosts may execute independent calls concurrently only when their own tool
/// policy allows it, and must submit one result for every invocation before the
/// runtime continues to the next execution wave or model response.
///
/// The batch borrows the run exclusively while it is unresolved. This makes
/// the runtime phase transition explicit in Rust: `next`, `result`, and
/// `cancel` cannot be called on the run until this batch is submitted or
/// cancelled. Dropping an unresolved batch requests cancellation as a final
/// guard against leaving the producer waiting for a result forever.
pub struct ToolInvocationBatch<'a> {
    invocations: Vec<ToolInvocation>,
    batch_id: ToolCallBatchId,
    run: &'a mut AgentRun,
    resolved: bool,
}

impl<'a> ToolInvocationBatch<'a> {
    pub(crate) fn from_batch(
        run: &'a mut AgentRun,
        batch_id: ToolCallBatchId,
        calls: Vec<PendingToolCall>,
    ) -> Option<Self> {
        if calls.is_empty() {
            return None;
        }
        Some(Self {
            invocations: calls.into_iter().map(ToolInvocation::new).collect(),
            batch_id,
            run,
            resolved: false,
        })
    }

    /// Returns invocations in model order within this host execution wave.
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

    /// Submits the complete result set for this batch.
    ///
    /// Results may be supplied in any order. The runtime validates the complete
    /// set and persists it in pending-call order. A correctable rejected
    /// submission leaves this batch active so the caller can fix and retry.
    /// When runtime records the calls as failed to recover the loop, this
    /// returns [`ToolInvocationSubmission::RejectedAndRecorded`] and releases
    /// the lease.
    pub async fn submit(
        &mut self,
        results: Vec<ToolInvocationResult>,
    ) -> Result<ToolInvocationSubmission, AgentError> {
        if self.resolved {
            return Err(AgentError::ToolInvocationBatchResolved);
        }
        let result = self
            .run
            .submit_tool_invocation_results(&self.batch_id, &self.invocations, results)
            .await;
        if result.is_ok() {
            self.resolved = true;
        }
        result
    }

    /// Cancels the run while this batch is awaiting host execution.
    pub async fn cancel(mut self) -> Result<RunResult, AgentError> {
        if self.resolved {
            return Err(AgentError::ToolInvocationBatchResolved);
        }
        self.resolved = true;
        self.run.cancel().await
    }
}

impl fmt::Debug for ToolInvocationBatch<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolInvocationBatch")
            .field("invocations", &self.invocations)
            .finish()
    }
}

impl PartialEq for ToolInvocationBatch<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.invocations == other.invocations
    }
}

impl Eq for ToolInvocationBatch<'_> {}

impl Drop for ToolInvocationBatch<'_> {
    fn drop(&mut self) {
        if !self.resolved {
            self.run.request_cancel();
        }
    }
}
