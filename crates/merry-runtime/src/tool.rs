//! Runtime-owned tool execution API and registry.
//!
//! [`ToolExecutor`] is an outcome-only boundary. Executors should run tool
//! infrastructure and return [`ToolExecutionOutcome`]; they should not call
//! runtime mutation APIs as callbacks. [`crate::Runtime::execute_tool_call`]
//! already owns the active runtime step permit while the executor runs, so
//! reentrant mutation attempts are rejected by normal step admission.
//!
//! Tool calls and results are provider-neutral Merry values. Provider adapters
//! render tool specs and continuations into provider wire formats outside this
//! crate.

pub use executor::{
    ToolActionPreflight, ToolActionProposalFuture, ToolActionProposalResult, ToolExecutionContext,
    ToolExecutionError, ToolExecutionOutcome, ToolExecutionResult, ToolExecutor,
    ToolExecutorFuture,
};
pub use patch_evidence::{
    WorkspacePatchChangeEvidence, WorkspacePatchExecutionEvidence, WorkspacePatchProposal,
};
pub use proposal::{
    ActionExecutionEvidence, ActionProposal, ActionProposalError, ActionProposalEvidence,
    ToolActionKind,
};
pub use registry::{RegisteredTool, ToolConcurrency, ToolRunner};
pub(crate) use registry::{ToolRegistry, ToolRegistryError};

mod executor;
mod patch_evidence;
mod proposal;
mod registry;

#[cfg(test)]
mod tests;
