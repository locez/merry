//! OpenAI-compatible provider adapter for Merry.

mod chat_completions;
mod config;
mod error;
mod image;
mod models;
mod parse;
mod provider;
mod render;
mod tool_arguments;
mod wire;

pub use config::{OpenAiProtocol, OpenAiProviderConfig};
pub use error::OpenAiProviderError;
pub use provider::OpenAiProvider;

#[cfg(test)]
mod tests {
    use super::*;
    use merry_core::{ToolCallResultStatus, ToolInputSchema, ToolName, ToolSpec};
    use merry_llm::{
        FinishReason, GenerationConfig, ModelContent, ModelEvent, ModelMessage, ModelMessageRole,
        ModelName, ModelOutput, ModelRequest, ModelResponseFormat, ModelStructuredOutputFormat,
        ModelToolCall, ModelToolCallId, ModelToolContinuation, ModelToolResult,
        ModelToolResultContent, ProviderErrorKind, ReasoningEffort, ServiceTier, ToolArguments,
        Usage,
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

    fn request_with_reasoning_effort() -> ModelRequest {
        ModelRequest::new(
            ModelName::new("gpt-5.1").expect("valid model name"),
            vec![message(ModelMessageRole::User, "Solve this carefully.")],
            Vec::new(),
            GenerationConfig::default().with_reasoning_effort(Some(
                ReasoningEffort::new("high").expect("valid reasoning effort"),
            )),
        )
        .expect("valid model request")
    }

    fn request_with_service_tier() -> ModelRequest {
        ModelRequest::new(
            ModelName::new("gpt-5.1").expect("valid model name"),
            vec![message(ModelMessageRole::User, "Hello")],
            Vec::new(),
            GenerationConfig::default().with_service_tier(Some(ServiceTier::Priority)),
        )
        .expect("valid model request")
    }

    fn request_with_structured_output() -> ModelRequest {
        let schema = serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "answer": { "type": "string" }
            },
            "required": ["answer"],
            "additionalProperties": false
        }))
        .expect("valid schema");
        let format = ModelResponseFormat::StructuredOutput(
            ModelStructuredOutputFormat::new("answer_payload", schema)
                .expect("valid structured output format"),
        );

        ModelRequest::new_with_response_format(
            ModelName::new("gpt-4.1-mini").expect("valid model name"),
            vec![message(ModelMessageRole::User, "Hello")],
            Vec::new(),
            GenerationConfig::default(),
            Some(format),
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
                message(ModelMessageRole::User, "Use the weather tool result."),
            ],
            vec![weather_tool()],
            vec![tool_continuation_with_json_result()],
            GenerationConfig::new(Some(256), false).expect("valid generation config"),
        )
        .expect("valid model request")
    }

    fn request_with_assistant_commentary_and_tool_continuation() -> ModelRequest {
        ModelRequest::new_with_continuations(
            ModelName::new("gpt-4.1-mini").expect("valid model name"),
            vec![
                message(ModelMessageRole::System, "You are concise."),
                message(ModelMessageRole::User, "Weather in Shanghai?"),
                message(
                    ModelMessageRole::Assistant,
                    "I will check the weather tool.",
                ),
                message(ModelMessageRole::User, "Use the weather tool result."),
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
        assert!(config.capabilities().supports_parallel_tool_calls());
        assert_eq!(config.protocol(), OpenAiProtocol::Responses);
        assert!(config.capabilities().supports_usage_reporting());
        assert_eq!(config.capabilities().max_input_tokens(), None);
        assert_eq!(config.capabilities().max_output_tokens(), None);

        let debug = format!("{config:?}");
        assert!(!debug.contains("sk-test"));
        assert!(debug.contains("<redacted>"));
        assert!(debug.contains("Responses"));

        let chat = config
            .clone()
            .with_protocol(OpenAiProtocol::ChatCompletions);
        assert_eq!(chat.protocol(), OpenAiProtocol::ChatCompletions);

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
        for invalid in [
            "https://api.example.test/v1?tenant=one",
            "https://api.example.test/v1#fragment",
        ] {
            assert!(
                OpenAiProviderConfig::new("sk-test")
                    .and_then(|config| config.with_base_url(invalid))
                    .is_err()
            );
        }
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
    fn rendered_request_with_tool_matches_responses_json_without_runtime_state() {
        let rendered = crate::render::render_responses_request(&request_with_tools())
            .expect("request should render");

        assert_eq!(
            rendered,
            json!({
                "model": "gpt-4.1-mini",
                "input": [
                    {
                        "role": "system",
                        "content": [
                            {
                                "type": "input_text",
                                "text": "You are concise."
                            }
                        ]
                    },
                    {
                        "role": "user",
                        "content": [
                            {
                                "type": "input_text",
                                "text": "Weather in Shanghai?"
                            }
                        ]
                    }
                ],
                "stream": true,
                "store": false,
                "parallel_tool_calls": false,
                "max_output_tokens": 256,
                "tools": [
                    {
                        "type": "function",
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
                ],
                "tool_choice": "auto"
            })
        );

        let runtime_state_fields = [
            "previous_response_id",
            "thread_id",
            "session_id",
            "ledger_id",
            "conversation",
        ];
        let object = rendered
            .as_object()
            .expect("request JSON should be an object");
        assert_eq!(object.get("store"), Some(&json!(false)));
        for field in runtime_state_fields {
            assert!(!object.contains_key(field), "{field} should be omitted");
        }
    }

    #[test]
    fn rendered_request_without_tools_omits_tools_and_tool_choice() {
        let rendered = crate::render::render_responses_request(&request_without_tools())
            .expect("request should render");
        let object = rendered
            .as_object()
            .expect("request JSON should be an object");

        assert_eq!(object.get("parallel_tool_calls"), Some(&json!(false)));
        assert_eq!(object.get("store"), Some(&json!(false)));
        assert!(!object.contains_key("tools"));
        assert!(!object.contains_key("tool_choice"));
        assert!(!object.contains_key("max_output_tokens"));
    }

    #[test]
    fn rendered_request_with_service_tier_uses_responses_service_tier_field() {
        let rendered = crate::render::render_responses_request(&request_with_service_tier())
            .expect("request should render");

        assert_eq!(rendered["service_tier"], json!("priority"));
    }

    #[test]
    fn rendered_request_without_service_tier_omits_service_tier_field() {
        let rendered = crate::render::render_responses_request(&request_without_tools())
            .expect("request should render");

        assert!(rendered.get("service_tier").is_none());
    }

    #[test]
    fn rendered_chat_request_with_service_tier_uses_service_tier_field() {
        let rendered =
            crate::chat_completions::render::render_chat_request(&request_with_service_tier())
                .expect("request should render");

        assert_eq!(rendered["service_tier"], json!("priority"));
    }

    #[test]
    fn rendered_request_with_reasoning_effort_uses_responses_reasoning_field() {
        let rendered = crate::render::render_responses_request(&request_with_reasoning_effort())
            .expect("request should render");

        assert_eq!(rendered["reasoning"], json!({ "effort": "high" }));
    }

    #[test]
    fn rendered_request_with_structured_output_uses_text_format_json_schema() {
        let rendered = crate::render::render_responses_request(&request_with_structured_output())
            .expect("request should render");

        assert_eq!(
            rendered["text"],
            json!({
                "format": {
                    "type": "json_schema",
                    "name": "answer_payload",
                    "strict": true,
                    "schema": {
                        "type": "object",
                        "properties": {
                            "answer": { "type": "string" }
                        },
                        "required": ["answer"],
                        "additionalProperties": false
                    }
                }
            })
        );
        assert!(rendered.get("tools").is_none());
        assert!(rendered.get("tool_choice").is_none());
    }

    #[test]
    fn rendered_request_tool_continuation_matches_responses_json_without_state() {
        let rendered = crate::render::render_responses_request(&request_with_tool_continuation())
            .expect("request should render");

        assert_eq!(
            rendered,
            json!({
                "model": "gpt-4.1-mini",
                "input": [
                    {
                        "role": "system",
                        "content": [
                            {
                                "type": "input_text",
                                "text": "You are concise."
                            }
                        ]
                    },
                    {
                        "role": "user",
                        "content": [
                            {
                                "type": "input_text",
                                "text": "Use the weather tool result."
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
                        "output": "{\"temperature_c\":22,\"condition\":\"clear\"}"
                    }
                ],
                "stream": true,
                "store": false,
                "parallel_tool_calls": false,
                "max_output_tokens": 256,
                "tools": [
                    {
                        "type": "function",
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
                ],
                "tool_choice": "auto"
            })
        );

        let object = rendered
            .as_object()
            .expect("request JSON should be an object");
        assert_eq!(object.get("store"), Some(&json!(false)));
        for field in [
            "previous_response_id",
            "thread_id",
            "session_id",
            "ledger_id",
            "conversation",
        ] {
            assert!(!object.contains_key(field), "{field} should be omitted");
        }
    }

    #[test]
    fn rendered_assistant_history_uses_responses_output_message_item() {
        let rendered = crate::render::render_responses_request(
            &request_with_assistant_commentary_and_tool_continuation(),
        )
        .expect("request should render");

        assert_eq!(
            rendered["input"],
            json!([
                {
                    "role": "system",
                    "content": [
                        {
                            "type": "input_text",
                            "text": "You are concise."
                        }
                    ]
                },
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
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "output_text",
                            "text": "I will check the weather tool.",
                            "annotations": []
                        }
                    ]
                },
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "input_text",
                            "text": "Use the weather tool result."
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
                    "output": "{\"temperature_c\":22,\"condition\":\"clear\"}"
                }
            ])
        );

        assert_eq!(rendered.get("store"), Some(&json!(false)));
        assert!(rendered.get("previous_response_id").is_none());
    }

    #[test]
    fn rendered_request_enables_parallel_tool_calls() {
        let rendered = crate::render::render_responses_request(&request_with_parallel_tool_calls())
            .expect("parallel tool calls should render");

        assert_eq!(rendered["parallel_tool_calls"], true);
    }

    #[test]
    fn parse_text_response_fixture_to_model_response() {
        let fixture = include_str!("../tests/fixtures/responses_text.json");
        let response =
            crate::parse::parse_responses_response(fixture).expect("fixture should parse");

        assert_eq!(response.finish_reason(), FinishReason::Stop);
        assert_eq!(
            response.usage(),
            Some(Usage::with_details(12, None, 5, None, 17))
        );
        assert_eq!(
            response.outputs(),
            &[ModelOutput::text("Hello from the assistant.")]
        );
    }

    #[test]
    fn parse_tool_call_response_fixture_to_model_response() {
        let fixture = include_str!("../tests/fixtures/responses_tool_call.json");
        let response =
            crate::parse::parse_responses_response(fixture).expect("fixture should parse");

        assert_eq!(response.finish_reason(), FinishReason::ToolCalls);
        assert_eq!(
            response.usage(),
            Some(Usage::with_details(20, None, 8, None, 28))
        );
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
    fn responses_tool_arguments_recover_literal_controls_after_outer_json_decode() {
        let response = crate::parse::parse_responses_response(
            "{\n\
                \"status\": \"completed\",\n\
                \"output\": [{\n\
                    \"type\": \"function_call\",\n\
                    \"call_id\": \"call_control_chars\",\n\
                    \"name\": \"run_process\",\n\
                    \"arguments\": \"{\\\"command\\\":\\\"printf 'line one\\nline two\\tvalue'\\\"}\"\n\
                }]\n\
            }",
        )
        .expect("literal controls in nested arguments should recover");

        let ModelOutput::ToolCall { call } = &response.outputs()[0] else {
            panic!("expected a tool call output");
        };
        assert_eq!(
            call.arguments().as_object().get("command"),
            Some(&Value::String(
                "printf 'line one\nline two\tvalue'".to_owned()
            ))
        );
    }

    #[test]
    fn responses_usage_preserves_cached_reasoning_and_total_counts() {
        let response = crate::parse::parse_responses_response(
            r#"{
                "status": "completed",
                "output": [
                    {
                        "type": "message",
                        "content": [
                            { "type": "output_text", "text": "Done" }
                        ]
                    }
                ],
                "usage": {
                    "input_tokens": 20,
                    "input_tokens_details": { "cached_tokens": 12 },
                    "output_tokens": 8,
                    "output_tokens_details": { "reasoning_tokens": 3 },
                    "total_tokens": 28
                }
            }"#,
        )
        .expect("response should parse");

        assert_eq!(
            response.usage(),
            Some(Usage::with_details(20, Some(12), 8, Some(3), 28))
        );
    }

    #[test]
    fn malformed_tool_arguments_fixture_returns_invalid_tool_call() {
        let fixture = include_str!("../tests/fixtures/responses_bad_tool_args.json");
        let error = crate::parse::parse_responses_response(fixture)
            .expect_err("malformed tool arguments should fail");

        assert_eq!(error.kind(), ProviderErrorKind::InvalidToolCall);
    }

    #[test]
    fn parse_streaming_text_jsonl_fixture_to_normalized_events() {
        let fixture = include_str!("../tests/fixtures/responses_stream_text.jsonl");
        let events =
            crate::parse::parse_responses_stream_events(fixture).expect("stream should parse");

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
                    Some(Usage::with_details(9, None, 3, None, 12)),
                )
            }
        );
    }

    #[test]
    fn parse_streaming_tool_call_jsonl_fixture_to_normalized_events() {
        let fixture = include_str!("../tests/fixtures/responses_stream_tool_call.jsonl");
        let events = crate::parse::parse_responses_stream_events(fixture)
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
                    Some(Usage::with_details(14, None, 6, None, 20)),
                )
            }
        );
    }

    #[test]
    fn malformed_streaming_tool_arguments_fixture_returns_invalid_tool_call() {
        let fixture = include_str!("../tests/fixtures/responses_stream_bad_tool_args.jsonl");
        let error = crate::parse::parse_responses_stream_events(fixture)
            .expect_err("malformed streamed tool arguments should fail");

        assert_eq!(error.kind(), ProviderErrorKind::InvalidToolCall);
    }

    #[test]
    fn streaming_completed_event_completes_response() {
        let fixture = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Done\"}\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"Done\"}]}],\"usage\":{\"input_tokens\":4,\"output_tokens\":1}}}\n",
            "data: [DONE]\n",
        );

        let events = crate::parse::parse_responses_stream_events(fixture)
            .expect("completion event should parse");

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
