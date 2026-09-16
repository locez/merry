use crate::{config::WorkspaceToolLimits, schema::with_property_limit};
use merry_core::{PendingToolCall, ToolSpec};
use merry_runtime::ToolBuildError;
use schemars::JsonSchema;
use serde::Deserialize;

#[merry_tools_macros::tool(
    crate = "crate",
    name = "apply_patch",
    description = "Apply one constrained patch to UTF-8 files. Send exactly one envelope: `*** Begin Patch` ... `*** End Patch`. Inside it, use the section that matches the intent: `*** Add File: <path>` with every content line prefixed `+` (creates missing parent directories, never overwrites an existing path); `*** Update File: <path>` with hunks started by `@@` and every line prefixed with one space for context, `+` for an added line, or `-` for a removed line; `*** Delete File: <path>` to remove an existing file and leave its parent directory in place. Name each file in at most one Add or Delete section, and repeat `*** Update File:` sections for one file only when that is easier than one section with several hunks, because repeated update sections merge into a single change. Hunks apply in order and each one must match the current file bytes exactly once, so keep context minimal but unique and re-read the file when it may have changed; a hunk with only context lines is dropped unless the envelope has no `+` or `-` line at all, which is rejected. The whole envelope is planned before anything is written, so one unmatched path or hunk writes nothing. A section path is relative to a workspace root or absolute: an absolute path may name a file outside the workspace, where the sandbox decides what is reachable and writable, and every spelling is accepted, including dot-prefixed components. Note the reported path when you need to refer to the file again. The patch payload and resulting writes are bounded by configured limits."
)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApplyPatchInput {
    #[schemars(
        description = "Patch envelope containing one or more Add, Update, or Delete file sections.",
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
