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
    /// Model output was blocked by provider safety or content policy.
    Blocked,
    /// Model work was cancelled.
    Cancelled,
    /// Model stopped because a provider error occurred.
    Error,
}

/// Provider-neutral detail explaining a non-stop finish.
///
/// Providers report a more specific cause for some terminal states, such as the
/// Responses API `incomplete_details.reason`. The runtime only keeps the causes
/// it can act on or report deterministically; providers must not send their raw
/// wording across this boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FinishDetail {
    /// The configured output token budget, including reasoning, was reached.
    MaxOutputTokens,
    /// Provider content policy blocked the output.
    ContentFilter,
}

impl FinishDetail {
    /// Stable lowercase detail text for diagnostics and journal messages.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MaxOutputTokens => "max_output_tokens",
            Self::ContentFilter => "content_filter",
        }
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    finish_detail: Option<FinishDetail>,
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
            finish_detail: None,
        }
    }

    /// Returns a copy with an optional detail for a non-stop finish.
    #[must_use]
    pub fn with_finish_detail(mut self, finish_detail: Option<FinishDetail>) -> Self {
        self.finish_detail = finish_detail;
        self
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

    /// Optional provider-neutral detail for a non-stop finish.
    #[must_use]
    pub fn finish_detail(&self) -> Option<FinishDetail> {
        self.finish_detail
    }
}
