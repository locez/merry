use crate::{config::WorkspaceToolLimits, schema::with_property_limit};
use merry_core::{PendingToolCall, ToolSpec};
use merry_runtime::ToolBuildError;
use schemars::JsonSchema;
use serde::Deserialize;

#[crate::tool(
    crate = "crate",
    name = "apply_patch",
    description = "Apply one constrained patch to UTF-8 files under configured stable roots. Use *** Add File: <relative-path> with + lines to create a missing file, or *** Update File: <relative-path> with minimal hunk context and lines prefixed with space, +, or -. Add File creates missing parent directories and never overwrites an existing path. Use exactly one patch envelope, keep the patch localized, and do not submit whole-file content for a small edit. The patch payload and resulting writes are bounded by configured limits."
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
    serde_json::from_value(serde_json::Value::Object(
        call.arguments().as_object().clone(),
    ))
    .map_err(|error| format!("invalid apply_patch arguments: {error}"))
}
