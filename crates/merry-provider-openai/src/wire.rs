//! Private Responses API wire shapes.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize)]
pub(crate) struct ResponsesRequest<'a> {
    pub(crate) model: &'a str,
    pub(crate) input: Vec<ResponsesInputItem<'a>>,
    pub(crate) stream: bool,
    pub(crate) store: bool,
    pub(crate) parallel_tool_calls: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) prompt_cache_key: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning: Option<ResponsesReasoning<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) text: Option<ResponsesText<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tools: Vec<ResponsesTool<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_choice: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ResponsesReasoning<'a> {
    pub(crate) effort: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum ResponsesInputItem<'a> {
    Message(ResponsesMessageInputItem<'a>),
    AssistantMessage(ResponsesAssistantMessageInputItem<'a>),
    FunctionCall(ResponsesFunctionCallInputItem<'a>),
    FunctionCallOutput(ResponsesFunctionCallOutputInputItem<'a>),
}

impl<'a> ResponsesInputItem<'a> {
    pub(crate) fn message(role: &'static str, text: &'a str) -> Self {
        if role == "assistant" {
            return Self::AssistantMessage(ResponsesAssistantMessageInputItem {
                kind: "message",
                role,
                content: vec![ResponsesOutputMessageContent::output_text(text)],
            });
        }

        Self::Message(ResponsesMessageInputItem {
            role,
            content: vec![ResponsesInputContent::input_text(text)],
        })
    }

    pub(crate) fn message_content(
        role: &'static str,
        content: Vec<ResponsesInputContent<'a>>,
    ) -> Self {
        Self::Message(ResponsesMessageInputItem { role, content })
    }

    pub(crate) fn function_call(call_id: &'a str, name: &'a str, arguments: String) -> Self {
        Self::FunctionCall(ResponsesFunctionCallInputItem {
            kind: "function_call",
            call_id,
            name,
            arguments,
        })
    }

    pub(crate) fn function_call_output(call_id: &'a str, output: &'a str) -> Self {
        Self::FunctionCallOutput(ResponsesFunctionCallOutputInputItem {
            kind: "function_call_output",
            call_id,
            output,
        })
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct ResponsesMessageInputItem<'a> {
    pub(crate) role: &'static str,
    pub(crate) content: Vec<ResponsesInputContent<'a>>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ResponsesAssistantMessageInputItem<'a> {
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) role: &'static str,
    pub(crate) content: Vec<ResponsesOutputMessageContent<'a>>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub(crate) enum ResponsesInputContent<'a> {
    #[serde(rename = "input_text")]
    InputText { text: &'a str },
    #[serde(rename = "input_image")]
    InputImage { image_url: String },
}

impl<'a> ResponsesInputContent<'a> {
    pub(crate) fn input_text(text: &'a str) -> Self {
        Self::InputText { text }
    }

    pub(crate) fn input_image(image_url: String) -> Self {
        Self::InputImage { image_url }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct ResponsesOutputMessageContent<'a> {
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) text: &'a str,
    pub(crate) annotations: Vec<Value>,
}

impl<'a> ResponsesOutputMessageContent<'a> {
    fn output_text(text: &'a str) -> Self {
        Self {
            kind: "output_text",
            text,
            annotations: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct ResponsesFunctionCallInputItem<'a> {
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) call_id: &'a str,
    pub(crate) name: &'a str,
    pub(crate) arguments: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct ResponsesFunctionCallOutputInputItem<'a> {
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) call_id: &'a str,
    pub(crate) output: &'a str,
}

#[derive(Debug, Serialize)]
pub(crate) struct ResponsesTool<'a> {
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) name: &'a str,
    pub(crate) description: &'a str,
    pub(crate) parameters: &'a Value,
}

#[derive(Debug, Serialize)]
pub(crate) struct ResponsesText<'a> {
    pub(crate) format: ResponsesTextFormat<'a>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ResponsesTextFormat<'a> {
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) name: &'a str,
    pub(crate) strict: bool,
    pub(crate) schema: &'a Value,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResponsesResponse {
    #[serde(default)]
    pub(crate) output: Vec<ResponsesOutputItem>,
    pub(crate) status: Option<String>,
    pub(crate) usage: Option<ResponsesUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ResponsesOutputItem {
    #[serde(rename = "message")]
    Message {
        content: Vec<ResponsesOutputContent>,
    },
    #[serde(rename = "function_call")]
    FunctionCall {
        call_id: Option<String>,
        name: Option<String>,
        arguments: Option<String>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ResponsesOutputContent {
    #[serde(rename = "output_text")]
    OutputText { text: String },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub(crate) struct ResponsesUsage {
    pub(crate) input_tokens: u64,
    #[serde(default)]
    pub(crate) input_tokens_details: Option<ResponsesInputTokensDetails>,
    pub(crate) output_tokens: u64,
    #[serde(default)]
    pub(crate) output_tokens_details: Option<ResponsesOutputTokensDetails>,
    #[serde(default)]
    pub(crate) total_tokens: Option<u64>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub(crate) struct ResponsesInputTokensDetails {
    #[serde(default)]
    pub(crate) cached_tokens: Option<u64>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub(crate) struct ResponsesOutputTokensDetails {
    #[serde(default)]
    pub(crate) reasoning_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ResponsesStreamEvent {
    #[serde(rename = "response.created")]
    Created,
    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta { delta: String },
    #[serde(rename = "response.output_item.added")]
    OutputItemAdded {
        output_index: u64,
        item: ResponsesStreamOutputItem,
    },
    #[serde(rename = "response.function_call_arguments.delta")]
    FunctionCallArgumentsDelta { output_index: u64, delta: String },
    #[serde(rename = "response.function_call_arguments.done")]
    FunctionCallArgumentsDone {
        output_index: u64,
        arguments: String,
    },
    #[serde(rename = "response.output_item.done")]
    OutputItemDone {
        output_index: u64,
        item: ResponsesStreamOutputItem,
    },
    #[serde(rename = "response.completed")]
    Completed { response: ResponsesResponse },
    #[serde(rename = "response.incomplete")]
    Incomplete { response: ResponsesResponse },
    #[serde(rename = "response.failed")]
    Failed { response: ResponsesResponse },
    #[serde(rename = "error")]
    Error {
        code: Option<String>,
        message: Option<String>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ResponsesStreamOutputItem {
    #[serde(rename = "function_call")]
    FunctionCall {
        call_id: Option<String>,
        name: Option<String>,
        arguments: Option<String>,
    },
    #[serde(other)]
    Other,
}
