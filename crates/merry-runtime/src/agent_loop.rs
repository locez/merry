//! Runtime-owned serial agent loop.
//!
//! The public module is intentionally a narrow facade over focused loop
//! components. Configuration, protocol handles, execution drivers, producer
//! lifecycle, and tool-wave classification live in separate modules so each
//! owns one part of the runtime contract.

mod config;
mod driver;
mod helpers;
mod producer;
mod run;
mod tool_flow;
mod types;

pub use config::{
    AgentLoopConfig, AgentLoopConfigError, DEFAULT_AGENT_LOOP_MAX_MODEL_TURNS,
    StructuredOutputRetryPolicy,
};
pub use run::{AgentRun, AgentRunMessage};
pub use types::{AgentLoopBlockedReason, AgentLoopError, AgentLoopResult, AgentLoopStatus};

pub(crate) use helpers::{
    agent_loop_cancelled_diagnostic, agent_loop_stream_error, agent_loop_stream_error_with_source,
    blocked_reason_code, can_retry_structured_output, collect_step_events, continuation_step_input,
    final_assistant_output_from_step, publish_journal_event, record_final_output_tool_call,
    structured_output_failure_result, take_subagent_notification, take_subagent_notification_input,
    tool_execution_cancelled_diagnostic, tool_resolution_artifact_id,
    tool_resolution_diagnostic_code, tool_resolution_is_policy_denied, tool_resolution_status,
    trace_loop_error, trace_loop_finish, validate_final_output,
};
pub(crate) use tool_flow::{
    PendingLoopToolCall, PendingLoopToolWave, StepOutcome, classify_step_events,
    execute_stream_runtime_batch, next_agent_run_batch_id, receive_and_publish_bridge_tool_results,
    settle_cancelled_bridge_tool_calls, settle_failed_bridge_tool_calls,
};
