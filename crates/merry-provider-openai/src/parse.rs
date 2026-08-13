//! Response and streaming event parsing into Merry-owned model types.

use crate::{
    OpenAiProviderError,
    wire::{
        ResponsesOutputContent, ResponsesOutputItem, ResponsesResponse, ResponsesStreamEvent,
        ResponsesStreamOutputItem, ResponsesUsage,
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
pub(crate) fn parse_responses_response(
    fixture: &str,
) -> Result<ModelResponse, merry_llm::ModelError> {
    parse_responses_response_inner(fixture).map_err(Into::into)
}

fn parse_responses_response_inner(body: &str) -> Result<ModelResponse, OpenAiProviderError> {
    let response: ResponsesResponse = serde_json::from_str(body).map_err(|error| {
        OpenAiProviderError::protocol(format!("failed to parse Responses response: {error}"))
    })?;

    parse_response(response)
}

#[allow(dead_code)]
pub(crate) fn parse_responses_stream_events(
    jsonl: &str,
) -> Result<Vec<ModelEvent>, merry_llm::ModelError> {
    parse_responses_stream_events_inner(jsonl).map_err(Into::into)
}

fn parse_responses_stream_events_inner(
    jsonl: &str,
) -> Result<Vec<ModelEvent>, OpenAiProviderError> {
    let mut events = vec![ModelEvent::Started];
    let mut parser = ResponsesStreamParser::new();

    for raw_line in jsonl.lines() {
        events.extend(parser.parse_sse_line(raw_line)?);
    }

    parser.finish()?;

    Ok(events)
}

pub(crate) struct ResponsesStreamParser {
    aggregate_text: String,
    tool_call_buffers: BTreeMap<u64, StreamToolCallBuffer>,
    tool_calls: Vec<ModelToolCall>,
    completed: bool,
}

impl ResponsesStreamParser {
    pub(crate) fn new() -> Self {
        Self {
            aggregate_text: String::new(),
            tool_call_buffers: BTreeMap::new(),
            tool_calls: Vec::new(),
            completed: false,
        }
    }

    pub(crate) fn parse_sse_line(
        &mut self,
        raw_line: &str,
    ) -> Result<Vec<ModelEvent>, OpenAiProviderError> {
        let line = raw_line.trim_end_matches(['\r', '\n']);
        if line.is_empty() || line.starts_with(':') {
            return Ok(Vec::new());
        }

        let Some((field, value)) = line.split_once(':') else {
            return Err(unexpected_sse_line(line));
        };
        if field != "data" {
            if is_ignorable_sse_field(field) {
                return Ok(Vec::new());
            }
            return Err(unexpected_sse_line(line));
        }

        let data = value.strip_prefix(' ').unwrap_or(value);
        if data.is_empty() {
            return Ok(Vec::new());
        }
        if data == "[DONE]" {
            return Ok(Vec::new());
        }
        if self.completed {
            return Err(OpenAiProviderError::protocol(
                "stream emitted an event after completion",
            ));
        }

        let event: ResponsesStreamEvent = serde_json::from_str(data).map_err(|error| {
            OpenAiProviderError::protocol(format!(
                "failed to parse Responses stream event: {error}"
            ))
        })?;

        self.parse_event(event)
    }

    pub(crate) fn finish(&self) -> Result<(), OpenAiProviderError> {
        if self.completed {
            return Ok(());
        }

        Err(OpenAiProviderError::protocol(
            "Responses stream ended before response.completed",
        ))
    }

    fn parse_event(
        &mut self,
        event: ResponsesStreamEvent,
    ) -> Result<Vec<ModelEvent>, OpenAiProviderError> {
        match event {
            ResponsesStreamEvent::Created | ResponsesStreamEvent::Other => Ok(Vec::new()),
            ResponsesStreamEvent::OutputTextDelta { delta } => {
                if delta.is_empty() {
                    return Ok(Vec::new());
                }

                self.aggregate_text.push_str(&delta);
                Ok(vec![ModelEvent::OutputTextDelta { delta }])
            }
            ResponsesStreamEvent::OutputItemAdded { output_index, item } => {
                self.merge_output_item(output_index, item)?;
                Ok(Vec::new())
            }
            ResponsesStreamEvent::FunctionCallArgumentsDelta {
                output_index,
                delta,
            } => {
                self.tool_call_buffers
                    .entry(output_index)
                    .or_default()
                    .arguments
                    .push_str(&delta);
                Ok(Vec::new())
            }
            ResponsesStreamEvent::FunctionCallArgumentsDone {
                output_index,
                arguments,
            } => {
                self.tool_call_buffers
                    .entry(output_index)
                    .or_default()
                    .set_arguments(arguments)?;
                Ok(Vec::new())
            }
            ResponsesStreamEvent::OutputItemDone { output_index, item } => match item {
                ResponsesStreamOutputItem::Other => Ok(Vec::new()),
                item @ ResponsesStreamOutputItem::FunctionCall { .. } => {
                    self.merge_output_item(output_index, item)?;
                    let buffer = self
                        .tool_call_buffers
                        .remove(&output_index)
                        .ok_or_else(|| {
                            OpenAiProviderError::protocol(
                                "completed function call had no buffered item",
                            )
                        })?;
                    let tool_call = buffer.into_model_tool_call()?;
                    self.tool_calls.push(tool_call.clone());
                    Ok(vec![ModelEvent::ToolCallRequested { call: tool_call }])
                }
            },
            ResponsesStreamEvent::Completed { response } => {
                self.completed = true;
                Ok(vec![ModelEvent::Completed {
                    response: self.completed_response(response)?,
                }])
            }
            ResponsesStreamEvent::Incomplete { response } => {
                Err(OpenAiProviderError::protocol(format!(
                    "Responses stream ended incomplete with status {}",
                    response.status.as_deref().unwrap_or("unknown")
                )))
            }
            ResponsesStreamEvent::Failed { response } => {
                Err(OpenAiProviderError::protocol(format!(
                    "Responses stream failed with status {}",
                    response.status.as_deref().unwrap_or("unknown")
                )))
            }
            ResponsesStreamEvent::Error { code, message } => {
                let code = code.unwrap_or_else(|| "unknown".to_owned());
                let message =
                    message.unwrap_or_else(|| "provider returned stream error".to_owned());
                Err(OpenAiProviderError::protocol(format!(
                    "Responses stream error {code}: {message}"
                )))
            }
        }
    }

    fn merge_output_item(
        &mut self,
        output_index: u64,
        item: ResponsesStreamOutputItem,
    ) -> Result<(), OpenAiProviderError> {
        match item {
            ResponsesStreamOutputItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => self
                .tool_call_buffers
                .entry(output_index)
                .or_default()
                .merge(call_id, name, arguments),
            ResponsesStreamOutputItem::Other => Ok(()),
        }
    }

    fn completed_response(
        &self,
        response: ResponsesResponse,
    ) -> Result<ModelResponse, OpenAiProviderError> {
        let usage = response.usage.map(usage_from_wire).transpose()?;
        if !self.tool_call_buffers.is_empty() {
            return Err(OpenAiProviderError::protocol(
                "Responses stream completed with unfinished function call",
            ));
        }

        Ok(ModelResponse::new(
            stream_outputs(&self.aggregate_text, &self.tool_calls),
            stream_finish_reason(&self.tool_calls),
            usage,
        ))
    }
}

fn is_ignorable_sse_field(field: &str) -> bool {
    matches!(field, "event" | "id" | "retry")
}

fn unexpected_sse_line(line: &str) -> OpenAiProviderError {
    OpenAiProviderError::protocol(format!(
        "unexpected Responses stream line {}; expected an SSE `data:` field",
        compact_stream_line(line)
    ))
}

fn compact_stream_line(line: &str) -> String {
    let mut compact = line
        .chars()
        .filter(|character| !character.is_control())
        .take(120)
        .collect::<String>();
    if line.chars().count() > 120 {
        compact.push_str("...");
    }
    format!("{compact:?}")
}

fn parse_response(response: ResponsesResponse) -> Result<ModelResponse, OpenAiProviderError> {
    let mut outputs = Vec::new();
    for item in response.output {
        match item {
            ResponsesOutputItem::Message { content } => {
                for content in content {
                    if let ResponsesOutputContent::OutputText { text } = content
                        && !text.is_empty()
                    {
                        outputs.push(ModelOutput::text(&text));
                    }
                }
            }
            ResponsesOutputItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => {
                outputs.push(ModelOutput::tool_call(parse_tool_call(
                    call_id, name, arguments,
                )?));
            }
            ResponsesOutputItem::Other => {}
        }
    }

    let has_tool_call = outputs
        .iter()
        .any(|output| matches!(output, ModelOutput::ToolCall { .. }));
    Ok(ModelResponse::new(
        outputs,
        if has_tool_call {
            FinishReason::ToolCalls
        } else {
            parse_response_status(response.status.as_deref())?
        },
        response.usage.map(usage_from_wire).transpose()?,
    ))
}

fn parse_response_status(status: Option<&str>) -> Result<FinishReason, OpenAiProviderError> {
    match status {
        Some("completed") => Ok(FinishReason::Stop),
        Some("incomplete") => Ok(FinishReason::Length),
        Some("failed") => Ok(FinishReason::Error),
        Some(other) => Err(OpenAiProviderError::protocol(format!(
            "unsupported Responses status `{other}`"
        ))),
        None => Ok(FinishReason::Stop),
    }
}

fn stream_finish_reason(tool_calls: &[ModelToolCall]) -> FinishReason {
    if tool_calls.is_empty() {
        FinishReason::Stop
    } else {
        FinishReason::ToolCalls
    }
}

fn usage_from_wire(usage: ResponsesUsage) -> Result<Usage, OpenAiProviderError> {
    let total_tokens = match usage.total_tokens {
        Some(total_tokens) => total_tokens,
        None => usage
            .input_tokens
            .checked_add(usage.output_tokens)
            .ok_or_else(|| OpenAiProviderError::protocol("usage total token count overflowed"))?,
    };

    Ok(Usage::with_details(
        usage.input_tokens,
        usage
            .input_tokens_details
            .and_then(|details| details.cached_tokens),
        usage.output_tokens,
        usage
            .output_tokens_details
            .and_then(|details| details.reasoning_tokens),
        total_tokens,
    ))
}

fn stream_outputs(aggregate_text: &str, tool_calls: &[ModelToolCall]) -> Vec<ModelOutput> {
    let mut outputs = Vec::new();
    if !aggregate_text.is_empty() {
        outputs.push(ModelOutput::text(aggregate_text));
    }
    outputs.extend(tool_calls.iter().cloned().map(ModelOutput::tool_call));
    outputs
}

fn parse_tool_call(
    call_id: Option<String>,
    name: Option<String>,
    arguments: Option<String>,
) -> Result<ModelToolCall, OpenAiProviderError> {
    let call_id = required_field(call_id, "function call id")?;
    let name = required_field(name, "function call name")?;
    let arguments = required_field(arguments, "function call arguments")?;
    let arguments: Value =
        crate::tool_arguments::parse_tool_arguments(&arguments).map_err(|error| {
            OpenAiProviderError::invalid_tool_call(format!(
                "function call arguments must be valid JSON: {error}"
            ))
        })?;
    let arguments = ToolArguments::try_from(arguments).map_err(|error| {
        OpenAiProviderError::invalid_tool_call(format!(
            "function call arguments must be a JSON object: {error}"
        ))
    })?;
    let call_id = ModelToolCallId::new(&call_id).map_err(|error| {
        OpenAiProviderError::protocol(format!("function call id is invalid: {error}"))
    })?;
    let name = ToolName::new(&name).map_err(|error| {
        OpenAiProviderError::protocol(format!("function call name is invalid: {error}"))
    })?;

    Ok(ModelToolCall::new(call_id, name, arguments))
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
    call_id: Option<String>,
    name: Option<String>,
    arguments: String,
}

impl StreamToolCallBuffer {
    fn merge(
        &mut self,
        call_id: Option<String>,
        name: Option<String>,
        arguments: Option<String>,
    ) -> Result<(), OpenAiProviderError> {
        merge_optional_field(&mut self.call_id, call_id, "streamed function call id")?;
        merge_optional_field(&mut self.name, name, "streamed function call name")?;
        if let Some(arguments) = arguments {
            self.set_arguments(arguments)?;
        }

        Ok(())
    }

    fn set_arguments(&mut self, arguments: String) -> Result<(), OpenAiProviderError> {
        if self.arguments.is_empty() || self.arguments == arguments {
            self.arguments = arguments;
            Ok(())
        } else {
            Err(OpenAiProviderError::protocol(
                "streamed function call arguments changed across stream events",
            ))
        }
    }

    fn into_model_tool_call(self) -> Result<ModelToolCall, OpenAiProviderError> {
        parse_tool_call(
            Some(required_field(self.call_id, "streamed function call id")?),
            Some(required_field(self.name, "streamed function call name")?),
            Some(self.arguments),
        )
    }
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
                "{field} changed across stream events"
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
