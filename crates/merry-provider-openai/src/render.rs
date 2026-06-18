//! Request rendering from Merry-owned model types.

use crate::{
    OpenAiProviderError,
    wire::{
        ResponsesInputItem, ResponsesReasoning, ResponsesRequest, ResponsesText,
        ResponsesTextFormat, ResponsesTool,
    },
};
use merry_llm::{
    ModelInputItem, ModelMessageRole, ModelRequest, ModelResponseFormat,
    ModelStructuredOutputFormat, ModelToolCall,
};
use serde_json::{Map, Value};

#[allow(dead_code)]
pub(crate) fn render_responses_request(
    request: &ModelRequest,
) -> Result<Value, OpenAiProviderError> {
    render_responses_request_with_prompt_cache_key(request, None)
}

#[allow(dead_code)]
pub(crate) fn render_responses_request_with_prompt_cache_key(
    request: &ModelRequest,
    prompt_cache_key: Option<&str>,
) -> Result<Value, OpenAiProviderError> {
    if request.generation().allow_parallel_tool_calls() {
        return Err(OpenAiProviderError::invalid_request(
            "parallel tool calls are not supported by the OpenAI provider adapter yet",
        ));
    }

    let input = render_input_items(request.input())?;

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
        prompt_cache_key,
        max_output_tokens: request.generation().max_output_tokens(),
        reasoning: request
            .generation()
            .reasoning_effort()
            .map(|effort| ResponsesReasoning {
                effort: effort.as_str(),
            }),
        text: render_response_format(request.response_format())?,
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

fn render_response_format<'a>(
    format: Option<&'a ModelResponseFormat>,
) -> Result<Option<ResponsesText<'a>>, OpenAiProviderError> {
    match format {
        None => Ok(None),
        Some(ModelResponseFormat::StructuredOutput(format)) => {
            Ok(Some(render_structured_output_format(format)?))
        }
    }
}

fn render_structured_output_format<'a>(
    format: &'a ModelStructuredOutputFormat,
) -> Result<ResponsesText<'a>, OpenAiProviderError> {
    if format.schema().as_object().is_none() {
        return Err(OpenAiProviderError::invalid_request(format!(
            "structured output {} schema must be a JSON object",
            format.name()
        )));
    }

    Ok(ResponsesText {
        format: ResponsesTextFormat {
            kind: "json_schema",
            name: format.name(),
            strict: format.strict(),
            schema: format.schema().as_value(),
        },
    })
}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
struct RenderedToolContinuation<'a> {
    id: &'a str,
    name: &'a str,
    arguments: String,
    result_content: &'a str,
}

fn render_input_items<'a>(
    input: &'a [ModelInputItem],
) -> Result<Vec<ResponsesInputItem<'a>>, OpenAiProviderError> {
    input
        .iter()
        .map(|item| match item {
            ModelInputItem::Message(message) => Ok(ResponsesInputItem::message(
                render_role(message.role()),
                message.content().as_text(),
            )),
            ModelInputItem::ToolCall(call) => {
                let call = render_tool_call(call)?;
                Ok(ResponsesInputItem::function_call(
                    call.id,
                    call.name,
                    call.arguments,
                ))
            }
            ModelInputItem::ToolResult(result) => Ok(ResponsesInputItem::function_call_output(
                result.call_id().as_str(),
                result.content().as_str(),
            )),
        })
        .collect()
}

fn render_tool_call<'a>(
    call: &'a ModelToolCall,
) -> Result<RenderedToolCall<'a>, OpenAiProviderError> {
    let arguments = stringify_json_object(call.arguments().as_object(), "tool call arguments")?;

    Ok(RenderedToolCall {
        id: call.id().as_str(),
        name: call.name().as_str(),
        arguments,
    })
}

#[derive(Debug, PartialEq, Eq)]
struct RenderedToolCall<'a> {
    id: &'a str,
    name: &'a str,
    arguments: String,
}

#[cfg(test)]
fn render_tool_continuation<'a>(
    continuation: &'a merry_llm::ModelToolContinuation,
) -> Result<RenderedToolContinuation<'a>, OpenAiProviderError> {
    let result = continuation.result();
    let call = continuation.call();
    let call = render_tool_call(call)?;

    Ok(RenderedToolContinuation {
        id: call.id,
        name: call.name,
        arguments: call.arguments,
        result_content: result.content().as_str(),
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
    use merry_core::{ErrorInfo, ToolCallResultStatus, ToolName};
    use merry_llm::{
        GenerationConfig, ModelContent, ModelInputItem, ModelMessage, ModelMessageRole, ModelName,
        ModelRequest, ModelToolCall, ModelToolCallId, ModelToolContinuation, ModelToolResult,
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

        let rendered = super::render_responses_request(&request).expect("request should render");

        assert_eq!(
            rendered["input"],
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
    fn renders_ordered_input_items_without_moving_tool_items_to_tail() {
        let call = ModelToolCall::new(
            ModelToolCallId::new("call_ordered").expect("valid call id"),
            ToolName::new("lookup_weather").expect("valid tool name"),
            ToolArguments::try_from(json!({ "city": "Shanghai" })).expect("valid arguments"),
        );
        let result = ModelToolResult::new(
            call.id().clone(),
            ToolCallResultStatus::Succeeded,
            ModelToolResultContent::text("22 C").expect("valid result content"),
            None,
        )
        .expect("valid result");
        let request = ModelRequest::new_with_input_and_stable_prefix(
            ModelName::new("debug-model").expect("valid model name"),
            vec![
                ModelInputItem::Message(
                    ModelMessage::new(
                        ModelMessageRole::User,
                        ModelContent::text("Weather in Shanghai?").expect("valid content"),
                    )
                    .expect("valid message"),
                ),
                ModelInputItem::ToolCall(call),
                ModelInputItem::ToolResult(result),
                ModelInputItem::Message(
                    ModelMessage::new(
                        ModelMessageRole::User,
                        ModelContent::text("Now summarize.").expect("valid content"),
                    )
                    .expect("valid message"),
                ),
            ],
            Vec::new(),
            GenerationConfig::default(),
            0,
        )
        .expect("valid request");

        let rendered = super::render_responses_request(&request).expect("request renders");

        assert_eq!(
            rendered["input"],
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
                    "call_id": "call_ordered",
                    "name": "lookup_weather",
                    "arguments": "{\"city\":\"Shanghai\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_ordered",
                    "output": "22 C"
                },
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "input_text",
                            "text": "Now summarize."
                        }
                    ]
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
