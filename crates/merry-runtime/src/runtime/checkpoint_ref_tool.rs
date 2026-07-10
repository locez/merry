use crate::{
    CheckpointRefId, RuntimeError, ToolExecutionError, ToolExecutor, ToolExecutorFuture,
    tool::{RegisteredTool, ToolActionKind, ToolExecutionContext, ToolExecutionOutcome},
};
use merry_core::{
    CoreError, ErrorInfo, PendingToolCall, RuntimeJournalEvent, ToolInputSchema, ToolName, ToolSpec,
};
use schemars::Schema;
use serde_json::json;
use std::sync::Arc;

use super::{RuntimeInner, persist_resume_safe_savepoint_if_configured};

pub(super) const MERRY_READ_CHECKPOINT_REF_TOOL_NAME: &str = "merry_read_checkpoint_ref";
const CHECKPOINT_REF_NOT_FOUND: &str = "checkpoint_ref_not_found";

pub(super) fn merry_read_checkpoint_ref_tool_name() -> ToolName {
    ToolName::new(MERRY_READ_CHECKPOINT_REF_TOOL_NAME).expect("static tool name is valid")
}

pub(super) fn is_merry_read_checkpoint_ref_tool(tool_name: &ToolName) -> bool {
    tool_name.as_str() == MERRY_READ_CHECKPOINT_REF_TOOL_NAME
}

pub(super) fn merry_read_checkpoint_ref_tool() -> Result<RegisteredTool, CoreError> {
    let spec = ToolSpec::new(
        merry_read_checkpoint_ref_tool_name(),
        "Read a bounded excerpt for a ref from the current compacted checkpoint.",
        merry_read_checkpoint_ref_input_schema()?,
    )?;
    Ok(RegisteredTool::new(
        spec,
        Arc::new(MerryReadCheckpointRefExecutor),
        ToolActionKind::ReadOnly,
    )
    .with_parallel_safe_execution())
}

fn merry_read_checkpoint_ref_input_schema() -> Result<ToolInputSchema, CoreError> {
    let schema = Schema::try_from(json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["ref"],
        "properties": {
            "ref": {
                "type": "string",
                "description": "Checkpoint ref id from the current compacted checkpoint, such as r1 or prior-c1."
            }
        }
    }))
    .expect("static checkpoint ref schema should be a JSON object");
    ToolInputSchema::new(schema)
}

#[derive(Debug)]
struct MerryReadCheckpointRefExecutor;

impl ToolExecutor for MerryReadCheckpointRefExecutor {
    fn execute<'a>(
        &'a self,
        _call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async {
            Err(ToolExecutionError::infrastructure(
                "merry_read_checkpoint_ref must be executed through runtime checkpoint ref access",
            ))
        })
    }
}

pub(super) fn checkpoint_ref_not_found_outcome(ref_id: &str) -> ToolExecutionOutcome {
    let payload = json!({
        "error": CHECKPOINT_REF_NOT_FOUND,
        "ref": ref_id,
    });
    ToolExecutionOutcome::failed_json(
        payload.to_string(),
        ErrorInfo::new(CHECKPOINT_REF_NOT_FOUND, "checkpoint ref not found")
            .expect("static diagnostic is valid"),
    )
}

pub(super) async fn execute_merry_read_checkpoint_ref_tool_call(
    inner: &Arc<RuntimeInner>,
    pending: &PendingToolCall,
    context: ToolExecutionContext,
) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
    if context.cancellation_token().is_cancelled() {
        return Err(RuntimeError::ToolExecutionCancelled {
            session_id: inner.session_id.clone(),
            call_id: pending.id().clone(),
        });
    }

    let ref_value = checkpoint_ref_argument(pending);
    let outcome = match CheckpointRefId::new(&ref_value) {
        Ok(ref_id) => {
            let excerpt = {
                let session = inner.session.lock().await;
                session
                    .compacted_checkpoint_summary()
                    .and_then(|summary| summary.checkpoint_id().cloned())
                    .and_then(|checkpoint_id| {
                        session.read_checkpoint_ref(&checkpoint_id, &ref_id).ok()
                    })
            };

            match excerpt {
                Some(excerpt) => {
                    let payload = json!({
                        "ref": excerpt.ref_id().as_str(),
                        "excerpt": excerpt.excerpt(),
                    });
                    ToolExecutionOutcome::succeeded_json(payload.to_string())
                }
                None => checkpoint_ref_not_found_outcome(&ref_value),
            }
        }
        Err(_) => checkpoint_ref_not_found_outcome(&ref_value),
    };

    let (status, content, diagnostic, execution_evidence) = outcome.into_parts();
    debug_assert!(execution_evidence.is_none());
    let events = {
        let mut session = inner.session.lock().await;
        if context.cancellation_token().is_cancelled() {
            return Err(RuntimeError::ToolExecutionCancelled {
                session_id: inner.session_id.clone(),
                call_id: pending.id().clone(),
            });
        }
        session.submit_tool_execution_outcome(pending.id(), status, content, diagnostic, None)?
    };
    persist_resume_safe_savepoint_if_configured(inner).await;
    Ok(events)
}

fn checkpoint_ref_argument(pending: &PendingToolCall) -> String {
    pending
        .arguments()
        .as_object()
        .get("ref")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}
