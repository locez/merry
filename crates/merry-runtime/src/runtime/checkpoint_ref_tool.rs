use crate::{
    CheckpointError, CheckpointRefId, RuntimeError, ToolExecutionError, ToolExecutor,
    ToolExecutorFuture,
    tool::{RegisteredTool, ToolActionKind, ToolExecutionContext, ToolExecutionOutcome},
};
use merry_core::{CoreError, ErrorInfo, PendingToolCall, RuntimeJournalEvent, ToolName};
use merry_tools_macros::tool;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

use super::RuntimeInner;

pub(super) const MERRY_READ_CHECKPOINT_REF_TOOL_NAME: &str = "merry_read_checkpoint_ref";
const CHECKPOINT_REF_NOT_FOUND: &str = "checkpoint_ref_not_found";
const CHECKPOINT_REF_READ_FAILED: &str = "checkpoint_ref_read_failed";
const CHECKPOINT_REF_ARGUMENTS_INVALID: &str = "checkpoint_ref_arguments_invalid";
const DEFAULT_CHECKPOINT_REF_PAGE_BYTES: usize = 4096;
const MAX_CHECKPOINT_REF_PAGE_BYTES: usize = 16_384;

#[tool(
    crate = "crate",
    name = "merry_read_checkpoint_ref",
    description = "Read a bounded page from a ref's original artifact in the current compacted checkpoint."
)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CheckpointRefInput {
    #[serde(rename = "ref")]
    #[schemars(
        description = "Checkpoint ref id from the current compacted checkpoint, such as h42."
    )]
    reference: String,
    #[serde(default)]
    #[schemars(
        description = "Zero-based byte offset within the referenced artifact. Omit it to start at the beginning.",
        range(min = 0)
    )]
    offset: usize,
    #[serde(default = "default_checkpoint_ref_page_bytes")]
    #[schemars(
        description = "Maximum number of artifact bytes to return in this page. Omit it to use the 4096-byte default.",
        range(min = 1, max = MAX_CHECKPOINT_REF_PAGE_BYTES)
    )]
    max_bytes: usize,
}

fn default_checkpoint_ref_page_bytes() -> usize {
    DEFAULT_CHECKPOINT_REF_PAGE_BYTES
}

pub(super) fn merry_read_checkpoint_ref_tool_name() -> ToolName {
    ToolName::new(MERRY_READ_CHECKPOINT_REF_TOOL_NAME).expect("static tool name is valid")
}

pub(super) fn is_merry_read_checkpoint_ref_tool(tool_name: &ToolName) -> bool {
    tool_name.as_str() == MERRY_READ_CHECKPOINT_REF_TOOL_NAME
}

pub(super) fn merry_read_checkpoint_ref_tool() -> Result<RegisteredTool, CoreError> {
    let spec = CheckpointRefInput::tool_spec_with(
        merry_read_checkpoint_ref_tool_name().as_str(),
        "Read a bounded page from a ref's original artifact in the current compacted checkpoint.",
    )?;
    Ok(RegisteredTool::new(
        spec,
        Arc::new(MerryReadCheckpointRefExecutor),
        ToolActionKind::ReadOnly,
    )
    .with_parallel_safe_execution())
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
        Ok(input) => match CheckpointRefId::new(&input.reference) {
            Ok(ref_id) => {
                let page = {
                    let session = inner.session.lock().await;
                    session.read_checkpoint_ref_page_with_source(
                        &ref_id,
                        input.offset,
                        input.max_bytes,
                    )
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
                    }) => checkpoint_ref_not_found_outcome(&input.reference),
                    Err(error) => checkpoint_ref_read_failed_outcome(&input.reference, &error),
                }
            }
            Err(_) => checkpoint_ref_not_found_outcome(&input.reference),
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

fn checkpoint_ref_arguments(pending: &PendingToolCall) -> Result<CheckpointRefInput, &'static str> {
    let input = pending
        .arguments()
        .deserialize_as::<CheckpointRefInput>()
        .map_err(|_| "checkpoint ref arguments must match the declared input schema")?;
    if input.max_bytes == 0 || input.max_bytes > MAX_CHECKPOINT_REF_PAGE_BYTES {
        return Err("max_bytes must be between 1 and 16384");
    }
    Ok(input)
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
