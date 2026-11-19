use crate::{assert_schema_compiles, json_schema};
use merry_core::{
    CompactionUsageWindow, ContextWindowSource, CoreError, ErrorInfo, ImageSummary,
    MerryErrorDomain, MerryErrorInfo, MerryRetryability, ModelUsage, PendingToolCall, ProviderName,
    RuntimeEvent, RuntimeJournalEvent, RuntimeJournalPayload, SessionId, SessionUsage, SkillId,
    SubagentActivityPhase, SubagentActivitySnapshot, ToolCallArguments, ToolCallId, ToolCallResult,
    ToolCallResultStatus, ToolInputSchema, ToolName, ToolOutput, ToolOutputKind, ToolOutputPreview,
    ToolSpec, UsageContextWindow,
};
use schemars::Schema;
use serde_json::json;

#[test]
fn merry_error_info_serializes_stable_sdk_shape() {
    let diagnostic = MerryErrorInfo::builder(
        "tool.executor_exception",
        MerryErrorDomain::Tool,
        "Tool `lookup_order` raised an unexpected exception.",
        MerryRetryability::NotRetryable,
    )
    .hint("Handle expected business failures inside the tool.")
    .context("tool_name", "lookup_order")
    .context("call_id", "call_123")
    .build()
    .expect("valid SDK error info");

    assert_eq!(
        serde_json::to_value(&diagnostic).expect("serializes"),
        json!({
            "code": "tool.executor_exception",
            "domain": "tool",
            "message": "Tool `lookup_order` raised an unexpected exception.",
            "hint": "Handle expected business failures inside the tool.",
            "retryability": "not_retryable",
            "context": {
                "call_id": "call_123",
                "tool_name": "lookup_order"
            }
        })
    );
}

#[test]
fn merry_error_info_rejects_unbounded_or_sensitive_context_keys() {
    let error = MerryErrorInfo::builder(
        "provider.stream_failed",
        MerryErrorDomain::Provider,
        "Provider stream failed.",
        MerryRetryability::Retryable,
    )
    .context("authorization", "Bearer secret")
    .build()
    .expect_err("authorization context must be rejected");

    assert!(error.to_string().contains("context key is not allowed"));
}

#[test]
fn core_error_display_messages_include_actionable_context() {
    let id_error = SessionId::new("bad\nid").expect_err("control character should reject");
    assert!(matches!(id_error, CoreError::InvalidIdentifier { .. }));
    assert!(
        id_error
            .to_string()
            .contains("SessionId must not contain control characters")
    );

    let schema_error = ToolInputSchema::new(Schema::try_from(json!(true)).expect("boolean schema"))
        .expect_err("boolean schema should reject");
    assert!(matches!(schema_error, CoreError::InvalidSchema { .. }));
    assert!(
        schema_error
            .to_string()
            .contains("ToolInputSchema must be a JSON object")
    );

    let tool_error = ToolSpec::new(
        ToolName::new("valid_tool").expect("valid name"),
        "",
        ToolInputSchema::new(json_schema(json!({}))).expect("valid schema"),
    )
    .expect_err("blank description should reject");
    assert!(matches!(tool_error, CoreError::InvalidToolSpec { .. }));
    assert!(
        tool_error
            .to_string()
            .contains("ToolSpec description must not be blank")
    );
}

#[test]
fn schemars_generation_compiles_for_public_protocol_types() {
    assert_schema_compiles::<SessionId>();
    assert_schema_compiles::<ToolName>();
    assert_schema_compiles::<SkillId>();
    assert_schema_compiles::<ProviderName>();
    assert_schema_compiles::<ToolCallId>();
    assert_schema_compiles::<ToolCallArguments>();
    assert_schema_compiles::<PendingToolCall>();
    assert_schema_compiles::<ToolCallResultStatus>();
    assert_schema_compiles::<ToolCallResult>();
    assert_schema_compiles::<ToolOutputKind>();
    assert_schema_compiles::<ToolOutput>();
    assert_schema_compiles::<ToolOutputPreview>();
    assert_schema_compiles::<ImageSummary>();
    assert_schema_compiles::<ToolInputSchema>();
    assert_schema_compiles::<ToolSpec>();
    assert_schema_compiles::<ErrorInfo>();
    assert_schema_compiles::<RuntimeJournalEvent>();
    assert_schema_compiles::<RuntimeJournalPayload>();
    assert_schema_compiles::<RuntimeEvent>();
    assert_schema_compiles::<ModelUsage>();
    assert_schema_compiles::<SessionUsage>();
    assert_schema_compiles::<UsageContextWindow>();
    assert_schema_compiles::<CompactionUsageWindow>();
    assert_schema_compiles::<ContextWindowSource>();
    assert_schema_compiles::<SubagentActivityPhase>();
    assert_schema_compiles::<SubagentActivitySnapshot>();
}
