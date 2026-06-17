use merry_core::{PendingToolCall, ToolInputSchema, ToolName, ToolSpec};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, de::DeserializeOwned};

use crate::{
    WORKSPACE_LIST_DIR_TOOL, WORKSPACE_PATCH_TOOL, WORKSPACE_READ_FILE_TOOL,
    WORKSPACE_SEARCH_TEXT_TOOL,
};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadFileArgs {
    pub(crate) path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListDirArgs {
    pub(crate) path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SearchTextArgs {
    pub(crate) query: String,
    #[serde(default)]
    pub(crate) path: Option<String>,
    #[serde(default)]
    pub(crate) max_matches: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkspacePatchArgs {
    pub(crate) patch: String,
}

pub(crate) fn read_file_spec() -> ToolSpec {
    ToolSpec::new(
        ToolName::new(WORKSPACE_READ_FILE_TOOL).expect("static workspace tool name is valid"),
        "Read a UTF-8 file under a configured stable workspace root, rejecting traversal and ordinary symlink paths.",
        ToolInputSchema::new(schema_for!(ReadFileArgs))
            .expect("static workspace_read_file input schema is valid"),
    )
    .expect("static workspace_read_file spec is valid")
}

pub(crate) fn list_dir_spec() -> ToolSpec {
    ToolSpec::new(
        ToolName::new(WORKSPACE_LIST_DIR_TOOL).expect("static workspace tool name is valid"),
        "List one directory under configured stable workspace roots as a non-recursive, stable, memory-bounded, cancellable listing without symlink traversal.",
        ToolInputSchema::new(schema_for!(ListDirArgs))
            .expect("static workspace_list_dir input schema is valid"),
    )
    .expect("static workspace_list_dir spec is valid")
}

pub(crate) fn search_text_spec() -> ToolSpec {
    ToolSpec::new(
        ToolName::new(WORKSPACE_SEARCH_TEXT_TOOL).expect("static workspace tool name is valid"),
        "Search UTF-8 files under configured stable workspace roots with literal, case-sensitive matching and bounded traversal, entry inspection, and scanned bytes.",
        ToolInputSchema::new(schema_for!(SearchTextArgs))
            .expect("static workspace_search_text input schema is valid"),
    )
    .expect("static workspace_search_text spec is valid")
}

pub(crate) fn workspace_patch_spec() -> ToolSpec {
    ToolSpec::new(
        ToolName::new(WORKSPACE_PATCH_TOOL).expect("static workspace tool name is valid"),
        "Apply one Merry workspace patch set to existing UTF-8 files under configured stable workspace roots. The patch string must start with *** Begin Patch or *** Begin Workspace Patch, contain one or more *** Update File: <workspace-relative-path> sections, use hunk lines prefixed with space, +, or -, and end with the matching *** End Patch or *** End Workspace Patch. Prefer the smallest unique hunk context needed for a localized edit; do not submit whole-file content for small edits.",
        ToolInputSchema::new(schema_for!(WorkspacePatchArgs))
            .expect("static workspace_patch input schema is valid"),
    )
    .expect("static workspace_patch spec is valid")
}

pub(crate) fn parse_read_file_args(call: &PendingToolCall) -> Result<ReadFileArgs, String> {
    parse_tool_args(call, WORKSPACE_READ_FILE_TOOL)
}

pub(crate) fn parse_list_dir_args(call: &PendingToolCall) -> Result<ListDirArgs, String> {
    parse_tool_args(call, WORKSPACE_LIST_DIR_TOOL)
}

pub(crate) fn parse_search_text_args(call: &PendingToolCall) -> Result<SearchTextArgs, String> {
    parse_tool_args(call, WORKSPACE_SEARCH_TEXT_TOOL)
}

pub(crate) fn parse_workspace_patch_args(
    call: &PendingToolCall,
) -> Result<WorkspacePatchArgs, String> {
    parse_tool_args(call, WORKSPACE_PATCH_TOOL)
}

fn parse_tool_args<T>(call: &PendingToolCall, tool_name: &'static str) -> Result<T, String>
where
    T: DeserializeOwned,
{
    serde_json::from_value(serde_json::Value::Object(
        call.arguments().as_object().clone(),
    ))
    .map_err(|error| format!("invalid {tool_name} arguments: {error}"))
}
