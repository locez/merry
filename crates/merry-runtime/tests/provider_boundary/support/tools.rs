use merry_core::{
    ArtifactRef, ErrorInfo, PendingToolCall, ToolCallId, ToolCallResult, ToolInputSchema, ToolName,
    ToolSpec,
};
use merry_runtime::{
    ToolExecutionContext, ToolExecutionError, ToolExecutionOutcome, ToolExecutor,
    ToolExecutorFuture,
};
use schemars::Schema;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

pub(crate) fn failed_tool_result(
    call_id: ToolCallId,
    artifact: ArtifactRef,
    code: &str,
    message: &str,
) -> ToolCallResult {
    ToolCallResult::failed(
        call_id,
        artifact,
        merry_core::ErrorInfo::new(code, message).expect("valid diagnostic"),
    )
}

pub(crate) fn test_tool_spec(name: &str) -> ToolSpec {
    let schema = Schema::try_from(json!({
        "type": "object",
        "properties": {
            "query": { "type": "string" }
        },
        "required": ["query"]
    }))
    .expect("test schema should be a JSON schema");

    ToolSpec::new(
        ToolName::new(name).expect("valid tool name"),
        "Search test notes",
        ToolInputSchema::new(schema).expect("valid tool schema"),
    )
    .expect("valid tool spec")
}

pub(crate) fn assert_default_checkpoint_ref_tool(tools: &[ToolSpec]) {
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name().as_str(), "merry_read_checkpoint_ref");
}

pub(crate) fn assert_tools_are_default_checkpoint_ref_plus(
    tools: &[ToolSpec],
    expected_user_tools: &[ToolSpec],
) {
    assert_eq!(tools.len(), expected_user_tools.len() + 1);
    assert_eq!(tools[0].name().as_str(), "merry_read_checkpoint_ref");
    assert_eq!(&tools[1..], expected_user_tools);
}

pub(crate) fn path_tool_spec(name: &str) -> ToolSpec {
    let schema = Schema::try_from(json!({
        "type": "object",
        "properties": {
            "path": { "type": "string" }
        },
        "required": ["path"],
        "additionalProperties": false
    }))
    .expect("test schema should be a JSON schema");

    ToolSpec::new(
        ToolName::new(name).expect("valid tool name"),
        "Read a workspace file",
        ToolInputSchema::new(schema).expect("valid tool schema"),
    )
    .expect("valid tool spec")
}

#[derive(Clone)]
pub(crate) struct ScriptedToolExecutor {
    calls: Arc<Mutex<Vec<PendingToolCall>>>,
    response: ToolExecutorResponse,
}

#[derive(Clone)]
pub(crate) enum ToolExecutorResponse {
    Outcome(ToolExecutionOutcome),
    Error(Arc<dyn Fn() -> ToolExecutionError + Send + Sync>),
}

impl ScriptedToolExecutor {
    pub(crate) fn succeeding_text(text: &str) -> Self {
        Self::new(ToolExecutorResponse::Outcome(
            ToolExecutionOutcome::succeeded_text(text),
        ))
    }

    pub(crate) fn failing_json(code: &str, content: &str) -> Self {
        Self::new(ToolExecutorResponse::Outcome(
            ToolExecutionOutcome::failed_json(
                content,
                ErrorInfo::new(code, "tool domain failure").expect("valid diagnostic"),
            ),
        ))
    }

    pub(crate) fn infrastructure_error(message: &str) -> Self {
        let message = message.to_owned();
        Self::new(ToolExecutorResponse::Error(Arc::new(move || {
            ToolExecutionError::infrastructure(message.clone())
        })))
    }

    pub(crate) fn new(response: ToolExecutorResponse) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            response,
        }
    }

    pub(crate) fn calls(&self) -> Vec<PendingToolCall> {
        self.calls
            .lock()
            .expect("tool calls mutex should not be poisoned")
            .clone()
    }
}

impl ToolExecutor for ScriptedToolExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            self.calls
                .lock()
                .expect("tool calls mutex should not be poisoned")
                .push(call);

            match &self.response {
                ToolExecutorResponse::Outcome(outcome) => Ok(outcome.clone()),
                ToolExecutorResponse::Error(error) => Err(error()),
            }
        })
    }
}

pub(crate) fn assert_sanitized_policy_denial_json(value: &Value, tool_name: &str) {
    assert_eq!(
        value,
        &json!({
            "ok": false,
            "tool": tool_name,
            "error": {
                "code": "action_policy_denied",
                "message": "tool action was blocked by runtime policy"
            }
        })
    );
    assert!(value.get("call_id").is_none());
    assert!(value.get("action_kind").is_none());
    assert!(value.get("policy").is_none());
    assert!(value.get("reason").is_none());
    assert!(value.get("provider").is_none());
    assert!(value.get("provider_response").is_none());
    assert!(value.get("wire").is_none());
    assert!(value.get("previous_response_id").is_none());
}
