//! Private Chat Completions-compatible wire shapes.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize)]
pub(crate) struct ChatCompletionRequest<'a> {
    pub(crate) model: &'a str,
    pub(crate) messages: Vec<ChatMessage<'a>>,
    pub(crate) stream: bool,
    pub(crate) stream_options: StreamOptions,
    pub(crate) parallel_tool_calls: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_completion_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tools: Vec<ChatTool<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_choice: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ChatMessage<'a> {
    pub(crate) role: &'static str,
    pub(crate) content: &'a str,
}

#[derive(Debug, Serialize)]
pub(crate) struct StreamOptions {
    pub(crate) include_usage: bool,
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
    pub(crate) parameters: &'a Value,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatCompletionResponse {
    pub(crate) choices: Vec<ChatCompletionChoice>,
    pub(crate) usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatCompletionChoice {
    pub(crate) message: ChatCompletionMessage,
    pub(crate) finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatCompletionMessage {
    pub(crate) content: Option<String>,
    #[serde(default)]
    pub(crate) tool_calls: Vec<ChatToolCall>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatToolCall {
    pub(crate) id: Option<String>,
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) function: ChatToolCallFunction,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatToolCallFunction {
    pub(crate) name: Option<String>,
    pub(crate) arguments: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub(crate) struct ChatUsage {
    pub(crate) prompt_tokens: u64,
    pub(crate) completion_tokens: u64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatCompletionStreamChunk {
    pub(crate) choices: Vec<ChatCompletionStreamChoice>,
    pub(crate) usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatCompletionStreamChoice {
    pub(crate) delta: ChatCompletionStreamDelta,
    pub(crate) finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatCompletionStreamDelta {
    pub(crate) content: Option<String>,
    #[serde(default)]
    pub(crate) tool_calls: Vec<ChatCompletionStreamToolCall>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatCompletionStreamToolCall {
    pub(crate) index: u64,
    pub(crate) id: Option<String>,
    #[serde(rename = "type")]
    pub(crate) kind: Option<String>,
    pub(crate) function: Option<ChatCompletionStreamToolCallFunction>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatCompletionStreamToolCallFunction {
    pub(crate) name: Option<String>,
    pub(crate) arguments: Option<String>,
}
