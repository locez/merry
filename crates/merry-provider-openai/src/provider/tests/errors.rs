use crate::provider::{classify_http_status, format_provider_error_message};
use merry_llm::ProviderErrorKind;

#[test]
fn classifies_http_status_without_network() {
    assert_eq!(
        classify_http_status(reqwest::StatusCode::UNAUTHORIZED),
        ProviderErrorKind::Authentication
    );
    assert_eq!(
        classify_http_status(reqwest::StatusCode::FORBIDDEN),
        ProviderErrorKind::Authentication
    );
    assert_eq!(
        classify_http_status(reqwest::StatusCode::TOO_MANY_REQUESTS),
        ProviderErrorKind::RateLimited
    );
    assert_eq!(
        classify_http_status(reqwest::StatusCode::BAD_GATEWAY),
        ProviderErrorKind::Unavailable
    );
    assert_eq!(
        classify_http_status(reqwest::StatusCode::BAD_REQUEST),
        ProviderErrorKind::InvalidRequest
    );
}

#[test]
fn provider_error_metadata_does_not_expose_error_body_content() {
    let body = br#"{
            "error": {
                "code": "invalid_request_error",
                "message": "prompt secret sk-test-sensitive user request"
            }
        }"#;

    let details =
        crate::provider::provider_error_details(body).expect("error metadata should parse");
    assert_eq!(details.code.as_deref(), Some("invalid_request_error"));
    let message = format_provider_error_message(
        "Responses",
        reqwest::StatusCode::BAD_REQUEST,
        None,
        Some(&details),
        None,
    );
    assert!(!message.contains("prompt secret"));
    assert!(!message.contains("sk-test"));
    assert!(crate::provider::bounded_provider_metadata("req_abc-123").is_some());
    assert!(crate::provider::bounded_provider_metadata("secret value with spaces").is_none());
}

#[test]
fn provider_error_message_preserves_safe_server_details() {
    let body = br#"{
            "error": {
                "type": "invalid_request_error",
                "code": "invalid_json_schema",
                "param": "text.format.schema",
                "message": "Invalid schema for response_format 'compacted_checkpoint_candidate': Missing 'rationale'."
            }
        }"#;
    let details =
        crate::provider::provider_error_details(body).expect("error details should parse");
    let message = format_provider_error_message(
        "Responses",
        reqwest::StatusCode::BAD_REQUEST,
        Some("api.example.test:443"),
        Some(&details),
        Some("req_abc-123"),
    );

    assert!(message.contains("HTTP 400"));
    assert!(message.contains("host api.example.test:443"));
    assert!(message.contains("type: invalid_request_error"));
    assert!(message.contains("code: invalid_json_schema"));
    assert!(message.contains("param: text.format.schema"));
    assert!(message.contains("server error: Invalid schema for response_format"));
    assert!(message.contains("Missing 'rationale'."));
    assert!(message.contains("request_id: req_abc-123"));
}
