use crate::{
    AnthropicProviderConfig, AnthropicProviderError,
    wire::{
        AnthropicContentBlock, AnthropicMessage, AnthropicOutputConfig, AnthropicOutputFormat,
        AnthropicRequest, AnthropicTool, AnthropicToolChoice,
    },
};
use merry_core::ToolCallResultStatus;
use merry_llm::{ModelInputItem, ModelMessageRole, ModelRequest, ModelResponseFormat};
use serde_json::Value;

pub(crate) fn render_anthropic_request(
    config: &AnthropicProviderConfig,
    request: &ModelRequest,
) -> Result<Value, AnthropicProviderError> {
    let system = request
        .input()
        .iter()
        .filter_map(|item| match item {
            ModelInputItem::Message(message) if message.role() == ModelMessageRole::System => {
                Some(message.content().as_text())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let system = (!system.is_empty()).then(|| system.join("\n\n"));
    let messages = render_messages(request);
    if messages.is_empty() {
        return Err(AnthropicProviderError::invalid_request(
            "Anthropic request must contain at least one user or assistant message",
        ));
    }
    let tools = request
        .tools()
        .iter()
        .map(|tool| AnthropicTool {
            name: tool.name().as_str(),
            description: tool.description(),
            input_schema: tool.input_schema().as_schema(),
        })
        .collect::<Vec<_>>();
    let format = request.response_format().map(|format| match format {
        ModelResponseFormat::StructuredOutput(format) => AnthropicOutputFormat {
            kind: "json_schema",
            schema: format.schema(),
        },
    });
    let effort = request
        .generation()
        .reasoning_effort()
        .map(|effort| effort.as_str());
    let output_config =
        (format.is_some() || effort.is_some()).then_some(AnthropicOutputConfig { effort, format });
    let has_tools = !tools.is_empty();
    let wire = AnthropicRequest {
        model: request.model().as_str(),
        system,
        messages,
        max_tokens: request
            .generation()
            .max_output_tokens()
            .unwrap_or(config.default_max_output_tokens().get()),
        stream: true,
        tools,
        tool_choice: (has_tools && !request.generation().allow_parallel_tool_calls()).then_some(
            AnthropicToolChoice {
                kind: "auto",
                disable_parallel_tool_use: true,
            },
        ),
        output_config,
    };
    serde_json::to_value(wire).map_err(|error| {
        AnthropicProviderError::invalid_request(format!(
            "failed to serialize Anthropic request: {error}"
        ))
    })
}

fn render_messages<'a>(request: &'a ModelRequest) -> Vec<AnthropicMessage<'a>> {
    let mut messages = Vec::new();
    let mut index = 0;
    while index < request.input().len() {
        match &request.input()[index] {
            ModelInputItem::Message(message) if message.role() == ModelMessageRole::System => {
                index += 1;
            }
            ModelInputItem::Message(message) => {
                messages.push(AnthropicMessage {
                    role: match message.role() {
                        ModelMessageRole::User => "user",
                        ModelMessageRole::Assistant => "assistant",
                        ModelMessageRole::System => unreachable!("system messages are filtered"),
                    },
                    content: vec![AnthropicContentBlock::Text {
                        text: message.content().as_text(),
                    }],
                });
                index += 1;
            }
            ModelInputItem::ToolCall(_) => {
                let mut content = Vec::new();
                while let Some(ModelInputItem::ToolCall(call)) = request.input().get(index) {
                    content.push(AnthropicContentBlock::ToolUse {
                        id: call.id().as_str(),
                        name: call.name().as_str(),
                        input: call.arguments().as_object(),
                    });
                    index += 1;
                }
                messages.push(AnthropicMessage {
                    role: "assistant",
                    content,
                });
            }
            ModelInputItem::ToolResult(_) => {
                let mut content = Vec::new();
                while let Some(ModelInputItem::ToolResult(result)) = request.input().get(index) {
                    content.push(AnthropicContentBlock::ToolResult {
                        tool_use_id: result.call_id().as_str(),
                        content: result.content().as_str(),
                        is_error: result.status() == ToolCallResultStatus::Failed,
                    });
                    index += 1;
                }
                messages.push(AnthropicMessage {
                    role: "user",
                    content,
                });
            }
        }
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;
    use merry_core::{ToolInputSchema, ToolName, ToolSpec};
    use merry_llm::{
        GenerationConfig, ModelContent, ModelInputItem, ModelMessage, ModelName, ModelToolCall,
        ModelToolCallId, ModelToolResult, ModelToolResultContent, ToolArguments,
    };
    use schemars::Schema;
    use serde_json::json;

    #[test]
    fn renders_system_tool_batches_results_and_output_limit() {
        let call_1 = call("call-1", "a");
        let call_2 = call("call-2", "b");
        let request = ModelRequest::new_with_input_and_stable_prefix(
            ModelName::new("claude-test").expect("valid model"),
            vec![
                message(ModelMessageRole::System, "Be concise"),
                message(ModelMessageRole::User, "Search twice"),
                ModelInputItem::ToolCall(call_1.clone()),
                ModelInputItem::ToolCall(call_2.clone()),
                ModelInputItem::ToolResult(result(&call_2, "two")),
                ModelInputItem::ToolResult(result(&call_1, "one")),
            ],
            vec![tool_spec()],
            GenerationConfig::new(Some(777), true).expect("valid generation"),
            1,
        )
        .expect("valid request");
        let rendered = render_anthropic_request(
            &AnthropicProviderConfig::new("key").expect("valid config"),
            &request,
        )
        .expect("request should render");

        assert_eq!(rendered["system"], "Be concise");
        assert_eq!(rendered["max_tokens"], 777);
        assert_eq!(rendered["stream"], true);
        assert_eq!(rendered["messages"][1]["role"], "assistant");
        assert_eq!(rendered["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(rendered["messages"][1]["content"][1]["id"], "call-2");
        assert_eq!(rendered["messages"][2]["role"], "user");
        assert_eq!(
            rendered["messages"][2]["content"][0]["tool_use_id"],
            "call-2"
        );
        assert!(rendered.get("tool_choice").is_none());
    }

    fn message(role: ModelMessageRole, text: &str) -> ModelInputItem {
        ModelInputItem::Message(
            ModelMessage::new(role, ModelContent::text(text).expect("valid content"))
                .expect("valid message"),
        )
    }

    fn tool_spec() -> ToolSpec {
        ToolSpec::new(
            ToolName::new("search").expect("valid name"),
            "Search notes",
            ToolInputSchema::new(
                Schema::try_from(json!({
                    "type": "object",
                    "properties": {"query": {"type": "string"}},
                    "required": ["query"]
                }))
                .expect("valid schema"),
            )
            .expect("valid schema"),
        )
        .expect("valid tool")
    }

    fn call(id: &str, query: &str) -> ModelToolCall {
        ModelToolCall::new(
            ModelToolCallId::new(id).expect("valid id"),
            ToolName::new("search").expect("valid name"),
            ToolArguments::try_from(json!({"query": query})).expect("valid arguments"),
        )
    }

    fn result(call: &ModelToolCall, text: &str) -> ModelToolResult {
        ModelToolResult::succeeded(
            call.id().clone(),
            ModelToolResultContent::text(text).expect("valid result"),
        )
    }
}
