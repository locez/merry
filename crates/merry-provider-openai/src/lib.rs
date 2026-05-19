//! OpenAI-compatible provider adapter for Merry.

mod config;
mod error;
mod parse;
mod provider;
mod render;
mod wire;

pub use config::OpenAiProviderConfig;
pub use error::OpenAiProviderError;
pub use provider::OpenAiProvider;

#[cfg(test)]
mod tests {
    use super::*;
    use merry_core::{ToolCallResultStatus, ToolInputSchema, ToolName, ToolSpec};
    use merry_llm::{
        FinishReason, GenerationConfig, ModelContent, ModelEvent, ModelMessage, ModelMessageRole,
        ModelName, ModelOutput, ModelRequest, ModelToolCall, ModelToolCallId,
        ModelToolContinuation, ModelToolResult, ModelToolResultContent, ProviderErrorKind,
        ToolArguments, Usage,
    };
    use serde_json::{Value, json};

    fn object_schema() -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "city": { "type": "string" }
            },
            "required": ["city"]
        }))
        .expect("object schema should be valid")
    }

    fn weather_tool() -> ToolSpec {
        ToolSpec::new(
            ToolName::new("lookup_weather").expect("valid tool name"),
            "Look up weather for a city",
            object_schema(),
        )
        .expect("valid tool spec")
    }

    fn message(role: ModelMessageRole, text: &str) -> ModelMessage {
        ModelMessage::new(role, ModelContent::text(text).expect("valid text content"))
            .expect("valid message")
    }

    fn request_with_tools() -> ModelRequest {
        ModelRequest::new(
            ModelName::new("gpt-4.1-mini").expect("valid model name"),
            vec![
                message(ModelMessageRole::System, "You are concise."),
                message(ModelMessageRole::User, "Weather in Shanghai?"),
            ],
            vec![weather_tool()],
            GenerationConfig::new(Some(256), false).expect("valid generation config"),
        )
        .expect("valid model request")
    }

    fn request_without_tools() -> ModelRequest {
        ModelRequest::new(
            ModelName::new("gpt-4.1-mini").expect("valid model name"),
            vec![message(ModelMessageRole::User, "Hello")],
            Vec::new(),
            GenerationConfig::default(),
        )
        .expect("valid model request")
    }

    fn request_with_parallel_tool_calls() -> ModelRequest {
        ModelRequest::new(
            ModelName::new("gpt-4.1-mini").expect("valid model name"),
            vec![message(ModelMessageRole::User, "Weather in Shanghai?")],
            vec![weather_tool()],
            GenerationConfig::new(Some(256), true).expect("valid generation config"),
        )
        .expect("valid model request")
    }

    fn tool_continuation_with_json_result() -> ModelToolContinuation {
        let call = ModelToolCall::new(
            ModelToolCallId::new("call_abc123").expect("valid call id"),
            ToolName::new("lookup_weather").expect("valid tool name"),
            ToolArguments::try_from(json!({
                "city": "Shanghai",
                "units": "metric"
            }))
            .expect("valid tool arguments"),
        );
        let result = ModelToolResult::new(
            call.id().clone(),
            ToolCallResultStatus::Succeeded,
            ModelToolResultContent::json(r#"{"temperature_c":22,"condition":"clear"}"#)
                .expect("valid JSON result"),
            None,
        )
        .expect("valid tool result");

        ModelToolContinuation::new(call, result).expect("valid continuation")
    }

    fn request_with_tool_continuation() -> ModelRequest {
        ModelRequest::new_with_continuations(
            ModelName::new("gpt-4.1-mini").expect("valid model name"),
            vec![
                message(ModelMessageRole::System, "You are concise."),
                message(ModelMessageRole::User, "Continue after tool result."),
            ],
            vec![weather_tool()],
            vec![tool_continuation_with_json_result()],
            GenerationConfig::new(Some(256), false).expect("valid generation config"),
        )
        .expect("valid model request")
    }

    #[test]
    fn config_validates_defaults_and_rejects_invalid_values() {
        let config = OpenAiProviderConfig::new("sk-test").expect("valid config");

        assert_eq!(config.api_key_redacted(), "sk-...test");
        assert_eq!(config.base_url(), "https://api.openai.com/v1");
        assert_eq!(config.provider_name().as_str(), "openai-compatible");
        assert!(config.capabilities().supports_streaming());
        assert!(config.capabilities().supports_tool_calls());
        assert!(!config.capabilities().supports_parallel_tool_calls());
        assert!(config.capabilities().supports_usage_reporting());
        assert_eq!(config.capabilities().max_input_tokens(), None);
        assert_eq!(config.capabilities().max_output_tokens(), None);

        let debug = format!("{config:?}");
        assert!(!debug.contains("sk-test"));
        assert!(debug.contains("<redacted>"));

        for invalid_key in ["", "   ", " sk-test", "sk-test ", "sk-\ntest"] {
            assert!(
                OpenAiProviderConfig::new(invalid_key).is_err(),
                "{invalid_key:?} should reject"
            );
        }

        assert!(
            OpenAiProviderConfig::new("sk-test")
                .and_then(|config| config.with_base_url(""))
                .is_err()
        );
        assert!(
            OpenAiProviderConfig::new("sk-test")
                .and_then(|config| config.with_base_url(" https://api.example.test/v1"))
                .is_err()
        );
        assert!(
            OpenAiProviderConfig::new("sk-test")
                .and_then(|config| config.with_base_url("https://api.example.test/v1 "))
                .is_err()
        );
        assert!(
            OpenAiProviderConfig::new("sk-test")
                .and_then(|config| config.with_base_url("https://api.example.test\n/v1"))
                .is_err()
        );
        assert!(
            OpenAiProviderConfig::new("sk-test")
                .and_then(|config| config.with_base_url("ftp://api.example.test/v1"))
                .is_err()
        );
        assert!(
            OpenAiProviderConfig::new("sk-test")
                .and_then(|config| config.with_base_url("https://"))
                .is_err()
        );
        assert!(
            OpenAiProviderConfig::new("sk-test")
                .and_then(|config| config.with_base_url("https:///v1"))
                .is_err()
        );
        assert!(
            OpenAiProviderConfig::new("sk-test")
                .and_then(|config| config.with_organization("   "))
                .is_err()
        );
        assert!(
            OpenAiProviderConfig::new("sk-test")
                .and_then(|config| config.with_organization(" org"))
                .is_err()
        );
        assert!(
            OpenAiProviderConfig::new("sk-test")
                .and_then(|config| config.with_organization("org\nid"))
                .is_err()
        );
        assert!(
            OpenAiProviderConfig::new("sk-test")
                .and_then(|config| config.with_project(""))
                .is_err()
        );
        assert!(
            OpenAiProviderConfig::new("sk-test")
                .and_then(|config| config.with_project("proj "))
                .is_err()
        );
        assert!(
            OpenAiProviderConfig::new("sk-test")
                .and_then(|config| config.with_project("proj\tid"))
                .is_err()
        );
        assert!(
            OpenAiProviderConfig::new("sk-test")
                .and_then(|config| config.with_provider_name(" has-space"))
                .is_err()
        );
    }

    #[test]
    fn rendered_request_with_tool_matches_chat_completions_json_without_runtime_state() {
        let rendered = crate::render::render_chat_completion_request(&request_with_tools())
            .expect("request should render");

        assert_eq!(
            rendered,
            json!({
                "model": "gpt-4.1-mini",
                "messages": [
                    { "role": "system", "content": "You are concise." },
                    { "role": "user", "content": "Weather in Shanghai?" }
                ],
                "stream": true,
                "stream_options": { "include_usage": true },
                "parallel_tool_calls": false,
                "max_completion_tokens": 256,
                "tools": [
                    {
                        "type": "function",
                        "function": {
                            "name": "lookup_weather",
                            "description": "Look up weather for a city",
                            "parameters": {
                                "type": "object",
                                "properties": {
                                    "city": { "type": "string" }
                                },
                                "required": ["city"]
                            }
                        }
                    }
                ],
                "tool_choice": "auto"
            })
        );

        let runtime_state_fields = [
            "previous_response_id",
            "store",
            "thread_id",
            "session_id",
            "ledger_id",
            "conversation",
        ];
        let object = rendered
            .as_object()
            .expect("request JSON should be an object");
        for field in runtime_state_fields {
            assert!(!object.contains_key(field), "{field} should be omitted");
        }
    }

    #[test]
    fn rendered_request_without_tools_omits_tools_and_tool_choice() {
        let rendered = crate::render::render_chat_completion_request(&request_without_tools())
            .expect("request should render");
        let object = rendered
            .as_object()
            .expect("request JSON should be an object");

        assert_eq!(object.get("parallel_tool_calls"), Some(&json!(false)));
        assert!(!object.contains_key("tools"));
        assert!(!object.contains_key("tool_choice"));
        assert!(!object.contains_key("max_completion_tokens"));
    }

    #[test]
    fn rendered_request_tool_continuation_matches_chat_json_without_state() {
        let rendered =
            crate::render::render_chat_completion_request(&request_with_tool_continuation())
                .expect("request should render");

        assert_eq!(
            rendered,
            json!({
                "model": "gpt-4.1-mini",
                "messages": [
                    { "role": "system", "content": "You are concise." },
                    { "role": "user", "content": "Continue after tool result." },
                    {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [
                            {
                                "id": "call_abc123",
                                "type": "function",
                                "function": {
                                    "name": "lookup_weather",
                                    "arguments": "{\"city\":\"Shanghai\",\"units\":\"metric\"}"
                                }
                            }
                        ]
                    },
                    {
                        "role": "tool",
                        "tool_call_id": "call_abc123",
                        "content": "{\"temperature_c\":22,\"condition\":\"clear\"}"
                    }
                ],
                "stream": true,
                "stream_options": { "include_usage": true },
                "parallel_tool_calls": false,
                "max_completion_tokens": 256,
                "tools": [
                    {
                        "type": "function",
                        "function": {
                            "name": "lookup_weather",
                            "description": "Look up weather for a city",
                            "parameters": {
                                "type": "object",
                                "properties": {
                                    "city": { "type": "string" }
                                },
                                "required": ["city"]
                            }
                        }
                    }
                ],
                "tool_choice": "auto"
            })
        );

        let object = rendered
            .as_object()
            .expect("request JSON should be an object");
        for field in [
            "previous_response_id",
            "store",
            "thread_id",
            "session_id",
            "ledger_id",
            "conversation",
        ] {
            assert!(!object.contains_key(field), "{field} should be omitted");
        }
    }

    #[test]
    fn rendered_request_rejects_parallel_tool_calls() {
        let error =
            crate::render::render_chat_completion_request(&request_with_parallel_tool_calls())
                .expect_err("parallel tool calls should be rejected");

        assert!(matches!(error, OpenAiProviderError::InvalidRequest { .. }));
    }

    #[test]
    fn parse_text_completion_fixture_to_model_response() {
        let fixture = include_str!("../tests/fixtures/chat_completion_text.json");
        let response =
            crate::parse::parse_chat_completion_response(fixture).expect("fixture should parse");

        assert_eq!(response.finish_reason(), FinishReason::Stop);
        assert_eq!(response.usage(), Some(Usage::new(12, 5)));
        assert_eq!(
            response.outputs(),
            &[ModelOutput::text("Hello from the assistant.")]
        );
    }

    #[test]
    fn parse_tool_call_completion_fixture_to_model_response() {
        let fixture = include_str!("../tests/fixtures/chat_completion_tool_call.json");
        let response =
            crate::parse::parse_chat_completion_response(fixture).expect("fixture should parse");

        assert_eq!(response.finish_reason(), FinishReason::ToolCalls);
        assert_eq!(response.usage(), Some(Usage::new(20, 8)));
        assert_eq!(response.outputs().len(), 1);

        match &response.outputs()[0] {
            ModelOutput::ToolCall { call } => {
                assert_eq!(call.id().as_str(), "call_abc123");
                assert_eq!(call.name().as_str(), "lookup_weather");
                assert_eq!(
                    call.arguments().as_object().get("city"),
                    Some(&Value::String("Shanghai".to_owned()))
                );
            }
            output => panic!("expected tool call output, got {output:?}"),
        }
    }

    #[test]
    fn malformed_tool_arguments_fixture_returns_protocol_error() {
        let fixture = include_str!("../tests/fixtures/chat_completion_bad_tool_args.json");
        let error = crate::parse::parse_chat_completion_response(fixture)
            .expect_err("malformed tool arguments should fail");

        assert_eq!(error.kind(), ProviderErrorKind::Protocol);
    }

    #[test]
    fn parse_streaming_text_jsonl_fixture_to_normalized_events() {
        let fixture = include_str!("../tests/fixtures/chat_completion_stream_text.jsonl");
        let events = crate::parse::parse_chat_completion_stream_events(fixture)
            .expect("stream should parse");

        assert_eq!(events.len(), 4);
        assert_eq!(events[0], ModelEvent::Started);
        assert_eq!(
            events[1],
            ModelEvent::OutputTextDelta {
                delta: "Hello".to_owned()
            }
        );
        assert_eq!(
            events[2],
            ModelEvent::OutputTextDelta {
                delta: " world".to_owned()
            }
        );
        assert_eq!(
            events[3],
            ModelEvent::Completed {
                response: merry_llm::ModelResponse::new(
                    vec![ModelOutput::text("Hello world")],
                    FinishReason::Stop,
                    Some(Usage::new(9, 3)),
                )
            }
        );
    }

    #[test]
    fn parse_streaming_tool_call_jsonl_fixture_to_normalized_events() {
        let fixture = include_str!("../tests/fixtures/chat_completion_stream_tool_call.jsonl");
        let events = crate::parse::parse_chat_completion_stream_events(fixture)
            .expect("streamed tool call should parse");

        assert_eq!(events.len(), 3);
        assert_eq!(events[0], ModelEvent::Started);

        let expected_call = match &events[1] {
            ModelEvent::ToolCallRequested { call } => {
                assert_eq!(call.id().as_str(), "call_stream_123");
                assert_eq!(call.name().as_str(), "lookup_weather");
                assert_eq!(
                    call.arguments().as_object().get("city"),
                    Some(&Value::String("Shanghai".to_owned()))
                );
                call.clone()
            }
            event => panic!("expected streamed tool call, got {event:?}"),
        };

        assert_eq!(
            events[2],
            ModelEvent::Completed {
                response: merry_llm::ModelResponse::new(
                    vec![ModelOutput::tool_call(expected_call)],
                    FinishReason::ToolCalls,
                    Some(Usage::new(14, 6)),
                )
            }
        );
    }

    #[test]
    fn malformed_streaming_tool_arguments_fixture_returns_protocol_error() {
        let fixture = include_str!("../tests/fixtures/chat_completion_stream_bad_tool_args.jsonl");
        let error = crate::parse::parse_chat_completion_stream_events(fixture)
            .expect_err("malformed streamed tool arguments should fail");

        assert_eq!(error.kind(), ProviderErrorKind::Protocol);
    }

    #[test]
    fn streaming_usage_chunk_completes_after_finish_reason_with_empty_choices() {
        let fixture = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Done\"},\"finish_reason\":null}],\"usage\":null}\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":null}\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":1,\"total_tokens\":5}}\n",
            "data: [DONE]\n",
        );

        let events = crate::parse::parse_chat_completion_stream_events(fixture)
            .expect("usage-only completion chunk should parse");

        assert_eq!(
            events.last(),
            Some(&ModelEvent::Completed {
                response: merry_llm::ModelResponse::new(
                    vec![ModelOutput::text("Done")],
                    FinishReason::Stop,
                    Some(Usage::new(4, 1)),
                )
            })
        );
    }
}
