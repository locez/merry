use crate::{config::WorkspaceToolLimits, schema::with_property_limit};
use merry_core::{PendingToolCall, ToolSpec};
use merry_runtime::ToolBuildError;
use schemars::JsonSchema;
use serde::Deserialize;

#[merry_tools_macros::tool(
    crate = "crate",
    name = "read_text",
    description = "Read UTF-8 text using one-based lines. Omit start_line to begin at line 1 and omit max_lines to use the configured per-call limit. A successful result contains the complete returned start_line..end_line range; truncated means more lines follow. Continue at end_line + 1 to read the next range, within the configured limits."
)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadTextInput {
    #[schemars(
        description = "UTF-8 text file path to read, relative to a workspace root or absolute. An absolute path may name a file outside the workspace, where the sandbox decides what is reachable. Any spelling is accepted, including dot-prefixed components such as `.github/workflows`.",
        length(min = 1)
    )]
    pub(crate) path: String,
    #[serde(default)]
    #[schemars(
        description = "One-based first line to return. Defaults to 1.",
        range(min = 1)
    )]
    pub(crate) start_line: Option<usize>,
    #[serde(default)]
    #[schemars(
        description = "Maximum number of lines to return. Defaults to the configured read limit.",
        range(min = 1)
    )]
    pub(crate) max_lines: Option<usize>,
}

pub(crate) fn spec(limits: &WorkspaceToolLimits) -> Result<ToolSpec, ToolBuildError> {
    with_property_limit(
        ReadTextInput::tool_spec()?,
        "max_lines",
        "maximum",
        limits.max_read_lines,
    )
}

pub(super) fn parse(call: &PendingToolCall) -> Result<ReadTextInput, String> {
    call.arguments()
        .deserialize_as()
        .map_err(|error| format!("invalid read_text arguments: {error}"))
}
