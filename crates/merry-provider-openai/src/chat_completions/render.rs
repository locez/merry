use super::wire::{
    ChatContentPart, ChatImageUrl, ChatJsonSchema, ChatMessage, ChatRequest, ChatResponseFormat,
    ChatStreamOptions, ChatTool, ChatToolCall, ChatToolCallFunction, ChatToolFunction,
};
use crate::{OpenAiProtocol, OpenAiProviderError, image::png_data_url};
use merry_llm::{
    ModelCapabilities, ModelContentPart, ModelInputItem, ModelMessageRole, ModelRequest,
    ModelResponseFormat, ModelToolResult,
};
use serde_json::Value;
use std::borrow::Cow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReasoningReplay {
    Enabled,
    Disabled,
}

impl ReasoningReplay {
    pub(crate) const fn from_capabilities(capabilities: &ModelCapabilities) -> Self {
        if capabilities.replays_reasoning_input() {
            Self::Enabled
        } else {
            Self::Disabled
        }
    }
}

pub(crate) fn render_chat_request(
    request: &ModelRequest,
    reasoning_replay: ReasoningReplay,
) -> Result<Value, OpenAiProviderError> {
    let messages = render_messages(request, reasoning_replay)?;
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
    let service_tier = match request.generation().service_tier() {
        Some(service_tier)
            if !OpenAiProtocol::ChatCompletions.supports_service_tier(service_tier) =>
        {
            return Err(OpenAiProviderError::invalid_request(format!(
                "service_tier \"{service_tier}\" is not supported by the Chat Completions \
                 protocol; it requires the Responses protocol"
            )));
        }
        service_tier => service_tier.map(|tier| tier.as_str()),
    };
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
        service_tier,
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
    reasoning_replay: ReasoningReplay,
) -> Result<Vec<ChatMessage<'a>>, OpenAiProviderError> {
    let mut messages = Vec::new();
    let mut index = 0;
    let mut pending_reasoning: Option<&str> = None;
    while index < request.input().len() {
        match &request.input()[index] {
            ModelInputItem::Message(message) => {
                let role = render_role(message.role());
                let reasoning_content = if message.role() == ModelMessageRole::Assistant {
                    pending_reasoning.take()
                } else {
                    if pending_reasoning.is_some() {
                        return Err(OpenAiProviderError::invalid_request(
                            "assistant reasoning must be followed by an assistant message or tool call",
                        ));
                    }
                    None
                };
                if message.content().has_images() {
                    messages.push(ChatMessage::Multimodal {
                        role,
                        content: message
                            .content()
                            .parts()
                            .iter()
                            .map(|part| match part {
                                ModelContentPart::Text { text } => ChatContentPart::Text {
                                    text: Cow::Borrowed(text),
                                },
                                ModelContentPart::Image { image } => ChatContentPart::ImageUrl {
                                    image_url: ChatImageUrl {
                                        url: png_data_url(image.png_bytes()),
                                    },
                                },
                            })
                            .collect(),
                    });
                } else if message.role() == ModelMessageRole::Assistant
                    && matches!(
                        request.input().get(index + 1),
                        Some(ModelInputItem::ToolCall(_))
                    )
                {
                    index += 1;
                    let tool_calls = render_tool_calls(request.input(), &mut index)?;
                    messages.push(ChatMessage::AssistantToolCalls {
                        role,
                        content: Some(message.content().as_text()),
                        reasoning_content,
                        tool_calls,
                    });
                    continue;
                } else {
                    messages.push(ChatMessage::Text {
                        role,
                        content: message.content().as_text(),
                        reasoning_content,
                    });
                }
                index += 1;
            }
            ModelInputItem::Reasoning(reasoning) => {
                if reasoning_replay == ReasoningReplay::Enabled
                    && pending_reasoning.replace(reasoning.text()).is_some()
                {
                    return Err(OpenAiProviderError::invalid_request(
                        "assistant reasoning items must not be consecutive",
                    ));
                }
                index += 1;
            }
            ModelInputItem::ToolCall(_) => {
                let reasoning_content = pending_reasoning.take();
                let tool_calls = render_tool_calls(request.input(), &mut index)?;
                messages.push(ChatMessage::AssistantToolCalls {
                    role: "assistant",
                    content: None,
                    reasoning_content,
                    tool_calls,
                });
            }
            ModelInputItem::ToolResult(_) => {
                if pending_reasoning.is_some() {
                    return Err(OpenAiProviderError::invalid_request(
                        "assistant reasoning must be followed by an assistant message or tool call",
                    ));
                }
                let mut tool_images = Vec::new();
                while let Some(ModelInputItem::ToolResult(result)) = request.input().get(index) {
                    messages.push(ChatMessage::ToolResult {
                        role: "tool",
                        tool_call_id: result.call_id().as_str(),
                        content: result.content().as_str(),
                    });
                    append_tool_result_images(&mut tool_images, result);
                    index += 1;
                }
                if !tool_images.is_empty() {
                    messages.push(ChatMessage::Multimodal {
                        role: "user",
                        content: tool_images,
                    });
                }
            }
        }
    }
    if pending_reasoning.is_some() {
        return Err(OpenAiProviderError::invalid_request(
            "assistant reasoning must be followed by an assistant message or tool call",
        ));
    }
    Ok(messages)
}

fn render_tool_calls<'a>(
    input: &'a [ModelInputItem],
    index: &mut usize,
) -> Result<Vec<ChatToolCall<'a>>, OpenAiProviderError> {
    let mut tool_calls = Vec::new();
    while let Some(ModelInputItem::ToolCall(call)) = input.get(*index) {
        tool_calls.push(ChatToolCall {
            id: call.id().as_str(),
            kind: "function",
            function: ChatToolCallFunction {
                name: call.name().as_str(),
                arguments: serde_json::to_string(call.arguments().as_object()).map_err(
                    |error| {
                        OpenAiProviderError::invalid_request(format!(
                            "failed to serialize tool arguments: {error}"
                        ))
                    },
                )?,
            },
        });
        *index += 1;
    }
    Ok(tool_calls)
}

fn append_tool_result_images<'a>(
    content: &mut Vec<ChatContentPart<'a>>,
    result: &'a ModelToolResult,
) {
    for image in result.images() {
        content.push(ChatContentPart::Text {
            text: Cow::Owned(format!("<image name={}>", image.label())),
        });
        content.push(ChatContentPart::ImageUrl {
            image_url: ChatImageUrl {
                url: png_data_url(image.png_bytes()),
            },
        });
        content.push(ChatContentPart::Text {
            text: Cow::Borrowed("</image>"),
        });
    }
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
        ModelReasoning, ModelToolCall, ModelToolCallId, ModelToolResult, ModelToolResultContent,
        ToolArguments,
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

        let rendered =
            render_chat_request(&request, ReasoningReplay::Enabled).expect("request should render");
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
    fn renders_reasoning_on_the_assistant_tool_call_message() {
        let call = call("call-1", "alpha");
        let request = ModelRequest::new_with_input_and_stable_prefix(
            ModelName::new("deepseek-reasoner").expect("valid model"),
            vec![
                ModelInputItem::Reasoning(
                    ModelReasoning::new("Inspect the notes first.").expect("valid reasoning"),
                ),
                ModelInputItem::Message(
                    ModelMessage::new(
                        ModelMessageRole::Assistant,
                        ModelContent::text("Searching the notes").expect("valid content"),
                    )
                    .expect("valid message"),
                ),
                ModelInputItem::ToolCall(call.clone()),
                ModelInputItem::ToolResult(result(&call, "one")),
            ],
            Vec::new(),
            GenerationConfig::default(),
            0,
        )
        .expect("valid request");

        let rendered =
            render_chat_request(&request, ReasoningReplay::Enabled).expect("request should render");

        assert_eq!(rendered["messages"][0]["role"], "assistant");
        assert_eq!(rendered["messages"][0]["content"], "Searching the notes");
        assert_eq!(
            rendered["messages"][0]["reasoning_content"],
            "Inspect the notes first."
        );
        assert_eq!(rendered["messages"][0]["tool_calls"][0]["id"], "call-1");
        assert_eq!(rendered["messages"][1]["role"], "tool");
    }

    #[test]
    fn renders_reasoning_on_a_tool_only_assistant_message() {
        let call = call("call-1", "alpha");
        let request = ModelRequest::new_with_input_and_stable_prefix(
            ModelName::new("deepseek-reasoner").expect("valid model"),
            vec![
                ModelInputItem::Reasoning(
                    ModelReasoning::new("Call search.").expect("valid reasoning"),
                ),
                ModelInputItem::ToolCall(call.clone()),
                ModelInputItem::ToolResult(result(&call, "one")),
            ],
            Vec::new(),
            GenerationConfig::default(),
            0,
        )
        .expect("valid request");

        let rendered =
            render_chat_request(&request, ReasoningReplay::Enabled).expect("request should render");

        assert_eq!(rendered["messages"][0]["role"], "assistant");
        assert_eq!(rendered["messages"][0]["content"], json!(null));
        assert_eq!(rendered["messages"][0]["reasoning_content"], "Call search.");
        assert_eq!(rendered["messages"][0]["tool_calls"][0]["id"], "call-1");
        assert_eq!(rendered["messages"][1]["role"], "tool");
    }

    #[test]
    fn omits_reasoning_content_when_reasoning_replay_is_disabled() {
        let call = call("call-1", "alpha");
        let request = ModelRequest::new_with_input_and_stable_prefix(
            ModelName::new("gpt-5").expect("valid model"),
            vec![
                ModelInputItem::Reasoning(
                    ModelReasoning::new("Internal thought.").expect("valid reasoning"),
                ),
                ModelInputItem::Message(
                    ModelMessage::new(
                        ModelMessageRole::Assistant,
                        ModelContent::text("Searching").expect("valid content"),
                    )
                    .expect("valid message"),
                ),
                ModelInputItem::ToolCall(call.clone()),
                ModelInputItem::ToolResult(result(&call, "one")),
            ],
            Vec::new(),
            GenerationConfig::default(),
            0,
        )
        .expect("valid request");

        let rendered = render_chat_request(&request, ReasoningReplay::Disabled)
            .expect("request should render");

        assert_eq!(rendered["messages"][0]["role"], "assistant");
        assert_eq!(rendered["messages"][0]["content"], "Searching");
        assert!(rendered["messages"][0].get("reasoning_content").is_none());
        assert_eq!(rendered["messages"][0]["tool_calls"][0]["id"], "call-1");
    }

    #[test]
    fn rejects_invalid_reasoning_positions_when_replaying() {
        let reasoning_item = |text: &str| {
            ModelInputItem::Reasoning(ModelReasoning::new(text).expect("valid reasoning"))
        };
        let user_item = ModelInputItem::Message(
            ModelMessage::new(
                ModelMessageRole::User,
                ModelContent::text("next user").expect("valid content"),
            )
            .expect("valid message"),
        );
        let assistant_item = ModelInputItem::Message(
            ModelMessage::new(
                ModelMessageRole::Assistant,
                ModelContent::text("answer").expect("valid content"),
            )
            .expect("valid message"),
        );

        let consecutive = ModelRequest::new_with_input_and_stable_prefix(
            ModelName::new("deepseek-reasoner").expect("valid model"),
            vec![
                reasoning_item("first"),
                reasoning_item("second"),
                assistant_item,
            ],
            Vec::new(),
            GenerationConfig::default(),
            0,
        )
        .expect("valid request");
        assert!(render_chat_request(&consecutive, ReasoningReplay::Enabled).is_err());

        let before_user = ModelRequest::new_with_input_and_stable_prefix(
            ModelName::new("deepseek-reasoner").expect("valid model"),
            vec![reasoning_item("orphaned"), user_item],
            Vec::new(),
            GenerationConfig::default(),
            0,
        )
        .expect("valid request");
        assert!(render_chat_request(&before_user, ReasoningReplay::Enabled).is_err());

        let trailing = ModelRequest::new_with_input_and_stable_prefix(
            ModelName::new("deepseek-reasoner").expect("valid model"),
            vec![reasoning_item("trailing")],
            Vec::new(),
            GenerationConfig::default(),
            0,
        )
        .expect("valid request");
        assert!(render_chat_request(&trailing, ReasoningReplay::Enabled).is_err());
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

        let rendered =
            render_chat_request(&request, ReasoningReplay::Enabled).expect("request should render");

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

    #[test]
    fn renders_tool_result_images_after_contiguous_tool_messages() {
        let call_1 = call("call-1", "alpha");
        let call_2 = call("call-2", "beta");
        let screenshot = ModelImage::png(
            "browser-snapshot.png",
            Arc::<[u8]>::from([137, 80, 78, 71, 13, 10, 26, 10, 42]),
            1,
            1,
        )
        .expect("valid image");
        let result_1 = result(&call_1, "one").with_images(vec![screenshot]);
        let result_2 = result(&call_2, "two");
        let request = ModelRequest::new_with_input_and_stable_prefix(
            ModelName::new("gpt-chat-image-test").expect("valid model"),
            vec![
                ModelInputItem::ToolCall(call_1),
                ModelInputItem::ToolCall(call_2),
                ModelInputItem::ToolResult(result_1),
                ModelInputItem::ToolResult(result_2),
            ],
            Vec::new(),
            GenerationConfig::default(),
            0,
        )
        .expect("valid request");

        let rendered =
            render_chat_request(&request, ReasoningReplay::Enabled).expect("request should render");
        assert_eq!(rendered["messages"][1]["role"], "tool");
        assert_eq!(rendered["messages"][2]["role"], "tool");
        assert_eq!(rendered["messages"][3]["role"], "user");
        assert_eq!(
            rendered["messages"][3]["content"],
            json!([
                {"type": "text", "text": "<image name=browser-snapshot.png>"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw0KGgoq"}},
                {"type": "text", "text": "</image>"}
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
