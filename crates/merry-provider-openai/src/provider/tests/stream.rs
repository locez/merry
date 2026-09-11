use super::model_request;
use crate::{OpenAiProtocol, OpenAiProviderError};
use crate::{
    OpenAiProviderConfig, parse::ResponsesStreamParser, provider::OpenAiEventStreamEvents,
};
use merry_llm::{
    FinishReason, ModelEvent, ModelOutput, ModelProvider, ModelResponse, ModelStreamContext, Usage,
};
use std::collections::VecDeque;
use tokio_util::sync::CancellationToken;

#[test]
fn parses_sse_lines_to_started_deltas_and_completed_without_network() {
    let mut parser = ResponsesStreamParser::new();
    let mut events = VecDeque::from([ModelEvent::Started]);

    for line in [
        "data: {\"type\":\"response.created\"}",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello\"}",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\" world\"}",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello world\"}]}],\"usage\":{\"input_tokens\":9,\"output_tokens\":3}}}",
        "data: [DONE]",
    ] {
        events.extend(
            parser
                .parse_sse_line(line)
                .expect("stream line should parse"),
        );
    }
    parser.finish().expect("stream should complete");

    assert_eq!(
        events.into_iter().collect::<Vec<_>>(),
        vec![
            ModelEvent::Started,
            ModelEvent::OutputTextDelta {
                delta: "Hello".to_owned()
            },
            ModelEvent::OutputTextDelta {
                delta: " world".to_owned()
            },
            ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("Hello world")],
                    FinishReason::Stop,
                    Some(Usage::new(9, 3)),
                )
            },
        ]
    );
}

#[test]
fn parser_accepts_standard_sse_metadata_and_data_without_space() {
    let mut parser = ResponsesStreamParser::new();
    let mut events = VecDeque::from([ModelEvent::Started]);

    for line in [
        ": provider heartbeat",
        "event: response.output_text.delta",
        "id: stream-1",
        "retry: 1000",
        "data:{\"type\":\"response.output_text.delta\",\"delta\":\"Hello\"}",
        "data:{\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello\"}]}],\"usage\":{\"input_tokens\":4,\"output_tokens\":1}}}",
    ] {
        events.extend(
            parser
                .parse_sse_line(line)
                .expect("standard SSE line should parse or be ignored"),
        );
    }
    parser.finish().expect("stream should complete");

    assert_eq!(
        events.into_iter().collect::<Vec<_>>(),
        vec![
            ModelEvent::Started,
            ModelEvent::OutputTextDelta {
                delta: "Hello".to_owned()
            },
            ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("Hello")],
                    FinishReason::Stop,
                    Some(Usage::new(4, 1)),
                )
            },
        ]
    );
}

#[test]
fn parser_reports_unexpected_non_sse_stream_lines() {
    let mut parser = ResponsesStreamParser::new();

    let error = parser
        .parse_sse_line(r#"{"error":"bad upstream"}"#)
        .expect_err("non-SSE line should fail");

    assert!(matches!(error, OpenAiProviderError::Protocol { .. }));
    assert!(
        error
            .to_string()
            .contains("unexpected Responses stream line")
    );
    assert!(error.to_string().contains("expected an SSE `data:` field"));
}

#[test]
fn responses_stream_error_preserves_safe_server_message() {
    let mut parser = ResponsesStreamParser::new();
    let error = parser
        .parse_sse_line(
            r#"data: {"type":"error","code":"invalid_request_error","message":"response schema is invalid"}"#,
        )
        .expect_err("provider stream error should be surfaced");

    assert!(error.to_string().contains("invalid_request_error"));
    assert!(error.to_string().contains("response schema is invalid"));
    assert_eq!(
        merry_llm::ModelError::from(error).kind(),
        merry_llm::ProviderErrorKind::Protocol
    );
}

#[test]
fn responses_server_error_is_normalized_as_transient() {
    let mut parser = ResponsesStreamParser::new();
    let error = parser
        .parse_sse_line(
            r#"data: {"type":"error","code":"server_error","message":"Our server are currently overload. Please try again later."}"#,
        )
        .expect_err("provider stream error should be surfaced");
    let error: merry_llm::ModelError = error.into();

    assert_eq!(error.kind(), merry_llm::ProviderErrorKind::Unavailable);
    assert!(error.message().contains("server_error"));
    assert!(error.message().contains("try again later"));
}

#[test]
fn stream_state_emits_completed_from_final_usage_line_without_trailing_newline() {
    let mut events = OpenAiEventStreamEvents::new(OpenAiProtocol::Responses);

    assert_eq!(events.pop_pending(), Some(ModelEvent::Started));
    events
        .parse_bytes(b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"Done\"}\n")
        .expect("text delta should parse");
    assert_eq!(
        events.pop_pending(),
        Some(ModelEvent::OutputTextDelta {
            delta: "Done".to_owned()
        })
    );
    events
        .parse_bytes(b"data: {\"type\":\"response.created\"}\n")
        .expect("created event should parse");
    events
            .parse_bytes(b"data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"Done\"}]}],\"usage\":{\"input_tokens\":4,\"output_tokens\":1}}}")
            .expect("unterminated completion line should buffer");

    events.finish_stream().expect("stream should finish");

    assert_eq!(
        events.pop_pending(),
        Some(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("Done")],
                FinishReason::Stop,
                Some(Usage::new(4, 1)),
            )
        })
    );
    assert_eq!(events.pop_pending(), None);
}

#[test]
fn eof_finalization_returns_completed_from_final_usage_line_without_trailing_newline() {
    let mut events = OpenAiEventStreamEvents::new(OpenAiProtocol::Responses);
    assert_eq!(events.pop_pending(), Some(ModelEvent::Started));
    events
        .parse_bytes(b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"Done\"}\n")
        .expect("text delta should parse");
    assert_eq!(
        events.pop_pending(),
        Some(ModelEvent::OutputTextDelta {
            delta: "Done".to_owned()
        })
    );
    events
        .parse_bytes(b"data: {\"type\":\"response.created\"}\n")
        .expect("created event should parse");
    events
            .parse_bytes(b"data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"Done\"}]}],\"usage\":{\"input_tokens\":4,\"output_tokens\":1}}}")
            .expect("unterminated completion line should buffer");

    let event = events
        .finish_stream_and_pop_pending()
        .expect("EOF finalization should succeed");

    assert_eq!(
        event,
        Some(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("Done")],
                FinishReason::Stop,
                Some(Usage::new(4, 1)),
            )
        })
    );
    assert_eq!(events.pop_pending(), None);
}

#[tokio::test]
async fn pre_cancelled_stream_setup_fails_before_network_request() {
    let token = CancellationToken::new();
    token.cancel();

    let config = OpenAiProviderConfig::new("sk-test")
        .expect("valid config")
        .with_base_url("https://api.example.test/v1")
        .expect("valid base URL");
    let error = crate::provider::OpenAiProvider::new(config)
        .stream_model(model_request(), ModelStreamContext::new(token))
        .await;
    let error = match error {
        Ok(_) => panic!("pre-cancelled setup should fail before sending"),
        Err(error) => error,
    };

    assert!(matches!(error, merry_llm::ModelError::Cancelled));
}
