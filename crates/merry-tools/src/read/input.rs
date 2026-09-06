use crate::{config::WorkspaceToolLimits, schema::with_property_limit};
use merry_core::{PendingToolCall, ToolSpec};
use merry_runtime::ToolBuildError;
use schemars::JsonSchema;
use serde::Deserialize;

#[crate::tool(
    crate = "crate",
    name = "read_text",
    description = "Read a bounded one-based line range from a UTF-8 text file under a configured stable root. Omit start_line to begin at line 1 and omit max_lines to use the configured limit. Use multiple focused reads for larger files; do not request or assume complete-file content."
)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadTextInput {
    #[schemars(
        description = "Workspace-relative UTF-8 text file path to read. Do not use host-absolute paths or parent traversal.",
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
    serde_json::from_value(serde_json::Value::Object(
        call.arguments().as_object().clone(),
    ))
    .map_err(|error| format!("invalid read_text arguments: {error}"))
}
