//! Public high-level agent facade.
//!
//! Construction, host invocation contracts, and run lifecycle are kept in
//! focused child modules while this root preserves the stable facade path.

mod core;
mod invocation;
mod run;

pub use crate::run_result::RunResult;
pub use core::{Agent, AgentBuilder};
pub(crate) use invocation::order_tool_invocation_results;
pub use invocation::{
    ToolInvocation, ToolInvocationBatch, ToolInvocationContent, ToolInvocationContentError,
    ToolInvocationResult, ToolInvocationSubmission,
};
pub use run::{AgentRun, AgentRunMessage};
