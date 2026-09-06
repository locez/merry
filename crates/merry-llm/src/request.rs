//! Provider-neutral model requests.

pub use crate::content::{ModelContent, ModelContentPart, ModelImage};
use crate::{
    ModelError,
    request::hashing::{dynamic_input_hash, stable_input_prefix_hash, tool_profile_hash},
    tool::{
        ModelToolBatchContinuation, ModelToolCallBatch, ModelToolContinuation,
        validate_provider_identifier,
    },
};
pub use generation::{GenerationConfig, ParallelToolCalls, ReasoningEffort, ServiceTier};
pub use hashing::{RequestContentHash, ToolProfileHash};
use merry_core::ToolSpec;
pub use message::{ModelInputItem, ModelMessage, ModelMessageRole};
pub use response_format::{ModelResponseFormat, ModelStructuredOutputFormat};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};
use std::{fmt, str::FromStr};

mod generation;

mod hashing;

mod message;

mod response_format;

/// Provider model identifier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct ModelName(String);

impl ModelName {
    /// Creates a validated model identifier.
    pub fn new(value: &str) -> Result<Self, ModelError> {
        validate_provider_identifier("ModelName", value)?;
        Ok(Self(value.to_owned()))
    }

    /// Borrows the model identifier as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ModelName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for ModelName {
    type Err = ModelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<&str> for ModelName {
    type Error = ModelError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for ModelName {
    type Error = ModelError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        validate_provider_identifier("ModelName", &value)?;
        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for ModelName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_from(value).map_err(de::Error::custom)
    }
}

/// Compiled provider input snapshot.
///
/// `ModelRequest` is the runtime/context compiler's provider-neutral snapshot of
/// what a model should see now. It is not runtime state and must not contain raw
/// chat history, provider conversation IDs, stored response IDs, sessions,
/// threads, ledger IDs, or runtime sequencing fields.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelRequest {
    model: ModelName,
    input: Vec<ModelInputItem>,
    messages: Vec<ModelMessage>,
    tools: Vec<ToolSpec>,
    #[serde(default)]
    continuations: Vec<ModelToolContinuation>,
    #[serde(skip)]
    batch_continuations: Vec<ModelToolBatchContinuation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ModelResponseFormat>,
    generation: GenerationConfig,
    stable_prefix_message_count: usize,
    tool_profile_hash: ToolProfileHash,
    stable_prefix_hash: RequestContentHash,
    dynamic_context_hash: RequestContentHash,
}

impl ModelRequest {
    /// Creates a validated compiled model request snapshot.
    pub fn new(
        model: ModelName,
        messages: Vec<ModelMessage>,
        tools: Vec<ToolSpec>,
        generation: GenerationConfig,
    ) -> Result<Self, ModelError> {
        Self::new_with_continuations(model, messages, tools, Vec::new(), generation)
    }

    /// Creates a validated compiled model request snapshot with ordered tool continuations.
    pub fn new_with_continuations(
        model: ModelName,
        messages: Vec<ModelMessage>,
        tools: Vec<ToolSpec>,
        continuations: Vec<ModelToolContinuation>,
        generation: GenerationConfig,
    ) -> Result<Self, ModelError> {
        Self::new_with_continuations_and_stable_prefix(
            model,
            messages,
            tools,
            continuations,
            generation,
            0,
        )
    }

    /// Creates a validated compiled model request snapshot with a response format contract.
    pub fn new_with_response_format(
        model: ModelName,
        messages: Vec<ModelMessage>,
        tools: Vec<ToolSpec>,
        generation: GenerationConfig,
        response_format: Option<ModelResponseFormat>,
    ) -> Result<Self, ModelError> {
        Self::new_with_continuations_and_stable_prefix_and_response_format(
            model,
            messages,
            tools,
            Vec::new(),
            generation,
            0,
            response_format,
        )
    }

    /// Creates a validated compiled model request snapshot with an explicit
    /// stable prefix boundary.
    ///
    /// The stable prefix is the runtime-owned provider-neutral request prefix:
    /// base/system instructions plus the model-visible tool profile. Dynamic
    /// context, user input, and tool continuations are intentionally hashed
    /// separately so callers can tell whether a request changed the cacheable
    /// prefix or only late context.
    pub fn new_with_continuations_and_stable_prefix(
        model: ModelName,
        messages: Vec<ModelMessage>,
        tools: Vec<ToolSpec>,
        continuations: Vec<ModelToolContinuation>,
        generation: GenerationConfig,
        stable_prefix_message_count: usize,
    ) -> Result<Self, ModelError> {
        Self::new_with_continuations_and_stable_prefix_and_response_format(
            model,
            messages,
            tools,
            continuations,
            generation,
            stable_prefix_message_count,
            None,
        )
    }

    /// Creates a validated compiled model request snapshot with an explicit
    /// stable prefix boundary and response format contract.
    pub fn new_with_continuations_and_stable_prefix_and_response_format(
        model: ModelName,
        messages: Vec<ModelMessage>,
        tools: Vec<ToolSpec>,
        continuations: Vec<ModelToolContinuation>,
        generation: GenerationConfig,
        stable_prefix_message_count: usize,
        response_format: Option<ModelResponseFormat>,
    ) -> Result<Self, ModelError> {
        if messages.is_empty() {
            return Err(ModelError::invalid_request(
                "ModelRequest messages must not be empty",
            ));
        }

        if stable_prefix_message_count > messages.len() {
            return Err(ModelError::invalid_request(
                "ModelRequest stable prefix message count must not exceed messages length",
            ));
        }

        let input = input_from_messages_and_continuations(&messages, &continuations);
        Self::new_with_input_and_stable_prefix_and_response_format(
            model,
            input,
            tools,
            generation,
            stable_prefix_message_count,
            response_format,
        )
    }

    /// Creates a validated compiled model request snapshot from ordered input items.
    pub fn new_with_input_and_stable_prefix(
        model: ModelName,
        input: Vec<ModelInputItem>,
        tools: Vec<ToolSpec>,
        generation: GenerationConfig,
        stable_prefix_item_count: usize,
    ) -> Result<Self, ModelError> {
        Self::new_with_input_and_stable_prefix_and_response_format(
            model,
            input,
            tools,
            generation,
            stable_prefix_item_count,
            None,
        )
    }

    /// Creates a validated compiled model request snapshot from ordered input
    /// items and an optional response format contract.
    pub fn new_with_input_and_stable_prefix_and_response_format(
        model: ModelName,
        input: Vec<ModelInputItem>,
        tools: Vec<ToolSpec>,
        generation: GenerationConfig,
        stable_prefix_item_count: usize,
        response_format: Option<ModelResponseFormat>,
    ) -> Result<Self, ModelError> {
        if input.is_empty() {
            return Err(ModelError::invalid_request(
                "ModelRequest input must not be empty",
            ));
        }

        if stable_prefix_item_count > input.len() {
            return Err(ModelError::invalid_request(
                "ModelRequest stable prefix item count must not exceed input length",
            ));
        }
        if input.iter().take(stable_prefix_item_count).any(|item| {
            !matches!(
                item,
                ModelInputItem::Message(message)
                    if message.role() == ModelMessageRole::System
            )
        }) {
            return Err(ModelError::invalid_request(
                "ModelRequest stable prefix messages must use the system role",
            ));
        }

        let messages = messages_from_input(&input);
        let batch_continuations = batch_continuations_from_input(&input)?;
        let continuations = batch_continuations
            .iter()
            .flat_map(ModelToolBatchContinuation::single_continuations)
            .collect();
        let tool_profile_hash = tool_profile_hash(&tools);
        let stable_prefix_hash = stable_input_prefix_hash(
            &input[..stable_prefix_item_count],
            &tools,
            response_format.as_ref(),
        );
        let dynamic_context_hash = dynamic_input_hash(&input[stable_prefix_item_count..]);

        Ok(Self {
            model,
            input,
            messages,
            tools,
            continuations,
            batch_continuations,
            response_format,
            generation,
            stable_prefix_message_count: stable_prefix_item_count,
            tool_profile_hash,
            stable_prefix_hash,
            dynamic_context_hash,
        })
    }

    /// Requested provider model.
    #[must_use]
    pub fn model(&self) -> &ModelName {
        &self.model
    }

    /// Compiled message snapshot.
    #[must_use]
    pub fn messages(&self) -> &[ModelMessage] {
        &self.messages
    }

    /// Ordered provider-neutral input snapshot.
    #[must_use]
    pub fn input(&self) -> &[ModelInputItem] {
        &self.input
    }

    /// Provider-neutral tool specifications.
    #[must_use]
    pub fn tools(&self) -> &[ToolSpec] {
        &self.tools
    }

    /// Ordered tool call/result continuations visible to the model.
    #[must_use]
    pub fn continuations(&self) -> &[ModelToolContinuation] {
        &self.continuations
    }

    /// Ordered complete tool-call batches visible to the model.
    #[must_use]
    pub fn batch_continuations(&self) -> &[ModelToolBatchContinuation] {
        &self.batch_continuations
    }

    /// Optional contract for model response shape.
    #[must_use]
    pub fn response_format(&self) -> Option<&ModelResponseFormat> {
        self.response_format.as_ref()
    }

    /// Number of leading messages included in the stable prefix hash.
    #[must_use]
    pub fn stable_prefix_message_count(&self) -> usize {
        self.stable_prefix_message_count
    }

    /// Number of leading input items included in the stable prefix hash.
    #[must_use]
    pub fn stable_prefix_item_count(&self) -> usize {
        self.stable_prefix_message_count
    }

    /// Leading system/developer messages included in the stable prefix hash.
    #[must_use]
    pub fn stable_prefix_messages(&self) -> &[ModelMessage] {
        &self.messages[..self.stable_prefix_message_count]
    }

    /// Leading input items included in the stable prefix hash.
    #[must_use]
    pub fn stable_prefix_input(&self) -> &[ModelInputItem] {
        &self.input[..self.stable_prefix_message_count]
    }

    /// Dynamic messages outside the stable prefix.
    #[must_use]
    pub fn dynamic_messages(&self) -> &[ModelMessage] {
        &self.messages[self.stable_prefix_message_count..]
    }

    /// Dynamic input outside the stable prefix.
    #[must_use]
    pub fn dynamic_input(&self) -> &[ModelInputItem] {
        &self.input[self.stable_prefix_message_count..]
    }

    /// Generation controls.
    #[must_use]
    pub fn generation(&self) -> &GenerationConfig {
        &self.generation
    }

    /// Stable hash of the provider-neutral tool profile.
    #[must_use]
    pub fn tool_profile_hash(&self) -> &ToolProfileHash {
        &self.tool_profile_hash
    }

    /// Stable hash of the cacheable provider-neutral prefix.
    #[must_use]
    pub fn stable_prefix_hash(&self) -> &RequestContentHash {
        &self.stable_prefix_hash
    }

    /// Stable hash of dynamic request context outside the cacheable prefix.
    #[must_use]
    pub fn dynamic_context_hash(&self) -> &RequestContentHash {
        &self.dynamic_context_hash
    }

    /// Stable hash of dynamic ordered input outside the cacheable prefix.
    #[must_use]
    pub fn dynamic_input_hash(&self) -> &RequestContentHash {
        &self.dynamic_context_hash
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelRequestWire {
    model: ModelName,
    #[serde(default)]
    input: Option<Vec<ModelInputItem>>,
    #[serde(default)]
    messages: Option<Vec<ModelMessage>>,
    tools: Vec<ToolSpec>,
    #[serde(default)]
    continuations: Vec<ModelToolContinuation>,
    #[serde(default)]
    response_format: Option<ModelResponseFormat>,
    generation: GenerationConfig,
    #[serde(default)]
    stable_prefix_message_count: usize,
    #[serde(default)]
    stable_prefix_item_count: Option<usize>,
    #[serde(default)]
    tool_profile_hash: Option<ToolProfileHash>,
    #[serde(default)]
    stable_prefix_hash: Option<RequestContentHash>,
    #[serde(default)]
    dynamic_context_hash: Option<RequestContentHash>,
}

impl<'de> Deserialize<'de> for ModelRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ModelRequestWire::deserialize(deserializer)?;
        let stable_prefix_item_count = wire
            .stable_prefix_item_count
            .unwrap_or(wire.stable_prefix_message_count);
        let request = if let Some(input) = wire.input {
            Self::new_with_input_and_stable_prefix_and_response_format(
                wire.model,
                input,
                wire.tools,
                wire.generation,
                stable_prefix_item_count,
                wire.response_format,
            )
        } else {
            let messages = wire
                .messages
                .ok_or_else(|| de::Error::missing_field("messages"))?;
            Self::new_with_continuations_and_stable_prefix_and_response_format(
                wire.model,
                messages,
                wire.tools,
                wire.continuations,
                wire.generation,
                stable_prefix_item_count,
                wire.response_format,
            )
        }
        .map_err(de::Error::custom)?;

        if let Some(expected_hash) = wire.tool_profile_hash
            && expected_hash != request.tool_profile_hash
        {
            return Err(de::Error::custom(
                "ModelRequest tool_profile_hash did not match tools",
            ));
        }
        if let Some(expected_hash) = wire.stable_prefix_hash
            && expected_hash != request.stable_prefix_hash
        {
            return Err(de::Error::custom(
                "ModelRequest stable_prefix_hash did not match stable prefix",
            ));
        }
        if let Some(expected_hash) = wire.dynamic_context_hash
            && expected_hash != request.dynamic_context_hash
        {
            return Err(de::Error::custom(
                "ModelRequest dynamic_context_hash did not match dynamic context",
            ));
        }

        Ok(request)
    }
}

fn input_from_messages_and_continuations(
    messages: &[ModelMessage],
    continuations: &[ModelToolContinuation],
) -> Vec<ModelInputItem> {
    let mut input = Vec::with_capacity(messages.len() + continuations.len().saturating_mul(2));
    input.extend(messages.iter().cloned().map(ModelInputItem::Message));
    for continuation in continuations {
        input.push(ModelInputItem::ToolCall(continuation.call().clone()));
        input.push(ModelInputItem::ToolResult(continuation.result().clone()));
    }
    input
}

fn messages_from_input(input: &[ModelInputItem]) -> Vec<ModelMessage> {
    input
        .iter()
        .filter_map(|item| match item {
            ModelInputItem::Message(message) => Some(message.clone()),
            ModelInputItem::ToolCall(_) | ModelInputItem::ToolResult(_) => None,
        })
        .collect()
}

fn batch_continuations_from_input(
    input: &[ModelInputItem],
) -> Result<Vec<ModelToolBatchContinuation>, ModelError> {
    let mut batches = Vec::new();
    let mut index = 0;
    while index < input.len() {
        match &input[index] {
            ModelInputItem::ToolCall(_) => {
                let call_start = index;
                while matches!(input.get(index), Some(ModelInputItem::ToolCall(_))) {
                    index += 1;
                }
                let calls = input[call_start..index]
                    .iter()
                    .filter_map(|item| match item {
                        ModelInputItem::ToolCall(call) => Some(call.clone()),
                        ModelInputItem::Message(_) | ModelInputItem::ToolResult(_) => None,
                    })
                    .collect::<Vec<_>>();

                let result_start = index;
                while matches!(input.get(index), Some(ModelInputItem::ToolResult(_))) {
                    index += 1;
                }
                let results = input[result_start..index]
                    .iter()
                    .filter_map(|item| match item {
                        ModelInputItem::ToolResult(result) => Some(result.clone()),
                        ModelInputItem::Message(_) | ModelInputItem::ToolCall(_) => None,
                    })
                    .collect::<Vec<_>>();

                if results.len() != calls.len() {
                    return Err(ModelError::invalid_request(
                        "ModelRequest tool call batches must be followed by one result for every call",
                    ));
                }

                let batch = ModelToolCallBatch::new(calls)?;
                batches.push(ModelToolBatchContinuation::new(batch, results)?);
            }
            ModelInputItem::ToolResult(_) => {
                return Err(ModelError::invalid_request(
                    "ModelRequest tool result input items must follow a matching tool call batch",
                ));
            }
            ModelInputItem::Message(_) => {
                index += 1;
            }
        }
    }
    Ok(batches)
}
