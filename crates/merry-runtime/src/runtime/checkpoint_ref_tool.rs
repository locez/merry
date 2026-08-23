use crate::{
    CheckpointError, CheckpointRefId, RuntimeError, ToolExecutionError, ToolExecutor,
    ToolExecutorFuture,
    tool::{RegisteredTool, ToolActionKind, ToolExecutionContext, ToolExecutionOutcome},
};
use merry_core::{
    CoreError, ErrorInfo, PendingToolCall, RuntimeJournalEvent, ToolInputSchema, ToolName, ToolSpec,
};
use schemars::Schema;
use serde_json::json;
use std::sync::Arc;

use super::RuntimeInner;

pub(super) const MERRY_READ_CHECKPOINT_REF_TOOL_NAME: &str = "merry_read_checkpoint_ref";
const CHECKPOINT_REF_NOT_FOUND: &str = "checkpoint_ref_not_found";
const CHECKPOINT_REF_READ_FAILED: &str = "checkpoint_ref_read_failed";
const CHECKPOINT_REF_ARGUMENTS_INVALID: &str = "checkpoint_ref_arguments_invalid";
const DEFAULT_CHECKPOINT_REF_PAGE_BYTES: usize = 4096;
const MAX_CHECKPOINT_REF_PAGE_BYTES: usize = 16_384;

pub(super) fn merry_read_checkpoint_ref_tool_name() -> ToolName {
    ToolName::new(MERRY_READ_CHECKPOINT_REF_TOOL_NAME).expect("static tool name is valid")
}

pub(super) fn is_merry_read_checkpoint_ref_tool(tool_name: &ToolName) -> bool {
    tool_name.as_str() == MERRY_READ_CHECKPOINT_REF_TOOL_NAME
}

pub(super) fn merry_read_checkpoint_ref_tool() -> Result<RegisteredTool, CoreError> {
    let spec = ToolSpec::new(
        merry_read_checkpoint_ref_tool_name(),
        "Read a bounded page from a ref's original artifact in the current compacted checkpoint.",
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
                "description": "Checkpoint ref id from the current compacted checkpoint, such as h42."
            },
            "offset": {
                "type": "integer",
                "minimum": 0,
                "description": "Zero-based byte offset within the referenced artifact. Omit it to start at the beginning."
            },
            "max_bytes": {
                "type": "integer",
                "minimum": 1,
                "maximum": MAX_CHECKPOINT_REF_PAGE_BYTES,
                "description": "Maximum number of artifact bytes to return in this page. Omit it to use the 4096-byte default."
            }
        }
    }))
    .expect("static checkpoint ref schema should be a JSON object");
    ToolInputSchema::new(schema)
}

fn checkpoint_ref_read_failed_outcome(ref_id: &str, error: &RuntimeError) -> ToolExecutionOutcome {
    let payload = json!({
        "error": CHECKPOINT_REF_READ_FAILED,
        "ref": ref_id,
        "message": error.to_string(),
    });
    ToolExecutionOutcome::failed_json(
        payload.to_string(),
        ErrorInfo::new(
            CHECKPOINT_REF_READ_FAILED,
            "checkpoint ref page could not be read",
        )
        .expect("static diagnostic is valid"),
    )
}

fn checkpoint_ref_arguments_invalid_outcome(reason: &'static str) -> ToolExecutionOutcome {
    ToolExecutionOutcome::failed_json(
        json!({
            "error": CHECKPOINT_REF_ARGUMENTS_INVALID,
            "message": reason,
        })
        .to_string(),
        ErrorInfo::new(CHECKPOINT_REF_ARGUMENTS_INVALID, reason)
            .expect("static diagnostic is valid"),
    )
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

    let outcome = match checkpoint_ref_arguments(pending) {
        Ok(CheckpointRefArguments {
            ref_id: ref_value,
            offset,
            max_bytes,
        }) => match CheckpointRefId::new(&ref_value) {
            Ok(ref_id) => {
                let page = {
                    let session = inner.session.lock().await;
                    session.read_checkpoint_ref_page_with_source(&ref_id, offset, max_bytes)
                };

                match page {
                    Ok((source_kind, page)) => {
                        let payload = json!({
                            "ref": ref_id.as_str(),
                            "source_kind": source_kind.as_str(),
                            "artifact_id": page.artifact_id().as_str(),
                            "offset": page.offset(),
                            "content": page.content(),
                            "next_offset": page.next_offset(),
                            "total_bytes": page.total_bytes(),
                            "done": page.next_offset().is_none(),
                        });
                        ToolExecutionOutcome::succeeded_json(payload.to_string())
                    }
                    Err(RuntimeError::Checkpoint {
                        source: CheckpointError::RefNotFound { .. },
                    }) => checkpoint_ref_not_found_outcome(&ref_value),
                    Err(error) => checkpoint_ref_read_failed_outcome(&ref_value, &error),
                }
            }
            Err(_) => checkpoint_ref_not_found_outcome(&ref_value),
        },
        Err(reason) => checkpoint_ref_arguments_invalid_outcome(reason),
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
    Ok(events)
}

struct CheckpointRefArguments {
    ref_id: String,
    offset: usize,
    max_bytes: usize,
}

fn checkpoint_ref_arguments(
    pending: &PendingToolCall,
) -> Result<CheckpointRefArguments, &'static str> {
    let arguments = pending.arguments().as_object();
    let offset = optional_page_argument(arguments, "offset", 0)?;
    let max_bytes =
        optional_page_argument(arguments, "max_bytes", DEFAULT_CHECKPOINT_REF_PAGE_BYTES)?;
    if max_bytes == 0 || max_bytes > MAX_CHECKPOINT_REF_PAGE_BYTES {
        return Err("max_bytes must be between 1 and 16384");
    }

    Ok(CheckpointRefArguments {
        ref_id: arguments
            .get("ref")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        offset,
        max_bytes,
    })
}

fn optional_page_argument(
    arguments: &serde_json::Map<String, serde_json::Value>,
    field: &'static str,
    default: usize,
) -> Result<usize, &'static str> {
    let Some(value) = arguments.get(field) else {
        return Ok(default);
    };
    let Some(value) = value.as_u64() else {
        return Err("checkpoint ref page arguments must be non-negative integers");
    };
    usize::try_from(value).map_err(|_| "checkpoint ref page argument is outside platform bounds")
}

#[cfg(test)]
mod argument_tests {
    use super::*;
    use merry_core::{ToolCallArguments, ToolCallId};

    fn pending(arguments: serde_json::Value) -> PendingToolCall {
        PendingToolCall::new(
            ToolCallId::new("checkpoint-ref-argument-test").expect("valid call id"),
            merry_read_checkpoint_ref_tool_name(),
            ToolCallArguments::try_from(arguments).expect("valid JSON arguments"),
        )
    }

    #[test]
    fn present_unparseable_page_arguments_are_rejected_instead_of_defaulted() {
        let invalid_offset = pending(json!({ "ref": "h1", "offset": "0" }));
        let invalid_max_bytes = pending(json!({ "ref": "h1", "max_bytes": "4096" }));

        assert!(checkpoint_ref_arguments(&invalid_offset).is_err());
        assert!(checkpoint_ref_arguments(&invalid_max_bytes).is_err());
    }

    #[test]
    fn absent_page_arguments_use_defaults() {
        let arguments = checkpoint_ref_arguments(&pending(json!({ "ref": "h1" })))
            .expect("missing optional page arguments should use defaults");

        assert_eq!(arguments.offset, 0);
        assert_eq!(arguments.max_bytes, DEFAULT_CHECKPOINT_REF_PAGE_BYTES);
    }

    #[test]
    fn checkpoint_ref_schema_describes_fields_and_matches_runtime_bounds() {
        let tool = merry_read_checkpoint_ref_tool().expect("checkpoint ref tool should build");
        crate::schema_contract::assert_provider_input_schema_fields_have_descriptions(tool.spec());
        let schema = serde_json::to_value(tool.spec().input_schema().as_schema())
            .expect("checkpoint ref schema should serialize");
        assert_eq!(schema["properties"]["offset"]["minimum"], 0);
        assert_eq!(
            schema["properties"]["max_bytes"]["maximum"],
            MAX_CHECKPOINT_REF_PAGE_BYTES
        );
    }
}
