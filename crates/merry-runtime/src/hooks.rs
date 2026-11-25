//! Runtime-owned lifecycle hooks and turn-control policy contracts.
//!
//! Hooks let Rust applications and foreign-language SDK hosts intercept model
//! steps and tool executions with typed decisions (`Allow`, `ModifyInput`,
//! `ModifyOutput`, `Deny`, `ShortCircuit`, `ContinueWithInput`, `StopTurn`).
//!
//! Unsubscribed hook points and non-matching tool filters incur zero channel
//! traffic and zero event emission: the runtime evaluates hooks only when an
//! installed handler or explicit bridge subscription matches the current point.

#[path = "hooks/bridge.rs"]
pub(crate) mod bridge;
#[path = "hooks/registry.rs"]
mod registry;
#[path = "hooks/wave.rs"]
pub(crate) mod wave;

pub use registry::HookRegistry;

use merry_core::{
    ErrorInfo, SessionId, SessionUsage, ToolCallArguments, ToolCallId, ToolCallResultStatus,
    ToolName, ToolOutput,
};
use merry_llm::GenerationConfig;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fmt, future::Future, pin::Pin};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Lifecycle boundary where a runtime hook can intercept execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookPoint {
    /// Before compiling and sending a model request for the next turn.
    BeforeModelStep,
    /// After a model step finishes, before tool execution or turn completion.
    AfterModelStep,
    /// After tool arguments are received, before executing a tool call.
    BeforeToolCall,
    /// After a tool call executes, before committing its durable result.
    AfterToolCall,
}

impl HookPoint {
    /// Returns the stable snake_case protocol label for this hook point.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BeforeModelStep => "before_model_step",
            Self::AfterModelStep => "after_model_step",
            Self::BeforeToolCall => "before_tool_call",
            Self::AfterToolCall => "after_tool_call",
        }
    }
}

impl fmt::Display for HookPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Runtime-assigned identifier for one host-bridged hook invocation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HookInvocationId(String);

impl HookInvocationId {
    /// Creates a validated hook invocation identifier.
    pub fn new(value: &str) -> Result<Self, HookError> {
        if value.trim().is_empty() || value.trim() != value || value.chars().any(char::is_control) {
            return Err(HookError::InvalidDecision {
                message: "hook invocation id must be non-blank and free of control characters"
                    .to_owned(),
            });
        }
        Ok(Self(value.to_owned()))
    }

    pub(crate) fn from_sequence(sequence: u64) -> Self {
        Self(format!("hook-{sequence}"))
    }

    /// Borrows the identifier text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HookInvocationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Filter selecting which tools trigger a tool-level hook point.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ToolHookFilter {
    /// No tools are subscribed; the hook point is skipped with zero overhead.
    #[default]
    None,
    /// Every tool call triggers the hook point.
    All,
    /// Only tool calls whose name appears in this set trigger the hook point.
    Only(BTreeSet<ToolName>),
}

impl ToolHookFilter {
    /// Returns whether the filter subscribes to at least one tool.
    #[must_use]
    pub fn is_active(&self) -> bool {
        match self {
            Self::None => false,
            Self::All => true,
            Self::Only(names) => !names.is_empty(),
        }
    }

    /// Returns whether `tool_name` matches this filter.
    #[must_use]
    pub fn matches(&self, tool_name: &ToolName) -> bool {
        match self {
            Self::None => false,
            Self::All => true,
            Self::Only(names) => names.contains(tool_name),
        }
    }

    /// Merges another filter into this filter using set union semantics.
    pub fn union_with(&mut self, other: &Self) {
        match (&mut *self, other) {
            (Self::All, _) | (_, Self::None) => {}
            (_, Self::All) => *self = Self::All,
            (Self::None, Self::Only(names)) => {
                if !names.is_empty() {
                    *self = Self::Only(names.clone());
                }
            }
            (Self::Only(existing), Self::Only(names)) => {
                existing.extend(names.iter().cloned());
            }
        }
    }
}

/// Declarative set of hook points and tool filters active for a handler or bridge.
///
/// The runtime checks subscriptions before building hook contexts or emitting
/// host-bridge invocations, so unregistered hooks add zero runtime traffic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookSubscriptions {
    before_model_step: bool,
    after_model_step: bool,
    before_tool_call: ToolHookFilter,
    after_tool_call: ToolHookFilter,
}

impl HookSubscriptions {
    /// Creates an empty subscription set with all hook points disabled.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Enables or disables `before_model_step` hook dispatch.
    #[must_use]
    pub const fn with_before_model_step(mut self, enabled: bool) -> Self {
        self.before_model_step = enabled;
        self
    }

    /// Enables or disables `after_model_step` hook dispatch.
    #[must_use]
    pub const fn with_after_model_step(mut self, enabled: bool) -> Self {
        self.after_model_step = enabled;
        self
    }

    /// Sets the tool filter for `before_tool_call` hook dispatch.
    #[must_use]
    pub fn with_before_tool_call(mut self, filter: ToolHookFilter) -> Self {
        self.before_tool_call = filter;
        self
    }

    /// Sets the tool filter for `after_tool_call` hook dispatch.
    #[must_use]
    pub fn with_after_tool_call(mut self, filter: ToolHookFilter) -> Self {
        self.after_tool_call = filter;
        self
    }

    /// Returns whether no hook points are subscribed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.before_model_step
            && !self.after_model_step
            && !self.before_tool_call.is_active()
            && !self.after_tool_call.is_active()
    }

    /// Returns whether `before_model_step` is subscribed.
    #[must_use]
    pub const fn matches_before_model_step(&self) -> bool {
        self.before_model_step
    }

    /// Returns whether `after_model_step` is subscribed.
    #[must_use]
    pub const fn matches_after_model_step(&self) -> bool {
        self.after_model_step
    }

    /// Returns whether `before_tool_call` is subscribed for `tool_name`.
    #[must_use]
    pub fn matches_before_tool_call(&self, tool_name: &ToolName) -> bool {
        self.before_tool_call.matches(tool_name)
    }

    /// Returns whether `after_tool_call` is subscribed for `tool_name`.
    #[must_use]
    pub fn matches_after_tool_call(&self, tool_name: &ToolName) -> bool {
        self.after_tool_call.matches(tool_name)
    }

    /// Merges `other` into `self` for composite [`RuntimeHookHandler`] implementations.
    pub fn merge_with(&mut self, other: &Self) {
        self.before_model_step |= other.before_model_step;
        self.after_model_step |= other.after_model_step;
        self.before_tool_call.union_with(&other.before_tool_call);
        self.after_tool_call.union_with(&other.after_tool_call);
    }
}

/// Read-only context supplied to a `before_model_step` hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BeforeModelStepContext {
    /// Owning session identity.
    pub session_id: SessionId,
    /// One-based model turn index about to execute in this run/burst.
    pub model_turn: usize,
    /// Configured turn budget for the current loop.
    pub max_model_turns: usize,
    /// Latest recorded session usage snapshot, when available.
    pub session_usage: Option<SessionUsage>,
    /// Active generation controls that will be sent unless overridden.
    pub generation_config: GenerationConfig,
}

/// Decision returned by a `before_model_step` hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum BeforeModelDecision {
    /// Proceed with the model step, with optional per-step dynamic overrides.
    Continue {
        /// Optional replacement generation controls for this single model step.
        #[serde(default)]
        generation_override: Option<GenerationConfig>,
        /// Optional dynamic steering note appended after the cached prefix.
        #[serde(default)]
        dynamic_steering_note: Option<String>,
        /// Requests a best-effort compaction pass before sending this step.
        #[serde(default)]
        force_compact: bool,
    },
    /// Stop the current turn cleanly without calling the model provider.
    StopTurn {
        /// Actionable human-readable reason recorded in loop status.
        reason: String,
        /// Optional explicit final text returned to the caller.
        #[serde(default)]
        final_output: Option<String>,
    },
}

impl BeforeModelDecision {
    /// Proceeds with no modifications.
    #[must_use]
    pub fn continue_normally() -> Self {
        Self::Continue {
            generation_override: None,
            dynamic_steering_note: None,
            force_compact: false,
        }
    }

    /// Stops the current turn before calling the model.
    #[must_use]
    pub fn stop_turn(reason: impl Into<String>, final_output: Option<String>) -> Self {
        Self::StopTurn {
            reason: reason.into(),
            final_output,
        }
    }
}

/// Read-only context supplied to an `after_model_step` hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AfterModelStepContext {
    /// Owning session identity.
    pub session_id: SessionId,
    /// One-based model turn index that just finished.
    pub model_turn: usize,
    /// Configured turn budget for the current loop.
    pub max_model_turns: usize,
    /// Assistant text produced during this step, when present.
    pub assistant_text: Option<String>,
    /// Ordered tool names requested by the model in this step (empty if final).
    pub pending_tool_names: Vec<ToolName>,
    /// Whether the model completed without requesting further tool calls.
    pub is_final_turn: bool,
    /// Latest recorded session usage snapshot, when available.
    pub session_usage: Option<SessionUsage>,
}

/// Decision returned by an `after_model_step` hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum AfterModelDecision {
    /// Accept the model step outcome and follow default runtime control flow.
    Accept,
    /// Continue the loop with a dynamic control note even when the model wanted to stop.
    ContinueWithInput {
        /// Non-blank feedback text delivered as a dynamic loop-control message.
        text: String,
    },
    /// Stop the current turn cleanly (settling any pending tool calls first).
    StopTurn {
        /// Actionable human-readable reason recorded in loop status.
        reason: String,
        /// Optional explicit final output overriding the model's step text.
        #[serde(default)]
        final_output: Option<String>,
    },
}

impl AfterModelDecision {
    /// Accepts default runtime control flow.
    #[must_use]
    pub const fn accept() -> Self {
        Self::Accept
    }

    /// Forces another model turn with feedback text.
    #[must_use]
    pub fn continue_with(text: impl Into<String>) -> Self {
        Self::ContinueWithInput { text: text.into() }
    }

    /// Stops the current turn cleanly.
    #[must_use]
    pub fn stop_turn(reason: impl Into<String>, final_output: Option<String>) -> Self {
        Self::StopTurn {
            reason: reason.into(),
            final_output,
        }
    }
}

/// Read-only context supplied to a `before_tool_call` hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BeforeToolCallContext {
    /// Owning session identity.
    pub session_id: SessionId,
    /// One-based model turn index that produced this tool call.
    pub model_turn: usize,
    /// Model-assigned tool call identifier.
    pub call_id: ToolCallId,
    /// Tool name requested by the model.
    pub tool_name: ToolName,
    /// JSON object arguments for this tool call.
    pub arguments: ToolCallArguments,
    /// Whether this tool executes on the host bridge rather than inside Rust runtime.
    pub is_bridge_tool: bool,
}

/// Decision returned by a `before_tool_call` hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum BeforeToolDecision {
    /// Proceed with the tool call unchanged.
    Allow,
    /// Replace the tool call arguments before execution (re-validated against schema).
    ModifyInput {
        /// Replacement JSON object arguments.
        arguments: ToolCallArguments,
    },
    /// Skip execution and record a failed tool result so the model can recover.
    Deny {
        /// Denial message surfaced to the model as a failed tool result.
        message: String,
    },
    /// Skip execution and record a synthetic succeeded tool output.
    ShortCircuit {
        /// Synthetic tool output recorded as the call's durable result.
        output: ToolOutput,
    },
    /// Skip this call and all remaining calls in the batch, settling them cleanly and stopping the turn.
    StopTurn {
        /// Actionable human-readable reason recorded in loop status.
        reason: String,
        /// Optional explicit final text returned to the caller.
        #[serde(default)]
        final_output: Option<String>,
    },
}

impl BeforeToolDecision {
    /// Allows the tool call to proceed unchanged.
    #[must_use]
    pub const fn allow() -> Self {
        Self::Allow
    }

    /// Replaces tool call arguments before execution.
    #[must_use]
    pub fn modify_input(arguments: ToolCallArguments) -> Self {
        Self::ModifyInput { arguments }
    }

    /// Denies the tool call and returns `message` to the model.
    #[must_use]
    pub fn deny(message: impl Into<String>) -> Self {
        Self::Deny {
            message: message.into(),
        }
    }

    /// Skips execution and returns `output` as a succeeded tool result.
    #[must_use]
    pub fn short_circuit(output: ToolOutput) -> Self {
        Self::ShortCircuit { output }
    }

    /// Stops the current turn cleanly without executing the tool.
    #[must_use]
    pub fn stop_turn(reason: impl Into<String>, final_output: Option<String>) -> Self {
        Self::StopTurn {
            reason: reason.into(),
            final_output,
        }
    }
}

/// Read-only context supplied to an `after_tool_call` hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AfterToolCallContext {
    /// Owning session identity.
    pub session_id: SessionId,
    /// One-based model turn index that produced this tool call.
    pub model_turn: usize,
    /// Model-assigned tool call identifier.
    pub call_id: ToolCallId,
    /// Tool name that executed.
    pub tool_name: ToolName,
    /// Effective JSON object arguments used for execution.
    pub arguments: ToolCallArguments,
    /// Execution result status.
    pub status: ToolCallResultStatus,
    /// Output produced by the tool.
    pub output: ToolOutput,
    /// Optional structured diagnostic when status is failed.
    pub diagnostic: Option<ErrorInfo>,
    /// Whether this tool executed on the host bridge.
    pub is_bridge_tool: bool,
}

/// Decision returned by an `after_tool_call` hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum AfterToolDecision {
    /// Keep the recorded tool result unchanged and continue the turn.
    Continue,
    /// Replace the tool output before committing it to the durable session transcript.
    ModifyOutput {
        /// Replacement tool output.
        output: ToolOutput,
    },
    /// Commit this tool result, settle any remaining pending calls cleanly, and stop the turn.
    StopTurn {
        /// Actionable human-readable reason recorded in loop status.
        reason: String,
        /// Optional explicit final text returned to the caller.
        #[serde(default)]
        final_output: Option<String>,
    },
}

impl AfterToolDecision {
    /// Keeps the tool output unchanged.
    #[must_use]
    pub const fn continue_normally() -> Self {
        Self::Continue
    }

    /// Replaces the tool output before durable commit.
    #[must_use]
    pub fn modify_output(output: ToolOutput) -> Self {
        Self::ModifyOutput { output }
    }

    /// Stops the current turn cleanly after committing this tool result.
    #[must_use]
    pub fn stop_turn(reason: impl Into<String>, final_output: Option<String>) -> Self {
        Self::StopTurn {
            reason: reason.into(),
            final_output,
        }
    }
}

/// Typed hook request payload sent over the host bridge when subscribed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "point", rename_all = "snake_case")]
pub enum HookRequest {
    BeforeModelStep(BeforeModelStepContext),
    AfterModelStep(AfterModelStepContext),
    BeforeToolCall(BeforeToolCallContext),
    AfterToolCall(AfterToolCallContext),
}

impl HookRequest {
    /// Returns the lifecycle point corresponding to this request.
    #[must_use]
    pub const fn point(&self) -> HookPoint {
        match self {
            Self::BeforeModelStep(_) => HookPoint::BeforeModelStep,
            Self::AfterModelStep(_) => HookPoint::AfterModelStep,
            Self::BeforeToolCall(_) => HookPoint::BeforeToolCall,
            Self::AfterToolCall(_) => HookPoint::AfterToolCall,
        }
    }
}

/// Typed hook decision returned over the host bridge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "point", rename_all = "snake_case", deny_unknown_fields)]
pub enum HookDecision {
    BeforeModelStep(BeforeModelDecision),
    AfterModelStep(AfterModelDecision),
    BeforeToolCall(BeforeToolDecision),
    AfterToolCall(AfterToolDecision),
}

impl HookDecision {
    /// Returns the lifecycle point corresponding to this decision.
    #[must_use]
    pub const fn point(&self) -> HookPoint {
        match self {
            Self::BeforeModelStep(_) => HookPoint::BeforeModelStep,
            Self::AfterModelStep(_) => HookPoint::AfterModelStep,
            Self::BeforeToolCall(_) => HookPoint::BeforeToolCall,
            Self::AfterToolCall(_) => HookPoint::AfterToolCall,
        }
    }

    /// Validates non-blank text invariants for decisions crossing external boundaries.
    pub fn validate(&self) -> Result<(), HookError> {
        match self {
            Self::BeforeModelStep(BeforeModelDecision::StopTurn { reason, .. })
            | Self::AfterModelStep(AfterModelDecision::StopTurn { reason, .. })
            | Self::BeforeToolCall(BeforeToolDecision::StopTurn { reason, .. })
            | Self::AfterToolCall(AfterToolDecision::StopTurn { reason, .. }) => {
                validate_nonblank_hook_text("stop_turn reason", reason)
            }
            Self::AfterModelStep(AfterModelDecision::ContinueWithInput { text }) => {
                validate_nonblank_hook_text("continue_with text", text)
            }
            Self::BeforeToolCall(BeforeToolDecision::Deny { message }) => {
                validate_nonblank_hook_text("deny message", message)
            }
            Self::BeforeModelStep(BeforeModelDecision::Continue { .. })
            | Self::AfterModelStep(AfterModelDecision::Accept)
            | Self::BeforeToolCall(
                BeforeToolDecision::Allow
                | BeforeToolDecision::ModifyInput { .. }
                | BeforeToolDecision::ShortCircuit { .. },
            )
            | Self::AfterToolCall(
                AfterToolDecision::Continue | AfterToolDecision::ModifyOutput { .. },
            ) => Ok(()),
        }
    }
}

fn validate_nonblank_hook_text(field: &str, value: &str) -> Result<(), HookError> {
    if value.trim().is_empty() {
        return Err(HookError::InvalidDecision {
            message: format!("{field} must not be blank"),
        });
    }
    if value
        .chars()
        .any(|ch| ch.is_control() && ch != '\n' && ch != '\r' && ch != '\t')
    {
        return Err(HookError::InvalidDecision {
            message: format!("{field} must not contain control characters"),
        });
    }
    Ok(())
}

/// One pending host-bridged hook invocation awaiting a decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HookInvocation {
    id: HookInvocationId,
    request: HookRequest,
}

impl HookInvocation {
    /// Creates a new hook invocation envelope.
    #[must_use]
    pub(crate) fn new(id: HookInvocationId, request: HookRequest) -> Self {
        Self { id, request }
    }

    /// Returns the runtime-assigned invocation identifier.
    #[must_use]
    pub fn id(&self) -> &HookInvocationId {
        &self.id
    }

    /// Returns the typed hook request payload.
    #[must_use]
    pub fn request(&self) -> &HookRequest {
        &self.request
    }

    /// Returns the lifecycle point of this invocation.
    #[must_use]
    pub const fn point(&self) -> HookPoint {
        self.request.point()
    }
}

/// Errors produced by hook execution or invalid hook decisions.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HookError {
    /// Hook execution failed (fail-closed).
    #[error("hook execution failed: {message}")]
    Failed { message: String },
    /// Hook execution was cancelled cooperatively.
    #[error("hook execution cancelled")]
    Cancelled,
    /// Host returned a decision variant for the wrong hook point.
    #[error("hook decision point mismatch: expected {expected}, received {actual}")]
    DecisionTypeMismatch {
        expected: HookPoint,
        actual: HookPoint,
    },
    /// Hook decision payload violated a contract invariant.
    #[error("invalid hook decision: {message}")]
    InvalidDecision { message: String },
}

/// Boxed async future returned by object-safe runtime hook handlers.
pub type HookFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, HookError>> + Send + 'a>>;

/// Object-safe async hook handler evaluated in-process by the Rust runtime.
pub trait RuntimeHookHandler: Send + Sync {
    /// Declares which hook points and tool names this handler cares about.
    fn subscriptions(&self) -> HookSubscriptions;

    /// Intercepts execution before a model step is compiled and sent.
    fn before_model_step<'a>(
        &'a self,
        _context: BeforeModelStepContext,
        _cancellation_token: CancellationToken,
    ) -> HookFuture<'a, BeforeModelDecision> {
        Box::pin(async { Ok(BeforeModelDecision::continue_normally()) })
    }

    /// Intercepts execution after a model step finishes.
    fn after_model_step<'a>(
        &'a self,
        _context: AfterModelStepContext,
        _cancellation_token: CancellationToken,
    ) -> HookFuture<'a, AfterModelDecision> {
        Box::pin(async { Ok(AfterModelDecision::accept()) })
    }

    /// Intercepts a tool call before execution.
    fn before_tool_call<'a>(
        &'a self,
        _context: BeforeToolCallContext,
        _cancellation_token: CancellationToken,
    ) -> HookFuture<'a, BeforeToolDecision> {
        Box::pin(async { Ok(BeforeToolDecision::allow()) })
    }

    /// Intercepts a tool call after execution, before durable commit.
    fn after_tool_call<'a>(
        &'a self,
        _context: AfterToolCallContext,
        _cancellation_token: CancellationToken,
    ) -> HookFuture<'a, AfterToolDecision> {
        Box::pin(async { Ok(AfterToolDecision::continue_normally()) })
    }
}
