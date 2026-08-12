use merry_core::{PendingToolCall, ToolInputSchema, ToolName, ToolSpec};
use schemars::{JsonSchema, Schema, schema_for};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;

use crate::{
    WORKSPACE_LIST_DIR_TOOL, WORKSPACE_PATCH_TOOL, WORKSPACE_READ_FILE_TOOL,
    WORKSPACE_SEARCH_TEXT_TOOL, config::WorkspaceToolLimits,
};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadFileArgs {
    #[schemars(
        description = "Workspace-relative UTF-8 file path to read. Do not use host-absolute paths or parent traversal.",
        length(min = 1)
    )]
    pub(crate) path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListDirArgs {
    #[schemars(
        description = "Workspace-relative directory path to list. Use \".\" for the workspace root; do not use host-absolute paths or parent traversal.",
        length(min = 1)
    )]
    pub(crate) path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SearchTextArgs {
    #[schemars(
        description = "Case-sensitive Rust regular expression to search for in bounded UTF-8 workspace files.",
        length(min = 1)
    )]
    pub(crate) query: String,
    #[serde(default)]
    #[schemars(
        description = "Optional workspace-relative directory or file path to search. Omit it to search the configured workspace roots."
    )]
    pub(crate) path: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional maximum number of matching lines to return. Omit it to use the configured tool limit."
    )]
    pub(crate) max_matches: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkspacePatchArgs {
    #[schemars(
        description = "Patch envelope containing one or more workspace-relative file update sections.",
        length(min = 1)
    )]
    pub(crate) patch: String,
}

pub(crate) fn read_file_spec() -> ToolSpec {
    ToolSpec::new(
        ToolName::new(WORKSPACE_READ_FILE_TOOL).expect("static workspace tool name is valid"),
        "Read the complete UTF-8 file under a configured stable workspace root, bounded by the configured file-size limit, while rejecting traversal and ordinary symlink paths. This tool accepts only path; it has no offset or max_bytes arguments.",
        ToolInputSchema::new(schema_for!(ReadFileArgs))
            .expect("static workspace_read_file input schema is valid"),
    )
    .expect("static workspace_read_file spec is valid")
}

pub(crate) fn list_dir_spec() -> ToolSpec {
    ToolSpec::new(
        ToolName::new(WORKSPACE_LIST_DIR_TOOL).expect("static workspace tool name is valid"),
        "List one directory under configured stable workspace roots as a non-recursive, stable, memory-bounded, cancellable listing capped by the configured entry limit and without symlink traversal.",
        ToolInputSchema::new(schema_for!(ListDirArgs))
            .expect("static workspace_list_dir input schema is valid"),
    )
    .expect("static workspace_list_dir spec is valid")
}

#[cfg(test)]
pub(crate) fn search_text_spec() -> ToolSpec {
    search_text_spec_with_limits(&WorkspaceToolLimits::default())
}

pub(crate) fn search_text_spec_with_limits(limits: &WorkspaceToolLimits) -> ToolSpec {
    let mut schema = schema_value::<SearchTextArgs>();
    set_property_constraint(
        &mut schema,
        "query",
        "string",
        "maxLength",
        limits.max_search_query_bytes,
    );
    set_property_constraint(&mut schema, "path", "string", "minLength", 1);
    set_property_constraint(&mut schema, "max_matches", "integer", "minimum", 1);
    set_property_constraint(
        &mut schema,
        "max_matches",
        "integer",
        "maximum",
        limits.max_search_matches,
    );
    ToolSpec::new(
        ToolName::new(WORKSPACE_SEARCH_TEXT_TOOL).expect("static workspace tool name is valid"),
        "Search UTF-8 files under configured stable workspace roots with a case-sensitive Rust regular expression. Combine alternatives in one query, for example `(foo|bar|baz)`. Escape regex metacharacters when literal matching is required. Query bytes, inspected entries/files, returned matches, scanned bytes, and matched-line bytes are bounded by the configured limits.",
        input_schema(schema),
    )
    .expect("static workspace_search_text spec is valid")
}

#[cfg(test)]
pub(crate) fn workspace_patch_spec() -> ToolSpec {
    workspace_patch_spec_with_limits(&WorkspaceToolLimits::default())
}

pub(crate) fn workspace_patch_spec_with_limits(limits: &WorkspaceToolLimits) -> ToolSpec {
    let mut schema = schema_value::<WorkspacePatchArgs>();
    set_property_constraint(
        &mut schema,
        "patch",
        "string",
        "maxLength",
        limits.max_patch_bytes,
    );
    ToolSpec::new(
        ToolName::new(WORKSPACE_PATCH_TOOL).expect("static workspace tool name is valid"),
        "Apply one Merry workspace patch set to UTF-8 files under configured stable workspace roots. Use *** Add File: <workspace-relative-path> followed by one or more + lines to create a new file, or *** Update File: <workspace-relative-path> with hunk lines prefixed with space, +, or - to edit an existing file. Add File creates missing parent directories, does not overwrite an existing path, and ignores bare blank formatting lines; use a + prefix for an intentional blank line in the new file. The patch payload and resulting file writes are bounded by configured byte limits. The patch string must start with *** Begin Patch or *** Begin Workspace Patch and end with the matching *** End Patch or *** End Workspace Patch. Prefer the smallest unique hunk context needed for a localized edit; do not submit whole-file content for small edits.",
        input_schema(schema),
    )
    .expect("static workspace_patch spec is valid")
}

fn schema_value<T: JsonSchema>() -> Value {
    serde_json::to_value(schema_for!(T)).expect("workspace input schema should serialize")
}

fn input_schema(value: Value) -> ToolInputSchema {
    ToolInputSchema::new(Schema::try_from(value).expect("workspace input schema is valid"))
        .expect("workspace input schema should be an object")
}

fn set_property_constraint(
    schema: &mut Value,
    property: &str,
    schema_type: &str,
    keyword: &str,
    value: usize,
) {
    let Some(field) = schema
        .get_mut("properties")
        .and_then(Value::as_object_mut)
        .and_then(|properties| properties.get_mut(property))
    else {
        panic!("workspace schema property {property} is missing");
    };
    set_constraint_for_type(field, schema_type, keyword, value);
}

fn set_constraint_for_type(schema: &mut Value, schema_type: &str, keyword: &str, value: usize) {
    let matches_type = schema
        .get("type")
        .and_then(|value| match value {
            Value::String(value) => Some(value == schema_type),
            Value::Array(values) => Some(
                values
                    .iter()
                    .any(|value| value.as_str() == Some(schema_type)),
            ),
            _ => None,
        })
        .unwrap_or(false);
    if matches_type {
        schema[keyword] = Value::from(value);
    }
    for branch_keyword in ["anyOf", "oneOf", "allOf"] {
        if let Some(branches) = schema.get_mut(branch_keyword).and_then(Value::as_array_mut) {
            for branch in branches {
                set_constraint_for_type(branch, schema_type, keyword, value);
            }
        }
    }
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
