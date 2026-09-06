use merry_core::{PendingToolCall, ToolInputSchema, ToolName, ToolSpec};
use merry_runtime::{
    FinalOutputContract, ToolExecutionContext, ToolExecutionError, ToolExecutionOutcome,
    ToolExecutor, ToolExecutorFuture,
};
use schemars::Schema;
use serde_json::json;
use std::sync::{Arc, Mutex};
use tokio::sync::{Mutex as AsyncMutex, oneshot};

pub(crate) fn tool_spec(name: &str) -> ToolSpec {
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
    .expect("test schema should be a JSON schema");

    FinalOutputContract::new(ToolInputSchema::new(schema).expect("valid final output schema"))
        .expect("valid final output contract")
}

#[derive(Clone)]
pub(crate) struct ScriptedToolExecutor {
    calls: Arc<Mutex<Vec<PendingToolCall>>>,
    response: ToolExecutorResponse,
}

#[derive(Clone)]
pub(crate) enum ToolExecutorResponse {
    Outcome(ToolExecutionOutcome),
    ScriptedOutcomes(Arc<Mutex<Vec<ToolExecutionOutcome>>>),
    InfrastructureError(String),
}

impl ScriptedToolExecutor {
    pub(crate) fn succeeding_text(text: &str) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            response: ToolExecutorResponse::Outcome(ToolExecutionOutcome::succeeded_text(text)),
        }
    }

    pub(crate) fn succeeding_texts(texts: Vec<String>) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            response: ToolExecutorResponse::ScriptedOutcomes(Arc::new(Mutex::new(
                texts
                    .into_iter()
                    .map(|text| ToolExecutionOutcome::succeeded_text(&text))
                    .rev()
                    .collect(),
            ))),
        }
    }

    pub(crate) fn infrastructure_error(message: &str) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            response: ToolExecutorResponse::InfrastructureError(message.to_owned()),
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
                ToolExecutorResponse::ScriptedOutcomes(outcomes) => outcomes
                    .lock()
                    .expect("scripted outcomes mutex should not be poisoned")
                    .pop()
                    .ok_or_else(|| ToolExecutionError::infrastructure("no scripted outcome")),
                ToolExecutorResponse::InfrastructureError(message) => {
                    Err(ToolExecutionError::infrastructure(message.clone()))
                }
            }
        })
    }
}

#[derive(Clone)]
pub(crate) struct BlockingToolExecutor {
    calls: Arc<Mutex<Vec<PendingToolCall>>>,
    started_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    release_rx: Arc<AsyncMutex<Option<oneshot::Receiver<()>>>>,
}

impl BlockingToolExecutor {
    pub(crate) fn new(started_tx: oneshot::Sender<()>, release_rx: oneshot::Receiver<()>) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            started_tx: Arc::new(Mutex::new(Some(started_tx))),
            release_rx: Arc::new(AsyncMutex::new(Some(release_rx))),
        }
    }

    pub(crate) fn calls(&self) -> Vec<PendingToolCall> {
        self.calls
            .lock()
            .expect("tool calls mutex should not be poisoned")
            .clone()
    }
}

impl ToolExecutor for BlockingToolExecutor {
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

            if let Some(started_tx) = self
                .started_tx
                .lock()
                .expect("started signal mutex should not be poisoned")
                .take()
            {
                let _ = started_tx.send(());
            }

            let release_rx = self
                .release_rx
                .lock()
                .await
                .take()
                .expect("blocking executor should only be used once");
            release_rx
                .await
                .expect("test should release the blocking executor");

            Ok(ToolExecutionOutcome::succeeded_text("search result\n"))
        })
    }
}
