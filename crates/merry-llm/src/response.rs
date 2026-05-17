//! Provider-neutral model responses.

use crate::{ModelToolCall, Usage};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Provider-neutral reason a model response ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// Model reached a natural stop condition.
    Stop,
    /// Model requested one or more tool calls.
    ToolCalls,
    /// Model hit a configured or provider token limit.
    Length,
    /// Model work was cancelled.
    Cancelled,
    /// Model stopped because a provider error occurred.
    Error,
}

/// Aggregated model output item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelOutput {
    /// Final text output.
    Text { text: String },
    /// Tool call output.
    ToolCall { call: ModelToolCall },
}

impl ModelOutput {
    /// Creates a text output item.
    #[must_use]
    pub fn text(text: &str) -> Self {
        Self::Text {
            text: text.to_owned(),
        }
    }

    /// Creates a tool call output item.
    #[must_use]
    pub fn tool_call(call: ModelToolCall) -> Self {
        Self::ToolCall { call }
    }
}

/// Aggregated provider-neutral model response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelResponse {
    outputs: Vec<ModelOutput>,
    finish_reason: FinishReason,
    usage: Option<Usage>,
}

impl ModelResponse {
    /// Creates an aggregated model response.
    #[must_use]
    pub fn new(
        outputs: Vec<ModelOutput>,
        finish_reason: FinishReason,
        usage: Option<Usage>,
    ) -> Self {
        Self {
            outputs,
            finish_reason,
            usage,
        }
    }

    /// Aggregated output items.
    #[must_use]
    pub fn outputs(&self) -> &[ModelOutput] {
        &self.outputs
    }

    /// Provider-neutral finish reason.
    #[must_use]
    pub fn finish_reason(&self) -> FinishReason {
        self.finish_reason
    }

    /// Optional token usage.
    #[must_use]
    pub fn usage(&self) -> Option<Usage> {
        self.usage
    }
}
