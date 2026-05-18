//! Response and streaming event parsing into Merry-owned model types.

use crate::{
    OpenAiProviderError,
    wire::{
        ChatCompletionResponse, ChatCompletionStreamChoice, ChatCompletionStreamChunk,
        ChatCompletionStreamToolCall, ChatToolCall, ChatUsage,
    },
};
use merry_core::ToolName;
use merry_llm::{
    FinishReason, ModelEvent, ModelOutput, ModelResponse, ModelToolCall, ModelToolCallId,
    ToolArguments, Usage,
};
use serde_json::Value;
use std::collections::BTreeMap;

#[allow(dead_code)]
pub(crate) fn parse_chat_completion_response(
    fixture: &str,
) -> Result<ModelResponse, merry_llm::ModelError> {
    parse_chat_completion_response_inner(fixture).map_err(Into::into)
}

fn parse_chat_completion_response_inner(body: &str) -> Result<ModelResponse, OpenAiProviderError> {
    let response: ChatCompletionResponse = serde_json::from_str(body).map_err(|error| {
        OpenAiProviderError::protocol(format!(
            "failed to parse Chat Completions response: {error}"
        ))
    })?;

    parse_completion_response(response)
}

#[allow(dead_code)]
pub(crate) fn parse_chat_completion_stream_events(
    jsonl: &str,
) -> Result<Vec<ModelEvent>, merry_llm::ModelError> {
    parse_chat_completion_stream_events_inner(jsonl).map_err(Into::into)
}

fn parse_chat_completion_stream_events_inner(
    jsonl: &str,
) -> Result<Vec<ModelEvent>, OpenAiProviderError> {
    let mut events = vec![ModelEvent::Started];
    let mut aggregate_text = String::new();
    let mut tool_call_buffers = BTreeMap::new();
    let mut tool_calls = Vec::new();
    let mut finish_reason = None;
    let mut completed = false;

    for raw_line in jsonl.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        let data = line
            .strip_prefix("data: ")
            .ok_or_else(|| OpenAiProviderError::protocol("stream line must start with `data: `"))?;
        if data == "[DONE]" {
            continue;
        }
        if completed {
            return Err(OpenAiProviderError::protocol(
                "stream emitted a chunk after completion",
            ));
        }

        let chunk: ChatCompletionStreamChunk = serde_json::from_str(data).map_err(|error| {
            OpenAiProviderError::protocol(format!("failed to parse stream chunk: {error}"))
        })?;

        if chunk.choices.is_empty() {
            if chunk.usage.is_none() {
                return Err(OpenAiProviderError::protocol(
                    "stream chunk with empty choices must include usage",
                ));
            }

            let usage = chunk.usage.map(usage_from_wire);
            let finish_reason = finish_reason.ok_or_else(|| {
                OpenAiProviderError::protocol("usage chunk arrived before finish reason")
            })?;
            events.push(ModelEvent::Completed {
                response: ModelResponse::new(
                    stream_outputs(&aggregate_text, &tool_calls),
                    finish_reason,
                    usage,
                ),
            });
            completed = true;
            continue;
        }

        if chunk.choices.len() != 1 {
            return Err(OpenAiProviderError::protocol(
                "stream chunks with multiple choices are not supported",
            ));
        }

        let choice = chunk
            .choices
            .into_iter()
            .next()
            .expect("length checked above");
        parse_stream_choice(
            choice,
            &mut events,
            &mut aggregate_text,
            &mut tool_call_buffers,
            &mut tool_calls,
            &mut finish_reason,
        )?;
    }

    if !completed {
        return Err(OpenAiProviderError::protocol(
            "stream ended before completion usage chunk",
        ));
    }

    Ok(events)
}

fn parse_completion_response(
    response: ChatCompletionResponse,
) -> Result<ModelResponse, OpenAiProviderError> {
    if response.choices.len() != 1 {
        return Err(OpenAiProviderError::protocol(
            "Chat Completions response must contain exactly one choice",
        ));
    }

    let choice = response
        .choices
        .into_iter()
        .next()
        .expect("length checked above");
    let finish_reason = parse_finish_reason(choice.finish_reason.as_deref())?;
    let mut outputs = Vec::new();

    if let Some(content) = choice.message.content.filter(|content| !content.is_empty()) {
        outputs.push(ModelOutput::text(&content));
    }

    for tool_call in choice.message.tool_calls {
        outputs.push(ModelOutput::tool_call(parse_tool_call(tool_call)?));
    }

    Ok(ModelResponse::new(
        outputs,
        finish_reason,
        response.usage.map(usage_from_wire),
    ))
}

fn parse_stream_choice(
    choice: ChatCompletionStreamChoice,
    events: &mut Vec<ModelEvent>,
    aggregate_text: &mut String,
    tool_call_buffers: &mut BTreeMap<u64, StreamToolCallBuffer>,
    tool_calls: &mut Vec<ModelToolCall>,
    finish_reason: &mut Option<FinishReason>,
) -> Result<(), OpenAiProviderError> {
    if let Some(delta) = choice.delta.content.filter(|delta| !delta.is_empty()) {
        aggregate_text.push_str(&delta);
        events.push(ModelEvent::OutputTextDelta { delta });
    }

    for tool_call_delta in choice.delta.tool_calls {
        merge_stream_tool_call_delta(tool_call_buffers, tool_call_delta)?;
    }

    if let Some(raw_finish_reason) = choice.finish_reason {
        let parsed_finish_reason = parse_finish_reason(Some(&raw_finish_reason))?;
        if parsed_finish_reason == FinishReason::ToolCalls {
            if tool_call_buffers.is_empty() {
                return Err(OpenAiProviderError::protocol(
                    "tool call finish reason had no streamed tool calls",
                ));
            }

            for (_, buffer) in std::mem::take(tool_call_buffers) {
                let tool_call = buffer.into_model_tool_call()?;
                events.push(ModelEvent::ToolCallRequested {
                    call: tool_call.clone(),
                });
                tool_calls.push(tool_call);
            }
        }
        *finish_reason = Some(parsed_finish_reason);
    }

    Ok(())
}

fn parse_tool_call(tool_call: ChatToolCall) -> Result<ModelToolCall, OpenAiProviderError> {
    if tool_call.kind != "function" {
        return Err(OpenAiProviderError::protocol(
            "only function tool calls are supported",
        ));
    }

    let id = required_field(tool_call.id, "tool call id")?;
    let name = required_field(tool_call.function.name, "tool call function name")?;
    let arguments = required_field(tool_call.function.arguments, "tool call arguments")?;
    let arguments: Value = serde_json::from_str(&arguments).map_err(|error| {
        OpenAiProviderError::protocol(format!("tool call arguments must be valid JSON: {error}"))
    })?;
    let arguments = ToolArguments::try_from(arguments).map_err(|error| {
        OpenAiProviderError::protocol(format!(
            "tool call arguments must be a JSON object: {error}"
        ))
    })?;
    let id = ModelToolCallId::new(&id).map_err(|error| {
        OpenAiProviderError::protocol(format!("tool call id is invalid: {error}"))
    })?;
    let name = ToolName::new(&name).map_err(|error| {
        OpenAiProviderError::protocol(format!("tool call function name is invalid: {error}"))
    })?;

    Ok(ModelToolCall::new(id, name, arguments))
}

fn parse_finish_reason(reason: Option<&str>) -> Result<FinishReason, OpenAiProviderError> {
    match reason {
        Some("stop") => Ok(FinishReason::Stop),
        Some("tool_calls" | "function_call") => Ok(FinishReason::ToolCalls),
        Some("length") => Ok(FinishReason::Length),
        Some("content_filter") => Ok(FinishReason::Error),
        Some(other) => Err(OpenAiProviderError::protocol(format!(
            "unsupported finish reason `{other}`"
        ))),
        None => Err(OpenAiProviderError::protocol("finish reason is missing")),
    }
}

fn usage_from_wire(usage: ChatUsage) -> Usage {
    Usage::new(usage.prompt_tokens, usage.completion_tokens)
}

fn stream_outputs(aggregate_text: &str, tool_calls: &[ModelToolCall]) -> Vec<ModelOutput> {
    let mut outputs = Vec::new();
    if !aggregate_text.is_empty() {
        outputs.push(ModelOutput::text(aggregate_text));
    }
    outputs.extend(tool_calls.iter().cloned().map(ModelOutput::tool_call));
    outputs
}

fn required_field(
    value: Option<String>,
    field: &'static str,
) -> Result<String, OpenAiProviderError> {
    match value {
        Some(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(OpenAiProviderError::protocol(format!("{field} is missing"))),
    }
}

#[derive(Debug, Default)]
struct StreamToolCallBuffer {
    id: Option<String>,
    kind: Option<String>,
    name: Option<String>,
    arguments: String,
}

impl StreamToolCallBuffer {
    fn merge(&mut self, delta: ChatCompletionStreamToolCall) -> Result<(), OpenAiProviderError> {
        merge_optional_field(&mut self.id, delta.id, "streamed tool call id")?;
        merge_optional_field(&mut self.kind, delta.kind, "streamed tool call type")?;

        if let Some(function) = delta.function {
            merge_optional_field(
                &mut self.name,
                function.name,
                "streamed tool call function name",
            )?;
            if let Some(arguments) = function.arguments {
                self.arguments.push_str(&arguments);
            }
        }

        Ok(())
    }

    fn into_model_tool_call(self) -> Result<ModelToolCall, OpenAiProviderError> {
        let kind = required_field(self.kind, "streamed tool call type")?;
        if kind != "function" {
            return Err(OpenAiProviderError::protocol(
                "only streamed function tool calls are supported",
            ));
        }

        parse_tool_call(ChatToolCall {
            id: Some(required_field(self.id, "streamed tool call id")?),
            kind,
            function: crate::wire::ChatToolCallFunction {
                name: Some(required_field(
                    self.name,
                    "streamed tool call function name",
                )?),
                arguments: Some(self.arguments),
            },
        })
    }
}

fn merge_stream_tool_call_delta(
    buffers: &mut BTreeMap<u64, StreamToolCallBuffer>,
    delta: ChatCompletionStreamToolCall,
) -> Result<(), OpenAiProviderError> {
    buffers.entry(delta.index).or_default().merge(delta)
}

fn merge_optional_field(
    slot: &mut Option<String>,
    value: Option<String>,
    field: &'static str,
) -> Result<(), OpenAiProviderError> {
    if let Some(value) = value {
        if value.trim().is_empty() {
            return Err(OpenAiProviderError::protocol(format!("{field} is blank")));
        }

        match slot {
            Some(existing) if existing != &value => Err(OpenAiProviderError::protocol(format!(
                "{field} changed across stream fragments"
            ))),
            Some(_) => Ok(()),
            None => {
                *slot = Some(value);
                Ok(())
            }
        }
    } else {
        Ok(())
    }
}
