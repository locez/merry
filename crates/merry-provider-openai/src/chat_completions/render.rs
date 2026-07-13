use super::wire::{
    ChatContentPart, ChatImageUrl, ChatJsonSchema, ChatMessage, ChatRequest, ChatResponseFormat,
    ChatStreamOptions, ChatTool, ChatToolCall, ChatToolCallFunction, ChatToolFunction,
};
use crate::{OpenAiProviderError, image::png_data_url};
use merry_llm::{
    ModelContentPart, ModelInputItem, ModelMessageRole, ModelRequest, ModelResponseFormat,
};
use serde_json::Value;

pub(crate) fn render_chat_request(request: &ModelRequest) -> Result<Value, OpenAiProviderError> {
    let messages = render_messages(request)?;
    let tools = request
        .tools()
        .iter()
        .map(|tool| {
            let parameters = tool.input_schema().as_schema();
            if parameters.as_object().is_none() {
                return Err(OpenAiProviderError::invalid_request(format!(
                    "tool {} input schema must be a JSON object",
                    tool.name()
                )));
            }
            Ok(ChatTool {
                kind: "function",
                function: ChatToolFunction {
                    name: tool.name().as_str(),
                    description: tool.description(),
                    parameters,
                    strict: true,
                },
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let response_format = request.response_format().map(|format| match format {
        ModelResponseFormat::StructuredOutput(format) => ChatResponseFormat::JsonSchema {
            json_schema: ChatJsonSchema {
                name: format.name(),
                strict: format.strict(),
                schema: format.schema(),
            },
        },
    });
    let has_tools = !tools.is_empty();
    let wire = ChatRequest {
        model: request.model().as_str(),
        messages,
        stream: true,
        stream_options: ChatStreamOptions {
            include_usage: true,
        },
        parallel_tool_calls: has_tools.then(|| request.generation().allow_parallel_tool_calls()),
        max_completion_tokens: request.generation().max_output_tokens(),
        reasoning_effort: request
            .generation()
            .reasoning_effort()
            .map(|effort| effort.as_str()),
        response_format,
        tools,
        tool_choice: has_tools.then_some("auto"),
    };

    serde_json::to_value(wire).map_err(|error| {
        OpenAiProviderError::invalid_request(format!(
            "failed to serialize Chat Completions request: {error}"
        ))
    })
}

fn render_messages<'a>(
    request: &'a ModelRequest,
) -> Result<Vec<ChatMessage<'a>>, OpenAiProviderError> {
    let mut messages = Vec::new();
    let mut index = 0;
    while index < request.input().len() {
        match &request.input()[index] {
            ModelInputItem::Message(message) => {
                if message.content().has_images() {
                    messages.push(ChatMessage::Multimodal {
                        role: render_role(message.role()),
                        content: message
                            .content()
                            .parts()
                            .iter()
                            .map(|part| match part {
                                ModelContentPart::Text { text } => ChatContentPart::Text { text },
                                ModelContentPart::Image { image } => ChatContentPart::ImageUrl {
                                    image_url: ChatImageUrl {
                                        url: png_data_url(image.png_bytes()),
                                    },
                                },
                            })
                            .collect(),
                    });
                } else {
                    messages.push(ChatMessage::Text {
                        role: render_role(message.role()),
                        content: message.content().as_text(),
                    });
                }
                index += 1;
            }
            ModelInputItem::ToolCall(_) => {
                let mut tool_calls = Vec::new();
                while let Some(ModelInputItem::ToolCall(call)) = request.input().get(index) {
                    tool_calls.push(ChatToolCall {
                        id: call.id().as_str(),
                        kind: "function",
                        function: ChatToolCallFunction {
                            name: call.name().as_str(),
                            arguments: serde_json::to_string(call.arguments().as_object())
                                .map_err(|error| {
                                    OpenAiProviderError::invalid_request(format!(
                                        "failed to serialize tool arguments: {error}"
                                    ))
                                })?,
                        },
                    });
                    index += 1;
                }
                messages.push(ChatMessage::AssistantToolCalls {
                    role: "assistant",
                    content: None,
                    tool_calls,
                });
            }
            ModelInputItem::ToolResult(result) => {
                messages.push(ChatMessage::ToolResult {
                    role: "tool",
                    tool_call_id: result.call_id().as_str(),
                    content: result.content().as_str(),
                });
                index += 1;
            }
        }
    }
    Ok(messages)
}

fn render_role(role: ModelMessageRole) -> &'static str {
    match role {
        ModelMessageRole::System => "system",
        ModelMessageRole::User => "user",
        ModelMessageRole::Assistant => "assistant",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use merry_core::{ToolCallResultStatus, ToolInputSchema, ToolName, ToolSpec};
    use merry_llm::{
        GenerationConfig, ModelContent, ModelImage, ModelInputItem, ModelMessage, ModelName,
        ModelToolCall, ModelToolCallId, ModelToolResult, ModelToolResultContent, ToolArguments,
    };
    use schemars::Schema;
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn renders_grouped_tool_history_and_parallel_controls() {
        let schema = Schema::try_from(json!({
            "type": "object",
            "properties": {"query": {"type": "string"}},
            "required": ["query"],
            "additionalProperties": false
        }))
        .expect("valid schema");
        let tool = ToolSpec::new(
            ToolName::new("search").expect("valid name"),
            "Search notes",
            ToolInputSchema::new(schema).expect("valid schema"),
        )
        .expect("valid tool");
        let call_1 = call("call-1", "alpha");
        let call_2 = call("call-2", "beta");
        let request = ModelRequest::new_with_input_and_stable_prefix(
            ModelName::new("gpt-test").expect("valid model"),
            vec![
                ModelInputItem::Message(
                    ModelMessage::new(
                        ModelMessageRole::System,
                        ModelContent::text("Be concise").expect("valid content"),
                    )
                    .expect("valid message"),
                ),
                ModelInputItem::Message(
                    ModelMessage::new(
                        ModelMessageRole::User,
                        ModelContent::text("Search twice").expect("valid content"),
                    )
                    .expect("valid message"),
                ),
                ModelInputItem::ToolCall(call_1.clone()),
                ModelInputItem::ToolCall(call_2.clone()),
                ModelInputItem::ToolResult(result(&call_2, "two")),
                ModelInputItem::ToolResult(result(&call_1, "one")),
            ],
            vec![tool],
            GenerationConfig::new(Some(512), true).expect("valid generation"),
            1,
        )
        .expect("valid request");

        let rendered = render_chat_request(&request).expect("request should render");
        assert_eq!(rendered["stream"], true);
        assert_eq!(rendered["stream_options"]["include_usage"], true);
        assert_eq!(rendered["parallel_tool_calls"], true);
        assert_eq!(rendered["max_completion_tokens"], 512);
        assert_eq!(rendered["messages"][2]["role"], "assistant");
        assert_eq!(rendered["messages"][2]["tool_calls"][0]["id"], "call-1");
        assert_eq!(rendered["messages"][2]["tool_calls"][1]["id"], "call-2");
        assert_eq!(rendered["messages"][3]["tool_call_id"], "call-2");
        assert_eq!(rendered["messages"][4]["tool_call_id"], "call-1");
        assert_eq!(rendered["tools"][0]["function"]["strict"], true);
    }

    #[test]
    fn renders_user_png_as_ordered_chat_image_url_content() {
        let content = ModelContent::user_with_images(
            "inspect [Image #1]",
            vec![
                ModelImage::png(
                    "[Image #1]",
                    Arc::<[u8]>::from([137, 80, 78, 71, 13, 10, 26, 10, 42]),
                    1,
                    1,
                )
                .expect("valid image"),
            ],
        )
        .expect("valid image content");
        let request = ModelRequest::new(
            ModelName::new("gpt-chat-image-test").expect("valid model"),
            vec![ModelMessage::new(ModelMessageRole::User, content).expect("valid user message")],
            Vec::new(),
            GenerationConfig::default(),
        )
        .expect("valid request");

        let rendered = render_chat_request(&request).expect("request should render");

        assert_eq!(
            rendered["messages"][0]["content"],
            json!([
                {"type": "text", "text": "<image name=[Image #1]>"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw0KGgoq"}},
                {"type": "text", "text": "</image>"},
                {"type": "text", "text": "inspect [Image #1]"}
            ])
        );
    }

    fn call(id: &str, query: &str) -> ModelToolCall {
        ModelToolCall::new(
            ModelToolCallId::new(id).expect("valid id"),
            ToolName::new("search").expect("valid name"),
            ToolArguments::try_from(json!({"query": query})).expect("valid arguments"),
        )
    }

    fn result(call: &ModelToolCall, text: &str) -> ModelToolResult {
        ModelToolResult::new(
            call.id().clone(),
            ToolCallResultStatus::Succeeded,
            ModelToolResultContent::text(text).expect("valid result"),
            None,
        )
        .expect("valid result")
    }
}
