use merry_core::{PendingToolCall, ToolInputSchema, ToolName, ToolSpec};
use merry_llm::{ModelToolCall, ModelToolCallId, ToolArguments};
use merry_runtime::{
    FINAL_OUTPUT_TOOL_NAME, FinalOutputContract, ToolExecutionContext, ToolExecutionOutcome,
    ToolExecutor, ToolExecutorFuture,
};
use schemars::Schema;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Barrier;

pub(crate) fn tool_spec(name: &str) -> ToolSpec {
    let schema = Schema::try_from(json!({
        "type": "object",
        "properties": {
            "query": { "type": "string" }
        },
        "required": ["query"]
    }))
    .expect("test schema should be valid JSON schema");

    ToolSpec::new(
        ToolName::new(name).expect("valid tool name"),
        "Search test notes",
        ToolInputSchema::new(schema).expect("valid tool schema"),
    )
    .expect("valid tool spec")
}

pub(crate) fn final_output_contract() -> FinalOutputContract {
    let schema = Schema::try_from(json!({
        "type": "object",
        "properties": {
            "summary": {
                "type": "string",
                "description": "Short final summary."
            }
        },
        "required": ["summary"],
        "additionalProperties": false
    }))
    .expect("final output schema should be valid");

    FinalOutputContract::new(ToolInputSchema::new(schema).expect("schema should be an object"))
        .expect("final output contract should be valid")
}

pub(crate) fn final_output_call(id: &str, arguments: serde_json::Value) -> ModelToolCall {
    ModelToolCall::new(
        ModelToolCallId::new(id).expect("final output call id should be valid"),
        ToolName::new(FINAL_OUTPUT_TOOL_NAME).expect("final output tool name should be valid"),
        ToolArguments::try_from(arguments).expect("final output arguments should be an object"),
    )
}

#[derive(Clone)]
pub(crate) struct BarrierToolExecutor {
    barrier: Arc<Barrier>,
}

impl BarrierToolExecutor {
    pub(crate) fn new(parties: usize) -> Self {
        Self {
            barrier: Arc::new(Barrier::new(parties)),
        }
    }
}

impl ToolExecutor for BarrierToolExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            self.barrier.wait().await;
            Ok(ToolExecutionOutcome::succeeded_text(format!(
                "result for {}",
                call.id()
            )))
        })
    }
}
