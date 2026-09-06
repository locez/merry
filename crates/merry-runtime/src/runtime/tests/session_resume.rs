use merry_core::ToolName;
use merry_llm::{ModelToolCall, ModelToolCallId, ToolArguments};

fn model_tool_call(id: &str, name: &str, arguments: serde_json::Value) -> ModelToolCall {
    ModelToolCall::new(
        ModelToolCallId::new(id).expect("valid model call id"),
        ToolName::new(name).expect("valid tool name"),
        ToolArguments::try_from(arguments).expect("valid tool arguments"),
    )
}

mod pending_recovery;

mod restoration;

mod savepoints;
