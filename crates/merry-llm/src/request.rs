//! Provider-neutral model requests.

use crate::{
    ModelError,
    tool::{ModelToolContinuation, validate_provider_identifier},
};
use merry_core::ToolSpec;
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::{borrow::Cow, fmt, str::FromStr};

const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

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

/// Stable fingerprint of the provider-neutral tool profile visible to a request.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct ToolProfileHash(String);

impl ToolProfileHash {
    /// Borrows the stable hash label.
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

/// Provider-neutral message role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelMessageRole {
    /// Instructions from the system/developer layer after context compilation.
    System,
    /// User-originated request content after context compilation.
    User,
    /// Assistant-originated content included in the compiled snapshot.
    Assistant,
}

/// Provider-neutral model input content.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelContent {
    text: String,
}

impl ModelContent {
    /// Creates validated text content.
    pub fn text(text: &str) -> Result<Self, ModelError> {
        validate_text("ModelContent text", text)?;
        Ok(Self {
            text: text.to_owned(),
        })
    }

    /// Returns the text if this content is text.
    #[must_use]
    pub fn as_text(&self) -> &str {
        &self.text
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ModelContentRef<'a> {
    Text { text: &'a str },
}

impl Serialize for ModelContent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        ModelContentRef::Text {
            text: self.as_text(),
        }
        .serialize(serializer)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ModelContentWire {
    Text { text: String },
}

impl<'de> Deserialize<'de> for ModelContent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match ModelContentWire::deserialize(deserializer)? {
            ModelContentWire::Text { text } => Self::text(&text).map_err(de::Error::custom),
        }
    }
}

impl JsonSchema for ModelContent {
    fn schema_name() -> Cow<'static, str> {
        "ModelContent".into()
    }

    fn schema_id() -> Cow<'static, str> {
        concat!(module_path!(), "::ModelContent").into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        ModelContentWire::json_schema(generator)
    }
}

/// Provider-neutral message in a compiled model input snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelMessage {
    role: ModelMessageRole,
    content: ModelContent,
}

impl ModelMessage {
    /// Creates a validated model message.
    pub fn new(role: ModelMessageRole, content: ModelContent) -> Result<Self, ModelError> {
        validate_text("ModelMessage content", content.as_text())?;
        Ok(Self { role, content })
    }

    /// Message role.
    #[must_use]
    pub fn role(&self) -> ModelMessageRole {
        self.role
    }

    /// Message content.
    #[must_use]
    pub fn content(&self) -> &ModelContent {
        &self.content
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelMessageWire {
    role: ModelMessageRole,
    content: ModelContent,
}

impl<'de> Deserialize<'de> for ModelMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ModelMessageWire::deserialize(deserializer)?;
        Self::new(wire.role, wire.content).map_err(de::Error::custom)
    }
}

/// Provider-neutral generation controls.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GenerationConfig {
    max_output_tokens: Option<u64>,
    allow_parallel_tool_calls: bool,
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
            allow_parallel_tool_calls,
        })
    }

    /// Optional maximum output tokens.
    #[must_use]
    pub fn max_output_tokens(&self) -> Option<u64> {
        self.max_output_tokens
    }

    /// Whether multiple pending tool calls may be requested in one response.
    #[must_use]
    pub fn allow_parallel_tool_calls(&self) -> bool {
        self.allow_parallel_tool_calls
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationConfigWire {
    max_output_tokens: Option<u64>,
    allow_parallel_tool_calls: bool,
}

impl<'de> Deserialize<'de> for GenerationConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = GenerationConfigWire::deserialize(deserializer)?;
        Self::new(wire.max_output_tokens, wire.allow_parallel_tool_calls).map_err(de::Error::custom)
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
    messages: Vec<ModelMessage>,
    tools: Vec<ToolSpec>,
    #[serde(default)]
    continuations: Vec<ModelToolContinuation>,
    generation: GenerationConfig,
    tool_profile_hash: ToolProfileHash,
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
        if messages.is_empty() {
            return Err(ModelError::invalid_request(
                "ModelRequest messages must not be empty",
            ));
        }

        Ok(Self {
            model,
            messages,
            tool_profile_hash: tool_profile_hash(&tools),
            tools,
            continuations,
            generation,
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
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelRequestWire {
    model: ModelName,
    messages: Vec<ModelMessage>,
    tools: Vec<ToolSpec>,
    #[serde(default)]
    continuations: Vec<ModelToolContinuation>,
    generation: GenerationConfig,
    #[serde(default)]
    tool_profile_hash: Option<ToolProfileHash>,
}

impl<'de> Deserialize<'de> for ModelRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ModelRequestWire::deserialize(deserializer)?;
        let request = Self::new_with_continuations(
            wire.model,
            wire.messages,
            wire.tools,
            wire.continuations,
            wire.generation,
        )
        .map_err(de::Error::custom)?;

        if let Some(expected_hash) = wire.tool_profile_hash
            && expected_hash != request.tool_profile_hash
        {
            return Err(de::Error::custom(
                "ModelRequest tool_profile_hash did not match tools",
            ));
        }

        Ok(request)
    }
}

fn tool_profile_hash(tools: &[ToolSpec]) -> ToolProfileHash {
    let mut canonical_tools = tools
        .iter()
        .map(|tool| {
            serde_json::to_string(tool)
                .expect("provider-neutral tool specs must serialize for profile hashing")
        })
        .collect::<Vec<_>>();
    canonical_tools.sort();

    let mut hash = FNV_OFFSET_BASIS;
    for tool in canonical_tools {
        for byte in tool.as_bytes() {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME);
        }
        hash = (hash ^ 0xff).wrapping_mul(FNV_PRIME);
    }

    ToolProfileHash(format!("fnv1a64:{hash:016x}"))
}

fn validate_text(kind: &'static str, value: &str) -> Result<(), ModelError> {
    if value.trim().is_empty() {
        return Err(ModelError::invalid_request(format!(
            "{kind} must not be blank"
        )));
    }

    Ok(())
}
