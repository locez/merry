//! Validated provider-neutral generation settings and compatibility decoding.

use crate::{ModelCapabilities, ModelError, tool::validate_provider_identifier};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};
use std::{fmt, str::FromStr};

/// Provider-neutral generation controls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GenerationConfig {
    max_output_tokens: Option<u64>,
    parallel_tool_calls: ParallelToolCalls,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    service_tier: Option<ServiceTier>,
}

impl GenerationConfig {
    /// Creates validated generation controls.
    pub fn new(
        max_output_tokens: Option<u64>,
        allow_parallel_tool_calls: bool,
    ) -> Result<Self, ModelError> {
        if max_output_tokens == Some(0) {
            return Err(ModelError::invalid_request(
                "max_output_tokens must be greater than zero",
            ));
        }

        Ok(Self {
            max_output_tokens,
            parallel_tool_calls: if allow_parallel_tool_calls {
                ParallelToolCalls::Enabled
            } else {
                ParallelToolCalls::Disabled
            },
            reasoning_effort: None,
            service_tier: None,
        })
    }

    /// Returns a copy with an explicit parallel tool-call preference.
    #[must_use]
    pub fn with_parallel_tool_calls(mut self, parallel_tool_calls: ParallelToolCalls) -> Self {
        self.parallel_tool_calls = parallel_tool_calls;
        self
    }

    /// Returns a copy with an optional model reasoning-effort hint.
    pub fn with_reasoning_effort(mut self, reasoning_effort: Option<ReasoningEffort>) -> Self {
        self.reasoning_effort = reasoning_effort;
        self
    }

    /// Returns a copy with an optional provider service-tier hint.
    #[must_use]
    pub fn with_service_tier(mut self, service_tier: Option<ServiceTier>) -> Self {
        self.service_tier = service_tier;
        self
    }

    /// Optional maximum output tokens.
    #[must_use]
    pub fn max_output_tokens(&self) -> Option<u64> {
        self.max_output_tokens
    }

    /// Whether multiple pending tool calls may be requested in one response.
    #[must_use]
    pub fn allow_parallel_tool_calls(&self) -> bool {
        self.parallel_tool_calls == ParallelToolCalls::Enabled
    }

    /// Returns the unresolved or explicit parallel tool-call preference.
    #[must_use]
    pub fn parallel_tool_calls(&self) -> ParallelToolCalls {
        self.parallel_tool_calls
    }

    /// Resolves automatic parallel tool-call behavior against provider capabilities.
    pub fn resolve_parallel_tool_calls(
        mut self,
        capabilities: &ModelCapabilities,
    ) -> Result<Self, ModelError> {
        self.parallel_tool_calls = match self.parallel_tool_calls {
            ParallelToolCalls::Auto if capabilities.supports_parallel_tool_calls() => {
                ParallelToolCalls::Enabled
            }
            ParallelToolCalls::Auto => ParallelToolCalls::Disabled,
            ParallelToolCalls::Enabled if !capabilities.supports_parallel_tool_calls() => {
                return Err(ModelError::invalid_request(
                    "parallel tool calls were enabled but the provider does not support them",
                ));
            }
            explicit => explicit,
        };
        Ok(self)
    }

    /// Optional provider-neutral reasoning-effort hint.
    #[must_use]
    pub fn reasoning_effort(&self) -> Option<&ReasoningEffort> {
        self.reasoning_effort.as_ref()
    }

    /// Optional provider service-tier hint (for example OpenAI `service_tier`).
    #[must_use]
    pub fn service_tier(&self) -> Option<ServiceTier> {
        self.service_tier
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationConfigWire {
    max_output_tokens: Option<u64>,
    #[serde(default)]
    parallel_tool_calls: Option<ParallelToolCalls>,
    #[serde(default)]
    allow_parallel_tool_calls: Option<bool>,
    #[serde(default)]
    reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    service_tier: Option<ServiceTier>,
}

impl<'de> Deserialize<'de> for GenerationConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = GenerationConfigWire::deserialize(deserializer)?;
        if wire.parallel_tool_calls.is_some() && wire.allow_parallel_tool_calls.is_some() {
            return Err(de::Error::custom(
                "GenerationConfig must not contain both parallel_tool_calls and allow_parallel_tool_calls",
            ));
        }
        let parallel_tool_calls = wire.parallel_tool_calls.unwrap_or_else(|| {
            wire.allow_parallel_tool_calls
                .map_or(ParallelToolCalls::Auto, |enabled| {
                    if enabled {
                        ParallelToolCalls::Enabled
                    } else {
                        ParallelToolCalls::Disabled
                    }
                })
        });
        Ok(Self::new(wire.max_output_tokens, false)
            .map_err(de::Error::custom)?
            .with_parallel_tool_calls(parallel_tool_calls)
            .with_reasoning_effort(wire.reasoning_effort)
            .with_service_tier(wire.service_tier))
    }
}

impl Default for GenerationConfig {
    fn default() -> Self {
        Self {
            max_output_tokens: None,
            parallel_tool_calls: ParallelToolCalls::Auto,
            reasoning_effort: None,
            service_tier: None,
        }
    }
}

/// Provider-neutral preference for model-generated parallel tool calls.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ParallelToolCalls {
    /// Enable when the selected provider declares support.
    #[default]
    Auto,
    /// Require provider support and enable parallel calls.
    Enabled,
    /// Force one tool call per model turn.
    Disabled,
}

/// Provider-neutral reasoning-effort value.
///
/// Merry stores this as a validated string because supported values are model
/// and provider dependent.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct ReasoningEffort(String);

impl ReasoningEffort {
    /// Creates a validated reasoning-effort value.
    pub fn new(value: &str) -> Result<Self, ModelError> {
        validate_provider_identifier("ReasoningEffort", value)?;
        Ok(Self(value.to_owned()))
    }

    /// Borrows the configured reasoning-effort value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ReasoningEffort {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for ReasoningEffort {
    type Err = ModelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<&str> for ReasoningEffort {
    type Error = ModelError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for ReasoningEffort {
    type Error = ModelError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        validate_provider_identifier("ReasoningEffort", &value)?;
        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for ReasoningEffort {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_from(value).map_err(de::Error::custom)
    }
}

/// Provider service tier selecting request processing priority and pricing.
///
/// Values mirror the OpenAI `service_tier` request field. Providers that do
/// not support service tiers ignore the value.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ServiceTier {
    /// Lowest-latency processing.
    Ultrafast,
    /// Let the provider choose the tier based on account settings.
    Auto,
    /// Standard processing tier.
    Default,
    /// Fast processing tier.
    Fast,
    /// Flexible, lower-cost processing with higher latency.
    Flex,
    /// Priority processing.
    Priority,
    /// Scale tier processing.
    Scale,
}

impl ServiceTier {
    /// All supported service tiers in display order.
    pub const ALL: [Self; 7] = [
        Self::Ultrafast,
        Self::Auto,
        Self::Default,
        Self::Fast,
        Self::Flex,
        Self::Priority,
        Self::Scale,
    ];

    /// Parses a service tier from its wire identifier.
    pub fn new(value: &str) -> Result<Self, ModelError> {
        Self::ALL
            .into_iter()
            .find(|tier| tier.as_str() == value)
            .ok_or_else(|| {
                ModelError::invalid_request(format!(
                    "ServiceTier {value:?} is not supported; expected one of {}",
                    Self::ALL
                        .iter()
                        .map(|tier| tier.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })
    }

    /// Returns the wire identifier for this tier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ultrafast => "ultrafast",
            Self::Auto => "auto",
            Self::Default => "default",
            Self::Fast => "fast",
            Self::Flex => "flex",
            Self::Priority => "priority",
            Self::Scale => "scale",
        }
    }
}

impl fmt::Display for ServiceTier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ServiceTier {
    type Err = ModelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<&str> for ServiceTier {
    type Error = ModelError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
