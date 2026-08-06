use super::wire::{ChatChunk, ChatUsage};
use crate::OpenAiProviderError;
use merry_core::ToolName;
use merry_llm::{
    FinishReason, ModelEvent, ModelOutput, ModelResponse, ModelToolCall, ModelToolCallId,
    ToolArguments, Usage,
};
use std::collections::BTreeMap;

pub(crate) struct ChatStreamParser {
    aggregate_text: String,
    tool_buffers: BTreeMap<u64, ChatToolBuffer>,
    tool_calls: Vec<ModelToolCall>,
    finish_reason: Option<FinishReason>,
    usage: Option<Usage>,
    completed: bool,
}

impl ChatStreamParser {
    pub(crate) fn new() -> Self {
        Self {
            aggregate_text: String::new(),
            tool_buffers: BTreeMap::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
            usage: None,
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
            if matches!(field, "event" | "id" | "retry") {
                return Ok(Vec::new());
            }
            return Err(unexpected_sse_line(line));
        }
        let data = value.strip_prefix(' ').unwrap_or(value);
        if data.is_empty() {
            return Ok(Vec::new());
        }
        if self.completed {
            return Err(OpenAiProviderError::protocol(
                "Chat Completions stream emitted data after [DONE]",
            ));
        }
        if data == "[DONE]" {
            let response = self.completed_response()?;
            self.completed = true;
            return Ok(vec![ModelEvent::Completed { response }]);
        }

        let chunk: ChatChunk = serde_json::from_str(data).map_err(|error| {
            OpenAiProviderError::protocol(format!(
                "failed to parse Chat Completions stream chunk: {error}"
            ))
        })?;
        self.parse_chunk(chunk)
    }

    pub(crate) fn finish(&self) -> Result<(), OpenAiProviderError> {
        if self.completed {
            Ok(())
        } else {
            Err(OpenAiProviderError::protocol(
                "Chat Completions stream ended before [DONE]",
            ))
        }
    }

    fn parse_chunk(&mut self, chunk: ChatChunk) -> Result<Vec<ModelEvent>, OpenAiProviderError> {
        if chunk.choices.len() > 1 {
            return Err(OpenAiProviderError::protocol(
                "Chat Completions stream returned multiple choices",
            ));
        }
        if let Some(usage) = chunk.usage {
            self.usage = Some(usage_from_wire(usage)?);
        }
        let Some(choice) = chunk.choices.into_iter().next() else {
            return Ok(Vec::new());
        };
        if choice.index != 0 {
            return Err(OpenAiProviderError::protocol(format!(
                "Chat Completions stream returned unsupported choice index {}",
                choice.index
            )));
        }

        let mut events = Vec::new();
        if let Some(content) = choice.delta.content
            && !content.is_empty()
        {
            self.aggregate_text.push_str(&content);
            events.push(ModelEvent::OutputTextDelta { delta: content });
        }
        for delta in choice.delta.tool_calls {
            self.tool_buffers
                .entry(delta.index)
                .or_default()
                .merge(delta.id, delta.function)?;
        }

        if let Some(reason) = choice.finish_reason {
            if self.finish_reason.is_some() {
                return Err(OpenAiProviderError::protocol(
                    "Chat Completions stream returned more than one finish reason",
                ));
            }
            let reason = parse_finish_reason(&reason)?;
            if reason == FinishReason::ToolCalls {
                for (_, buffer) in std::mem::take(&mut self.tool_buffers) {
                    let call = buffer.into_model_tool_call()?;
                    events.push(ModelEvent::ToolCallRequested { call: call.clone() });
                    self.tool_calls.push(call);
                }
                if self.tool_calls.is_empty() {
                    return Err(OpenAiProviderError::protocol(
                        "Chat Completions finished with tool_calls but emitted no calls",
                    ));
                }
            } else if !self.tool_buffers.is_empty() {
                return Err(OpenAiProviderError::protocol(
                    "Chat Completions finished without tool_calls but left tool deltas pending",
                ));
            }
            self.finish_reason = Some(reason);
        }

        Ok(events)
    }

    fn completed_response(&self) -> Result<ModelResponse, OpenAiProviderError> {
        let finish_reason = self.finish_reason.ok_or_else(|| {
            OpenAiProviderError::protocol("Chat Completions [DONE] arrived before a finish reason")
        })?;
        if !self.tool_buffers.is_empty() {
            return Err(OpenAiProviderError::protocol(
                "Chat Completions [DONE] arrived with unfinished tool calls",
            ));
        }

        let mut outputs = Vec::new();
        if !self.aggregate_text.is_empty() {
            outputs.push(ModelOutput::text(&self.aggregate_text));
        }
        outputs.extend(self.tool_calls.iter().cloned().map(ModelOutput::tool_call));
        Ok(ModelResponse::new(outputs, finish_reason, self.usage))
    }
}

#[derive(Default)]
struct ChatToolBuffer {
    id: Option<String>,
    name: String,
    arguments: String,
}

impl ChatToolBuffer {
    fn merge(
        &mut self,
        id: Option<String>,
        function: Option<super::wire::ChatToolCallFunctionDelta>,
    ) -> Result<(), OpenAiProviderError> {
        if let Some(id) = id {
            match &self.id {
                Some(existing) if existing != &id => {
                    return Err(OpenAiProviderError::protocol(
                        "Chat Completions changed a tool call id mid-stream",
                    ));
                }
                Some(_) => {}
                None => self.id = Some(id),
            }
        }
        if let Some(function) = function {
            if let Some(name) = function.name {
                self.name.push_str(&name);
            }
            if let Some(arguments) = function.arguments {
                self.arguments.push_str(&arguments);
            }
        }
        Ok(())
    }

    fn into_model_tool_call(self) -> Result<ModelToolCall, OpenAiProviderError> {
        let id = self.id.ok_or_else(|| {
            OpenAiProviderError::protocol("Chat Completions tool call is missing id")
        })?;
        if self.name.is_empty() {
            return Err(OpenAiProviderError::protocol(
                "Chat Completions tool call is missing function name",
            ));
        }
        let raw_arguments = if self.arguments.is_empty() {
            "{}"
        } else {
            &self.arguments
        };
        let arguments =
            serde_json::from_str::<serde_json::Value>(raw_arguments).map_err(|error| {
                OpenAiProviderError::invalid_tool_call(format!(
                    "Chat Completions tool arguments are not valid JSON: {error}"
                ))
            })?;
        Ok(ModelToolCall::new(
            ModelToolCallId::new(&id).map_err(|error| {
                OpenAiProviderError::protocol(format!(
                    "Chat Completions tool call id is invalid: {error}"
                ))
            })?,
            ToolName::new(&self.name).map_err(|error| {
                OpenAiProviderError::protocol(format!(
                    "Chat Completions tool name is invalid: {error}"
                ))
            })?,
            ToolArguments::try_from(arguments).map_err(|error| {
                OpenAiProviderError::invalid_tool_call(format!(
                    "Chat Completions tool arguments are invalid: {error}"
                ))
            })?,
        ))
    }
}

fn parse_finish_reason(reason: &str) -> Result<FinishReason, OpenAiProviderError> {
    match reason {
        "stop" => Ok(FinishReason::Stop),
        "tool_calls" => Ok(FinishReason::ToolCalls),
        "length" => Ok(FinishReason::Length),
        "content_filter" => Ok(FinishReason::Blocked),
        other => Err(OpenAiProviderError::protocol(format!(
            "unsupported Chat Completions finish reason `{other}`"
        ))),
    }
}

fn usage_from_wire(usage: ChatUsage) -> Result<Usage, OpenAiProviderError> {
    let total = usage.total_tokens.unwrap_or(
        usage
            .prompt_tokens
            .checked_add(usage.completion_tokens)
            .ok_or_else(|| OpenAiProviderError::protocol("usage total token count overflowed"))?,
    );
    Ok(Usage::with_details(
        usage.prompt_tokens,
        usage
            .prompt_tokens_details
            .and_then(|details| details.cached_tokens),
        usage.completion_tokens,
        usage
            .completion_tokens_details
            .and_then(|details| details.reasoning_tokens),
        total,
    ))
}

fn unexpected_sse_line(line: &str) -> OpenAiProviderError {
    let compact = line
        .chars()
        .filter(|character| !character.is_control())
        .take(120)
        .collect::<String>();
    OpenAiProviderError::protocol(format!(
        "unexpected Chat Completions stream line {compact:?}; expected an SSE `data:` field"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_interleaved_tool_calls_and_usage_in_call_order() {
        let mut parser = ChatStreamParser::new();
        let lines = [
            r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"call-2","function":{"name":"search","arguments":"{\"query\":\"b\"}"}},{"index":0,"id":"call-1","function":{"name":"search","arguments":"{\"query\":\"a\"}"}}]},"finish_reason":null}],"usage":null}"#,
            r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],"usage":null}"#,
            r#"data: {"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#,
            "data: [DONE]",
        ];
        let mut events = Vec::new();
        for line in lines {
            events.extend(parser.parse_sse_line(line).expect("line should parse"));
        }
        parser.finish().expect("stream should finish");

        let ids = events
            .iter()
            .filter_map(|event| match event {
                ModelEvent::ToolCallRequested { call } => Some(call.id().as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(ids, ["call-1", "call-2"]);
        let completed = events
            .iter()
            .find_map(|event| match event {
                ModelEvent::Completed { response } => Some(response),
                _ => None,
            })
            .expect("completion should be emitted");
        assert_eq!(completed.finish_reason(), FinishReason::ToolCalls);
        assert_eq!(completed.usage().expect("usage").total_tokens, 15);
    }

    #[test]
    fn parses_tool_call_without_argument_fragments_as_empty_object() {
        let mut parser = ChatStreamParser::new();
        let lines = [
            r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-empty","function":{"name":"list_items"}}]},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            "data: [DONE]",
        ];
        let mut events = Vec::new();
        for line in lines {
            events.extend(parser.parse_sse_line(line).expect("line should parse"));
        }

        let call = events.iter().find_map(|event| match event {
            ModelEvent::ToolCallRequested { call } => Some(call),
            _ => None,
        });
        assert!(
            call.expect("tool call should be emitted")
                .arguments()
                .as_object()
                .is_empty()
        );
        parser.finish().expect("stream should finish");
    }
}
