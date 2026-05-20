//! Request rendering from Merry-owned model types.

use crate::{
    OpenAiProviderError,
    wire::{ResponsesInputItem, ResponsesRequest, ResponsesTool},
};
use merry_llm::{ModelMessageRole, ModelRequest, ModelToolContinuation};
use serde_json::{Map, Value};

#[allow(dead_code)]
pub(crate) fn render_responses_request(
    request: &ModelRequest,
) -> Result<Value, OpenAiProviderError> {
    if request.generation().allow_parallel_tool_calls() {
        return Err(OpenAiProviderError::invalid_request(
            "parallel tool calls are not supported by the OpenAI provider adapter yet",
        ));
    }

    let mut input = request
        .messages()
        .iter()
        .map(|message| {
            ResponsesInputItem::message(render_role(message.role()), message.content().as_text())
        })
        .collect::<Vec<_>>();
    append_tool_continuation_input(request, &mut input)?;

    let tools = request
        .tools()
        .iter()
        .map(|tool| {
            Ok(ResponsesTool {
                kind: "function",
                name: tool.name().as_str(),
                description: tool.description(),
                parameters: schema_as_value(tool)?,
            })
        })
        .collect::<Result<Vec<_>, OpenAiProviderError>>()?;

    let wire = ResponsesRequest {
        model: request.model().as_str(),
        input,
        stream: true,
        store: false,
        parallel_tool_calls: false,
        max_output_tokens: request.generation().max_output_tokens(),
        tool_choice: if tools.is_empty() { None } else { Some("auto") },
        tools,
    };

    serde_json::to_value(wire).map_err(|error| {
        OpenAiProviderError::invalid_request(format!(
            "failed to serialize Responses request: {error}"
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

fn append_tool_continuation_input<'a>(
    request: &'a ModelRequest,
    input: &mut Vec<ResponsesInputItem<'a>>,
) -> Result<(), OpenAiProviderError> {
    let continuations = request.continuations();
    input.reserve(continuations.len().saturating_mul(2));
    for continuation in continuations {
        let continuation = render_tool_continuation(continuation)?;
        input.push(ResponsesInputItem::function_call(
            continuation.id,
            continuation.name,
            continuation.arguments,
        ));
        input.push(ResponsesInputItem::function_call_output(
            continuation.id,
            continuation.result_content,
        ));
    }

    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct RenderedToolContinuation<'a> {
    id: &'a str,
    name: &'a str,
    arguments: String,
    result_content: &'a str,
}

fn render_tool_continuation<'a>(
    continuation: &'a ModelToolContinuation,
) -> Result<RenderedToolContinuation<'a>, OpenAiProviderError> {
    let call = continuation.call();
    let arguments = stringify_json_object(call.arguments().as_object(), "tool call arguments")?;

    Ok(RenderedToolContinuation {
        id: call.id().as_str(),
        name: call.name().as_str(),
        arguments,
        result_content: continuation.result().content().as_str(),
    })
}

fn stringify_json_object(
    value: &Map<String, Value>,
    kind: &'static str,
) -> Result<String, OpenAiProviderError> {
    stringify_json(value, kind)
}

fn stringify_json<T>(value: &T, kind: &'static str) -> Result<String, OpenAiProviderError>
where
    T: serde::Serialize,
{
    serde_json::to_string(value).map_err(|error| {
        OpenAiProviderError::invalid_request(format!("failed to serialize {kind}: {error}"))
    })
}

#[cfg(test)]
mod tests {
    use super::render_tool_continuation;
    use crate::wire::ResponsesInputItem;
    use merry_core::{ErrorInfo, ToolCallResultStatus, ToolName};
    use merry_llm::{
        ModelToolCall, ModelToolCallId, ModelToolContinuation, ModelToolResult,
        ModelToolResultContent, ToolArguments,
    };
    use serde_json::json;

    fn continuation_with_result_content(content: ModelToolResultContent) -> ModelToolContinuation {
        let call = ModelToolCall::new(
            ModelToolCallId::new("call_abc123").expect("valid call id"),
            ToolName::new("lookup_weather").expect("valid tool name"),
            ToolArguments::try_from(json!({
                "city": "Shanghai",
                "units": "metric"
            }))
            .expect("valid arguments"),
        );
        let result = ModelToolResult::new(
            call.id().clone(),
            ToolCallResultStatus::Succeeded,
            content,
            None,
        )
        .expect("valid result");

        ModelToolContinuation::new(call, result).expect("valid continuation")
    }

    fn failed_continuation() -> ModelToolContinuation {
        let call = ModelToolCall::new(
            ModelToolCallId::new("call_failed").expect("valid call id"),
            ToolName::new("lookup_weather").expect("valid tool name"),
            ToolArguments::try_from(json!({
                "city": "Shanghai"
            }))
            .expect("valid arguments"),
        );
        let result = ModelToolResult::new(
            call.id().clone(),
            ToolCallResultStatus::Failed,
            ModelToolResultContent::json(r#"{"stderr":"permission denied"}"#)
                .expect("valid JSON result content"),
            Some(
                ErrorInfo::new("tool_failed", "Tool exited with status 2")
                    .expect("valid diagnostic"),
            ),
        )
        .expect("valid failed result");

        ModelToolContinuation::new(call, result).expect("valid continuation")
    }

    #[test]
    fn renders_tool_continuation_arguments_as_json_string_and_text_result() {
        let continuation = continuation_with_result_content(
            ModelToolResultContent::text("22 C and clear").expect("valid text result content"),
        );
        let continuation =
            render_tool_continuation(&continuation).expect("continuation should render");

        assert_eq!(continuation.id, "call_abc123");
        assert_eq!(continuation.name, "lookup_weather");
        assert_eq!(
            continuation.arguments,
            r#"{"city":"Shanghai","units":"metric"}"#
        );
        assert_eq!(continuation.result_content, "22 C and clear");
    }

    #[test]
    fn renders_tool_continuation_json_result_as_content_string() {
        let continuation = continuation_with_result_content(
            ModelToolResultContent::json(r#"{"ok":true,"temperature_c":22}"#)
                .expect("valid JSON result content"),
        );
        let continuation =
            render_tool_continuation(&continuation).expect("continuation should render");

        assert_eq!(
            continuation.arguments,
            r#"{"city":"Shanghai","units":"metric"}"#
        );
        assert_eq!(
            continuation.result_content,
            r#"{"ok":true,"temperature_c":22}"#
        );
    }

    #[test]
    fn appends_tool_continuations_as_responses_input_items_in_order() {
        let mut input = vec![ResponsesInputItem::message("user", "Weather in Shanghai?")];
        let request = merry_llm::ModelRequest::new_with_continuations(
            merry_llm::ModelName::new("debug-model").expect("valid model name"),
            vec![
                merry_llm::ModelMessage::new(
                    merry_llm::ModelMessageRole::User,
                    merry_llm::ModelContent::text("Weather in Shanghai?").expect("valid content"),
                )
                .expect("valid message"),
            ],
            Vec::new(),
            vec![continuation_with_result_content(
                ModelToolResultContent::json(r#"{"temperature_c":22}"#)
                    .expect("valid JSON result content"),
            )],
            merry_llm::GenerationConfig::default(),
        )
        .expect("valid request");

        super::append_tool_continuation_input(&request, &mut input)
            .expect("continuation input should render");

        assert_eq!(
            serde_json::to_value(input).expect("input should serialize"),
            json!([
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "input_text",
                            "text": "Weather in Shanghai?"
                        }
                    ]
                },
                {
                    "type": "function_call",
                    "call_id": "call_abc123",
                    "name": "lookup_weather",
                    "arguments": "{\"city\":\"Shanghai\",\"units\":\"metric\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_abc123",
                    "output": "{\"temperature_c\":22}"
                }
            ])
        );
    }

    #[test]
    fn failed_tool_result_uses_exact_content_without_special_wire_field() {
        let continuation = failed_continuation();
        let continuation =
            render_tool_continuation(&continuation).expect("continuation should render");

        assert_eq!(continuation.id, "call_failed");
        assert_eq!(
            continuation.result_content,
            r#"{"stderr":"permission denied"}"#
        );
    }
}
