//! Request rendering from Merry-owned model types.

use crate::{
    OpenAiProviderError,
    wire::{ChatCompletionRequest, ChatMessage, ChatTool, ChatToolFunction, StreamOptions},
};
use merry_llm::{ModelMessageRole, ModelRequest};
use serde_json::Value;

#[allow(dead_code)]
pub(crate) fn render_chat_completion_request(
    request: &ModelRequest,
) -> Result<Value, OpenAiProviderError> {
    if request.generation().allow_parallel_tool_calls() {
        return Err(OpenAiProviderError::invalid_request(
            "parallel tool calls are not supported by the OpenAI provider adapter yet",
        ));
    }

    let messages = request
        .messages()
        .iter()
        .map(|message| ChatMessage {
            role: render_role(message.role()),
            content: message.content().as_text(),
        })
        .collect();

    let tools = request
        .tools()
        .iter()
        .map(|tool| {
            Ok(ChatTool {
                kind: "function",
                function: ChatToolFunction {
                    name: tool.name().as_str(),
                    description: tool.description(),
                    parameters: schema_as_value(tool)?,
                },
            })
        })
        .collect::<Result<Vec<_>, OpenAiProviderError>>()?;

    let wire = ChatCompletionRequest {
        model: request.model().as_str(),
        messages,
        stream: true,
        stream_options: StreamOptions {
            include_usage: true,
        },
        parallel_tool_calls: request.generation().allow_parallel_tool_calls(),
        max_completion_tokens: request.generation().max_output_tokens(),
        tool_choice: if tools.is_empty() { None } else { Some("auto") },
        tools,
    };

    serde_json::to_value(wire).map_err(|error| {
        OpenAiProviderError::invalid_request(format!(
            "failed to serialize Chat Completions request: {error}"
        ))
    })
}

fn render_role(role: ModelMessageRole) -> &'static str {
    match role {
        ModelMessageRole::System => "system",
        ModelMessageRole::User => "user",
        ModelMessageRole::Assistant => "assistant",
    }
}

fn schema_as_value(tool: &merry_core::ToolSpec) -> Result<&Value, OpenAiProviderError> {
    let schema = tool.input_schema().as_schema();
    if schema.as_object().is_none() {
        return Err(OpenAiProviderError::invalid_request(format!(
            "tool {} input schema must be a JSON object",
            tool.name()
        )));
    }

    Ok(schema.as_value())
}
