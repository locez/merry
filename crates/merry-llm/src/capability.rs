//! Model capability declarations.

use crate::ModelError;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};

/// Provider-neutral model capability flags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelCapabilities {
    streaming: bool,
    tool_calls: bool,
    parallel_tool_calls: bool,
    usage_reporting: bool,
    max_input_tokens: Option<u64>,
    max_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    replays_reasoning_input: bool,
}

impl ModelCapabilities {
    /// Creates a validated capability declaration.
    pub fn new(
        streaming: bool,
        tool_calls: bool,
        parallel_tool_calls: bool,
        usage_reporting: bool,
        max_input_tokens: Option<u64>,
        max_output_tokens: Option<u64>,
    ) -> Result<Self, ModelError> {
        validate_positive_optional("max_input_tokens", max_input_tokens)?;
        validate_positive_optional("max_output_tokens", max_output_tokens)?;

        if parallel_tool_calls && !tool_calls {
            return Err(ModelError::invalid_request(
                "parallel tool calls require tool call support",
            ));
        }

        Ok(Self {
            streaming,
            tool_calls,
            parallel_tool_calls,
            usage_reporting,
            max_input_tokens,
            max_output_tokens,
            replays_reasoning_input: false,
        })
    }

    /// Whether this model can stream normalized events.
    #[must_use]
    pub fn supports_streaming(&self) -> bool {
        self.streaming
    }

    /// Whether this model can request tool calls.
    #[must_use]
    pub fn supports_tool_calls(&self) -> bool {
        self.tool_calls
    }

    /// Whether this model can request more than one pending tool call at a time.
    #[must_use]
    pub fn supports_parallel_tool_calls(&self) -> bool {
        self.parallel_tool_calls
    }

    /// Whether this model can report token usage.
    #[must_use]
    pub fn supports_usage_reporting(&self) -> bool {
        self.usage_reporting
    }

    /// Optional provider-reported input token limit.
    #[must_use]
    pub fn max_input_tokens(&self) -> Option<u64> {
        self.max_input_tokens
    }

    /// Optional provider-reported output token limit.
    #[must_use]
    pub fn max_output_tokens(&self) -> Option<u64> {
        self.max_output_tokens
    }

    /// Returns a copy with an explicit provider-reported input token limit.
    #[must_use]
    pub fn with_max_input_tokens(mut self, max_input_tokens: std::num::NonZeroU64) -> Self {
        self.max_input_tokens = Some(max_input_tokens.get());
        self
    }

    /// Whether this provider serializes [`crate::ModelInputItem::Reasoning`]
    /// into outbound wire requests.
    #[must_use]
    pub const fn replays_reasoning_input(&self) -> bool {
        self.replays_reasoning_input
    }

    /// Returns a copy declaring whether outbound requests replay reasoning items.
    #[must_use]
    pub const fn with_reasoning_input_replay(mut self, replays_reasoning_input: bool) -> Self {
        self.replays_reasoning_input = replays_reasoning_input;
        self
    }
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        Self {
            streaming: true,
            tool_calls: false,
            parallel_tool_calls: false,
            usage_reporting: false,
            max_input_tokens: None,
            max_output_tokens: None,
            replays_reasoning_input: false,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelCapabilitiesWire {
    streaming: bool,
    tool_calls: bool,
    parallel_tool_calls: bool,
    usage_reporting: bool,
    max_input_tokens: Option<u64>,
    max_output_tokens: Option<u64>,
    #[serde(default)]
    replays_reasoning_input: bool,
}

impl<'de> Deserialize<'de> for ModelCapabilities {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ModelCapabilitiesWire::deserialize(deserializer)?;
        Self::new(
            wire.streaming,
            wire.tool_calls,
            wire.parallel_tool_calls,
            wire.usage_reporting,
            wire.max_input_tokens,
            wire.max_output_tokens,
        )
        .map(|capabilities| capabilities.with_reasoning_input_replay(wire.replays_reasoning_input))
        .map_err(de::Error::custom)
    }
}

fn validate_positive_optional(field: &'static str, value: Option<u64>) -> Result<(), ModelError> {
    if value == Some(0) {
        return Err(ModelError::invalid_request(format!(
            "{field} must be greater than zero"
        )));
    }

    Ok(())
}
