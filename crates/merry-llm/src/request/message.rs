//! Ordered model input and validated message contracts.

use crate::{
    ModelError,
    content::{ModelContent, validate_text},
    tool::{ModelToolCall, ModelToolResult},
};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};

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
        if role != ModelMessageRole::User && content.has_images() {
            return Err(ModelError::invalid_request(
                "ModelMessage image content is allowed only for the user role",
            ));
        }
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

/// Provider-neutral ordered model input item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    content = "item",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ModelInputItem {
    /// User, assistant, or system text message.
    Message(ModelMessage),
    /// Model-requested tool call replayed into provider-visible history.
    ToolCall(ModelToolCall),
    /// Tool result replayed into provider-visible history.
    ToolResult(ModelToolResult),
}
