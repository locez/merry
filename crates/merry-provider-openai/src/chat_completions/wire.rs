use schemars::Schema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub(crate) struct ChatRequest<'a> {
    pub(crate) model: &'a str,
    pub(crate) messages: Vec<ChatMessage<'a>>,
    pub(crate) stream: bool,
    pub(crate) stream_options: ChatStreamOptions,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_completion_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_effort: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) response_format: Option<ChatResponseFormat<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tools: Vec<ChatTool<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_choice: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ChatStreamOptions {
    pub(crate) include_usage: bool,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum ChatMessage<'a> {
    Text {
        role: &'static str,
        content: &'a str,
    },
    Multimodal {
        role: &'static str,
        content: Vec<ChatContentPart<'a>>,
    },
    AssistantToolCalls {
        role: &'static str,
        content: Option<&'a str>,
        tool_calls: Vec<ChatToolCall<'a>>,
    },
    ToolResult {
        role: &'static str,
        tool_call_id: &'a str,
        content: &'a str,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ChatContentPart<'a> {
    Text { text: &'a str },
    ImageUrl { image_url: ChatImageUrl },
}

#[derive(Debug, Serialize)]
pub(crate) struct ChatImageUrl {
    pub(crate) url: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct ChatToolCall<'a> {
    pub(crate) id: &'a str,
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) function: ChatToolCallFunction<'a>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ChatToolCallFunction<'a> {
    pub(crate) name: &'a str,
    pub(crate) arguments: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct ChatTool<'a> {
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) function: ChatToolFunction<'a>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ChatToolFunction<'a> {
    pub(crate) name: &'a str,
    pub(crate) description: &'a str,
    pub(crate) parameters: &'a Schema,
    pub(crate) strict: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ChatResponseFormat<'a> {
    JsonSchema { json_schema: ChatJsonSchema<'a> },
}

#[derive(Debug, Serialize)]
pub(crate) struct ChatJsonSchema<'a> {
    pub(crate) name: &'a str,
    pub(crate) strict: bool,
    pub(crate) schema: &'a Schema,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatChunk {
    #[serde(default)]
    pub(crate) choices: Vec<ChatChoice>,
    pub(crate) usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatChoice {
    pub(crate) index: u64,
    #[serde(default)]
    pub(crate) delta: ChatDelta,
    pub(crate) finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ChatDelta {
    pub(crate) content: Option<String>,
    #[serde(default)]
    pub(crate) tool_calls: Vec<ChatToolCallDelta>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatToolCallDelta {
    pub(crate) index: u64,
    pub(crate) id: Option<String>,
    pub(crate) function: Option<ChatToolCallFunctionDelta>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatToolCallFunctionDelta {
    pub(crate) name: Option<String>,
    pub(crate) arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatUsage {
    pub(crate) prompt_tokens: u64,
    pub(crate) completion_tokens: u64,
    pub(crate) total_tokens: Option<u64>,
    pub(crate) prompt_tokens_details: Option<ChatPromptTokenDetails>,
    pub(crate) completion_tokens_details: Option<ChatCompletionTokenDetails>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatPromptTokenDetails {
    pub(crate) cached_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatCompletionTokenDetails {
    pub(crate) reasoning_tokens: Option<u64>,
}
