use crate::{config::WorkspaceToolLimits, schema::with_property_limit};
use merry_core::{PendingToolCall, ToolSpec};
use merry_runtime::ToolBuildError;
use schemars::JsonSchema;
use serde::Deserialize;

#[merry_tools_macros::tool(
    crate = "crate",
    name = "apply_patch",
    description = "Apply one constrained patch of workspace-relative UTF-8 files. Send exactly one envelope: `*** Begin Patch` ... `*** End Patch`. Inside it, name each file at most once and use the section that matches the intent: `*** Add File: <path>` with every content line prefixed `+` (creates missing parent directories, never overwrites an existing path); `*** Update File: <path>` with hunks started by `@@` and every line prefixed with one space for context, `+` for an added line, or `-` for a removed line; `*** Delete File: <path>` to remove an existing file and leave its parent directory in place. Merge edits to distant regions of one file into that file's single Update section instead of repeating the section. Hunks apply in order and each one must match the current file bytes exactly once, so keep context minimal but unique and re-read the file when it may have changed; a hunk with only context lines is dropped unless the envelope has no `+` or `-` line at all, which is rejected. The whole envelope is planned before anything is written, so one unmatched path or hunk writes nothing. Paths are workspace-relative. The patch payload and resulting writes are bounded by configured limits."
)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApplyPatchInput {
    #[schemars(
        description = "Patch envelope containing one or more workspace-relative file update sections.",
        length(min = 1)
    )]
    pub(crate) patch: String,
}

pub(crate) fn spec(limits: &WorkspaceToolLimits) -> Result<ToolSpec, ToolBuildError> {
    with_property_limit(
        ApplyPatchInput::tool_spec()?,
        "patch",
        "maxLength",
        limits.max_patch_bytes,
    )
}

pub(super) fn parse(call: &PendingToolCall) -> Result<ApplyPatchInput, String> {
    call.arguments()
        .deserialize_as()
        .map_err(|error| format!("invalid apply_patch arguments: {error}"))
}
