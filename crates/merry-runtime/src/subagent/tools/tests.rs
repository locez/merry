mod execution;
mod schema;

use crate::{
    ArtifactContent, ChildRuntimeFactory, ChildRuntimeInput, Runtime, SubagentConfig,
    SubagentManager,
};
use merry_core::{PendingToolCall, SessionId, ToolCallArguments, ToolCallId, ToolName};
use serde_json::Value;
use std::sync::{Arc, Mutex as StdMutex};

#[derive(Clone, Default)]
struct CapturingChildFactory {
    inputs: Arc<StdMutex<Vec<ChildRuntimeInput>>>,
}

impl CapturingChildFactory {
    fn inputs(&self) -> Vec<ChildRuntimeInput> {
        self.inputs
            .lock()
            .expect("inputs mutex is not poisoned")
            .clone()
    }
}

impl ChildRuntimeFactory for CapturingChildFactory {
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, crate::RuntimeError> {
        self.inputs
            .lock()
            .expect("inputs mutex is not poisoned")
            .push(input.clone());

        Runtime::builder(input.session_id)
            .task_anchor(input.task_anchor)
            .build()
    }
}

fn manager(factory: Arc<dyn ChildRuntimeFactory>) -> SubagentManager {
    SubagentManager::new(
        SessionId::new("parent").expect("valid session id"),
        SubagentConfig::default(),
        factory,
    )
}

fn pending_call(name: &str, arguments: Value) -> PendingToolCall {
    PendingToolCall::new(
        ToolCallId::new("call-1").expect("valid call id"),
        ToolName::new(name).expect("valid tool name"),
        ToolCallArguments::try_from(arguments).expect("object arguments"),
    )
}

fn outcome_json(outcome: &crate::ToolExecutionOutcome) -> Value {
    let ArtifactContent::Json { content } = outcome.content() else {
        panic!("expected JSON tool outcome");
    };
    serde_json::from_str(content).expect("outcome contains valid JSON")
}
