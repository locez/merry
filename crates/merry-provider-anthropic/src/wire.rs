use schemars::Schema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize)]
pub(crate) struct AnthropicRequest<'a> {
    pub(crate) model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) system: Option<String>,
    pub(crate) messages: Vec<AnthropicMessage<'a>>,
    pub(crate) max_tokens: u64,
    pub(crate) stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tools: Vec<AnthropicTool<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_choice: Option<AnthropicToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) output_config: Option<AnthropicOutputConfig<'a>>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AnthropicMessage<'a> {
    pub(crate) role: &'static str,
    pub(crate) content: Vec<AnthropicContentBlock<'a>>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum AnthropicContentBlock<'a> {
    Text {
        text: &'a str,
    },
    ToolUse {
        id: &'a str,
        name: &'a str,
        input: &'a serde_json::Map<String, Value>,
    },
    ToolResult {
        tool_use_id: &'a str,
        content: &'a str,
        is_error: bool,
    },
}

#[derive(Debug, Serialize)]
pub(crate) struct AnthropicTool<'a> {
    pub(crate) name: &'a str,
    pub(crate) description: &'a str,
    pub(crate) input_schema: &'a Schema,
}

#[derive(Debug, Serialize)]
pub(crate) struct AnthropicToolChoice {
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) disable_parallel_tool_use: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct AnthropicOutputConfig<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) effort: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) format: Option<AnthropicOutputFormat<'a>>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AnthropicOutputFormat<'a> {
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) schema: &'a Schema,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum AnthropicStreamEvent {
    MessageStart {
        message: AnthropicMessageStart,
    },
    ContentBlockStart {
        index: u64,
        content_block: AnthropicContentBlockStart,
    },
    ContentBlockDelta {
        index: u64,
        delta: AnthropicContentBlockDelta,
    },
    ContentBlockStop {
        index: u64,
    },
    MessageDelta {
        delta: AnthropicMessageDelta,
        usage: Option<AnthropicUsage>,
    },
    MessageStop,
    Error {
        error: AnthropicStreamError,
    },
    Ping,
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicMessageStart {
    pub(crate) usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum AnthropicContentBlockStart {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum AnthropicContentBlockDelta {
    TextDelta {
        text: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicMessageDelta {
    pub(crate) stop_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct AnthropicUsage {
    pub(crate) input_tokens: Option<u64>,
    pub(crate) cache_creation_input_tokens: Option<u64>,
    pub(crate) cache_read_input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicStreamError {
    #[serde(rename = "type")]
    pub(crate) kind: Option<String>,
}
