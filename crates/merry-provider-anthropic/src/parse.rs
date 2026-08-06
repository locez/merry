use crate::{
    AnthropicProviderError,
    wire::{
        AnthropicContentBlockDelta, AnthropicContentBlockStart, AnthropicStreamEvent,
        AnthropicUsage,
    },
};
use merry_core::ToolName;
use merry_llm::{
    FinishReason, ModelEvent, ModelOutput, ModelResponse, ModelToolCall, ModelToolCallId,
    ToolArguments, Usage,
};
use std::collections::BTreeMap;

pub(crate) struct AnthropicStreamParser {
    aggregate_text: String,
    tool_buffers: BTreeMap<u64, AnthropicToolBuffer>,
    tool_calls: BTreeMap<u64, ModelToolCall>,
    last_streamed_tool_index: Option<u64>,
    finish_reason: Option<FinishReason>,
    usage: UsageAccumulator,
    completed: bool,
}

impl AnthropicStreamParser {
    pub(crate) fn new() -> Self {
        Self {
            aggregate_text: String::new(),
            tool_buffers: BTreeMap::new(),
            tool_calls: BTreeMap::new(),
            last_streamed_tool_index: None,
            finish_reason: None,
            usage: UsageAccumulator::default(),
            completed: false,
        }
    }

    pub(crate) fn parse_sse_line(
        &mut self,
        raw_line: &str,
    ) -> Result<Vec<ModelEvent>, AnthropicProviderError> {
        let line = raw_line.trim_end_matches(['\r', '\n']);
        if line.is_empty() || line.starts_with(':') {
            return Ok(Vec::new());
        }
        let Some((field, value)) = line.split_once(':') else {
            return Err(unexpected_sse_line(line));
        };
        if field != "data" {
            if matches!(field, "event" | "id" | "retry") {
                return Ok(Vec::new());
            }
            return Err(unexpected_sse_line(line));
        }
        if self.completed {
            return Err(AnthropicProviderError::protocol(
                "Anthropic stream emitted data after message_stop",
            ));
        }
        let data = value.strip_prefix(' ').unwrap_or(value);
        if data.is_empty() {
            return Ok(Vec::new());
        }
        let event: AnthropicStreamEvent = serde_json::from_str(data).map_err(|error| {
            AnthropicProviderError::protocol(format!(
                "failed to parse Anthropic stream event: {error}"
            ))
        })?;
        self.parse_event(event)
    }

    pub(crate) fn finish(&self) -> Result<(), AnthropicProviderError> {
        if self.completed {
            Ok(())
        } else {
            Err(AnthropicProviderError::protocol(
                "Anthropic stream ended before message_stop",
            ))
        }
    }

    fn parse_event(
        &mut self,
        event: AnthropicStreamEvent,
    ) -> Result<Vec<ModelEvent>, AnthropicProviderError> {
        match event {
            AnthropicStreamEvent::MessageStart { message } => {
                if let Some(usage) = message.usage {
                    self.usage.apply(usage);
                }
                Ok(Vec::new())
            }
            AnthropicStreamEvent::ContentBlockStart {
                index,
                content_block,
            } => match content_block {
                AnthropicContentBlockStart::Text { text } if !text.is_empty() => {
                    self.aggregate_text.push_str(&text);
                    Ok(vec![ModelEvent::OutputTextDelta { delta: text }])
                }
                AnthropicContentBlockStart::Text { .. } | AnthropicContentBlockStart::Other => {
                    Ok(Vec::new())
                }
                AnthropicContentBlockStart::ToolUse { id, name, input } => {
                    let initial_input = if input.as_object().is_some_and(|object| object.is_empty())
                    {
                        String::new()
                    } else {
                        serde_json::to_string(&input).map_err(|error| {
                            AnthropicProviderError::protocol(format!(
                                "failed to serialize initial Anthropic tool input: {error}"
                            ))
                        })?
                    };
                    if self
                        .tool_buffers
                        .insert(
                            index,
                            AnthropicToolBuffer {
                                id,
                                name,
                                input: initial_input,
                            },
                        )
                        .is_some()
                    {
                        return Err(AnthropicProviderError::protocol(
                            "Anthropic stream repeated a tool content block index",
                        ));
                    }
                    Ok(Vec::new())
                }
            },
            AnthropicStreamEvent::ContentBlockDelta { index, delta } => match delta {
                AnthropicContentBlockDelta::TextDelta { text } if !text.is_empty() => {
                    self.aggregate_text.push_str(&text);
                    Ok(vec![ModelEvent::OutputTextDelta { delta: text }])
                }
                AnthropicContentBlockDelta::TextDelta { .. }
                | AnthropicContentBlockDelta::Other => Ok(Vec::new()),
                AnthropicContentBlockDelta::InputJsonDelta { partial_json } => {
                    self.tool_buffers
                        .get_mut(&index)
                        .ok_or_else(|| {
                            AnthropicProviderError::protocol(
                                "Anthropic tool input delta referenced an unknown content block",
                            )
                        })?
                        .input
                        .push_str(&partial_json);
                    Ok(Vec::new())
                }
            },
            AnthropicStreamEvent::ContentBlockStop { index } => {
                let Some(buffer) = self.tool_buffers.remove(&index) else {
                    return Ok(Vec::new());
                };
                if self
                    .last_streamed_tool_index
                    .is_some_and(|previous| index <= previous)
                {
                    return Err(AnthropicProviderError::protocol(
                        "Anthropic tool content blocks completed out of order",
                    ));
                }
                self.last_streamed_tool_index = Some(index);
                let call = buffer.into_model_tool_call()?;
                self.tool_calls.insert(index, call.clone());
                Ok(vec![ModelEvent::ToolCallRequested { call }])
            }
            AnthropicStreamEvent::MessageDelta { delta, usage } => {
                if let Some(usage) = usage {
                    self.usage.apply(usage);
                }
                if let Some(reason) = delta.stop_reason {
                    self.finish_reason = Some(parse_stop_reason(&reason)?);
                }
                Ok(Vec::new())
            }
            AnthropicStreamEvent::MessageStop => {
                if !self.tool_buffers.is_empty() {
                    return Err(AnthropicProviderError::protocol(
                        "Anthropic message stopped with unfinished tool input",
                    ));
                }
                let finish_reason = self.finish_reason.ok_or_else(|| {
                    AnthropicProviderError::protocol(
                        "Anthropic message_stop arrived before a stop reason",
                    )
                })?;
                let mut outputs = Vec::new();
                if !self.aggregate_text.is_empty() {
                    outputs.push(ModelOutput::text(&self.aggregate_text));
                }
                outputs.extend(
                    self.tool_calls
                        .values()
                        .cloned()
                        .map(ModelOutput::tool_call),
                );
                self.completed = true;
                Ok(vec![ModelEvent::Completed {
                    response: ModelResponse::new(
                        outputs,
                        finish_reason,
                        Some(self.usage.finish()?),
                    ),
                }])
            }
            AnthropicStreamEvent::Error { error } => {
                Err(AnthropicProviderError::protocol(format!(
                    "Anthropic stream returned error type {}",
                    error.kind.as_deref().unwrap_or("unknown")
                )))
            }
            AnthropicStreamEvent::Ping | AnthropicStreamEvent::Other => Ok(Vec::new()),
        }
    }
}

struct AnthropicToolBuffer {
    id: String,
    name: String,
    input: String,
}

impl AnthropicToolBuffer {
    fn into_model_tool_call(self) -> Result<ModelToolCall, AnthropicProviderError> {
        let raw_input = if self.input.is_empty() {
            "{}"
        } else {
            &self.input
        };
        let input = serde_json::from_str::<serde_json::Value>(raw_input).map_err(|error| {
            AnthropicProviderError::invalid_tool_call(format!(
                "Anthropic tool input is not valid JSON: {error}"
            ))
        })?;
        Ok(ModelToolCall::new(
            ModelToolCallId::new(&self.id).map_err(|error| {
                AnthropicProviderError::protocol(format!(
                    "Anthropic tool call id is invalid: {error}"
                ))
            })?,
            ToolName::new(&self.name).map_err(|error| {
                AnthropicProviderError::protocol(format!("Anthropic tool name is invalid: {error}"))
            })?,
            ToolArguments::try_from(input).map_err(|error| {
                AnthropicProviderError::invalid_tool_call(format!(
                    "Anthropic tool input must be an object: {error}"
                ))
            })?,
        ))
    }
}

#[derive(Default)]
struct UsageAccumulator {
    input_tokens: u64,
    cache_creation_input_tokens: u64,
    cache_read_input_tokens: u64,
    output_tokens: u64,
}

impl UsageAccumulator {
    fn apply(&mut self, usage: AnthropicUsage) {
        if let Some(value) = usage.input_tokens {
            self.input_tokens = value;
        }
        if let Some(value) = usage.cache_creation_input_tokens {
            self.cache_creation_input_tokens = value;
        }
        if let Some(value) = usage.cache_read_input_tokens {
            self.cache_read_input_tokens = value;
        }
        if let Some(value) = usage.output_tokens {
            self.output_tokens = value;
        }
    }

    fn finish(&self) -> Result<Usage, AnthropicProviderError> {
        let input_tokens = self
            .input_tokens
            .checked_add(self.cache_creation_input_tokens)
            .and_then(|total| total.checked_add(self.cache_read_input_tokens))
            .ok_or_else(|| AnthropicProviderError::protocol("input token count overflowed"))?;
        let total_tokens = input_tokens
            .checked_add(self.output_tokens)
            .ok_or_else(|| AnthropicProviderError::protocol("total token count overflowed"))?;
        Ok(Usage::with_details(
            input_tokens,
            (self.cache_read_input_tokens > 0).then_some(self.cache_read_input_tokens),
            self.output_tokens,
            None,
            total_tokens,
        ))
    }
}

fn parse_stop_reason(reason: &str) -> Result<FinishReason, AnthropicProviderError> {
    match reason {
        "end_turn" | "stop_sequence" => Ok(FinishReason::Stop),
        "tool_use" => Ok(FinishReason::ToolCalls),
        "max_tokens" | "model_context_window_exceeded" => Ok(FinishReason::Length),
        "refusal" => Ok(FinishReason::Blocked),
        "pause_turn" => Ok(FinishReason::Error),
        other => Err(AnthropicProviderError::protocol(format!(
            "unsupported Anthropic stop reason `{other}`"
        ))),
    }
}

fn unexpected_sse_line(line: &str) -> AnthropicProviderError {
    let compact = line
        .chars()
        .filter(|character| !character.is_control())
        .take(120)
        .collect::<String>();
    AnthropicProviderError::protocol(format!(
        "unexpected Anthropic stream line {compact:?}; expected an SSE `data:` field"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use merry_llm::{ModelError, ProviderErrorKind};

    #[test]
    fn parses_text_tool_use_partial_json_usage_and_completion() {
        let lines = [
            r#"data: {"type":"message_start","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":3,"output_tokens":0}}}"#,
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Checking"}}"#,
            r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"search","input":{}}}"#,
            r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"query\":\"notes\"}"}}"#,
            r#"data: {"type":"content_block_stop","index":1}"#,
            r#"data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}"#,
            r#"data: {"type":"message_stop"}"#,
        ];
        let mut parser = AnthropicStreamParser::new();
        let mut events = Vec::new();
        for line in lines {
            events.extend(parser.parse_sse_line(line).expect("line should parse"));
        }
        parser.finish().expect("stream should complete");
        assert!(matches!(events[0], ModelEvent::OutputTextDelta { .. }));
        assert!(matches!(events[1], ModelEvent::ToolCallRequested { .. }));
        let completed = match &events[2] {
            ModelEvent::Completed { response } => response,
            _ => panic!("completion expected"),
        };
        assert_eq!(completed.finish_reason(), FinishReason::ToolCalls);
        assert_eq!(completed.usage().expect("usage").input_tokens, 13);
        assert_eq!(completed.usage().expect("usage").total_tokens, 18);
    }

    #[test]
    fn parses_tool_use_with_empty_input_object() {
        let lines = [
            r#"data: {"type":"message_start","message":{"usage":{"input_tokens":1,"output_tokens":0}}}"#,
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_empty","name":"list_items","input":{}}}"#,
            r#"data: {"type":"content_block_stop","index":0}"#,
            r#"data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}}"#,
            r#"data: {"type":"message_stop"}"#,
        ];
        let mut parser = AnthropicStreamParser::new();
        let mut events = Vec::new();
        for line in lines {
            events.extend(parser.parse_sse_line(line).expect("line should parse"));
        }

        let call = events.iter().find_map(|event| match event {
            ModelEvent::ToolCallRequested { call } => Some(call),
            _ => None,
        });
        let call = call.expect("tool call should be emitted");
        assert!(call.arguments().as_object().is_empty());
        parser.finish().expect("stream should complete");
    }

    #[test]
    fn malformed_tool_input_is_classified_as_invalid_tool_call() {
        let error = AnthropicToolBuffer {
            id: "toolu_bad".to_owned(),
            name: "search".to_owned(),
            input: "[".to_owned(),
        }
        .into_model_tool_call()
        .expect_err("malformed tool input should fail");

        assert_eq!(
            ModelError::from(error).kind(),
            ProviderErrorKind::InvalidToolCall
        );
    }
}
