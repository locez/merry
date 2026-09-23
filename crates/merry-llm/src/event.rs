//! Normalized model event protocol.

use crate::{ModelOutputProgress, ModelResponse, ModelToolCall};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

/// Normalized event emitted by model providers.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelEvent {
    /// Provider accepted the request and began producing output.
    Started,
    /// Receive-side accounting without retaining text. None clears a failed attempt's sample.
    OutputProgress {
        progress: Option<ModelOutputProgress>,
    },
    /// Incremental text output.
    OutputTextDelta {
        /// Delta text.
        delta: String,
    },
    /// Model requested a tool call.
    ToolCallRequested {
        /// Requested tool call.
        call: ModelToolCall,
    },
    /// Stream completed. No events may be emitted after this event.
    Completed {
        /// Aggregated model response.
        response: ModelResponse,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ModelEventWire {
    Started {},
    OutputProgress {
        progress: Option<ModelOutputProgress>,
    },
    OutputTextDelta {
        delta: String,
    },
    ToolCallRequested {
        call: ModelToolCall,
    },
    Completed {
        response: ModelResponse,
    },
}

impl<'de> Deserialize<'de> for ModelEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ModelEventWire::deserialize(deserializer)?;
        Ok(match wire {
            ModelEventWire::Started {} => Self::Started,
            ModelEventWire::OutputProgress { progress } => Self::OutputProgress { progress },
            ModelEventWire::OutputTextDelta { delta } => Self::OutputTextDelta { delta },
            ModelEventWire::ToolCallRequested { call } => Self::ToolCallRequested { call },
            ModelEventWire::Completed { response } => Self::Completed { response },
        })
    }
}
