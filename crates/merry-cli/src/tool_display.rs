use merry_core::PendingToolCall;
use serde_json::{Map, Value};

pub(crate) fn format_tool_call_progress(prefix: &str, call: &PendingToolCall) -> String {
    let name = call.name().as_str();
    let detail = format_tool_call_detail(name, call.arguments().as_object());
    if name == "request_permissions" {
        return join_progress_parts("permission: request", detail.as_deref());
    }
    join_progress_parts(&format!("{prefix}: {name}"), detail.as_deref())
}

pub(crate) fn format_tool_call_detail(
    name: &str,
    arguments: &Map<String, Value>,
) -> Option<String> {
    match name {
        "workspace_patch" => format_workspace_patch_call_detail(arguments),
        _ => format_generic_tool_call_detail(arguments),
    }
}

fn join_progress_parts(prefix: &str, detail: Option<&str>) -> String {
    match detail {
        Some(detail) if !detail.is_empty() => format!("{prefix} {detail}"),
        _ => prefix.to_owned(),
    }
}

fn format_workspace_patch_call_detail(arguments: &Map<String, Value>) -> Option<String> {
    let patch = arguments.get("patch")?.as_str()?;
    let paths = workspace_patch_paths(patch);
    let bytes = patch.len();
    let detail = match paths.as_slice() {
        [] => format!("patch={bytes} bytes"),
        [path] => format!("patch={} ({bytes} bytes)", compact_inline(path, 80)),
        [first, ..] => format!(
            "patch={} +{} files ({bytes} bytes)",
            compact_inline(first, 80),
            paths.len() - 1
        ),
    };
    Some(detail)
}

fn workspace_patch_paths(patch: &str) -> Vec<&str> {
    patch
        .lines()
        .filter_map(|line| line.strip_prefix("*** Update File: ").map(str::trim))
        .filter(|path| !path.is_empty())
        .collect()
}

fn format_generic_tool_call_detail(arguments: &Map<String, Value>) -> Option<String> {
    let parts = arguments
        .iter()
        .filter_map(|(key, value)| format_inline_value(value).map(|value| format!("{key}={value}")))
        .collect::<Vec<_>>();
    non_empty_parts(parts)
}

fn format_inline_value(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(compact_shell_word(value)),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value)
            .ok()
            .map(|value| compact_inline(&value, 120)),
        Value::Null => Some("null".to_owned()),
    }
}

fn non_empty_parts(parts: Vec<String>) -> Option<String> {
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn compact_shell_word(value: &str) -> String {
    let value = compact_inline(value, 120);
    if value.is_empty() {
        return "\"\"".to_owned();
    }
    if value.bytes().all(is_safe_shell_word_byte) {
        value
    } else {
        format!("{value:?}")
    }
}

fn compact_inline(value: &str, max_chars: usize) -> String {
    let mut output = value
        .chars()
        .filter(|character| !character.is_control())
        .take(max_chars)
        .collect::<String>();
    if value.chars().count() > max_chars {
        output.push_str("...");
    }
    output
}

fn is_safe_shell_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'_' | b'-' | b'.' | b'/' | b':' | b'=' | b'@' | b'%' | b'+'
        )
}

#[cfg(test)]
mod tests {
    use super::format_tool_call_detail;
    use serde_json::json;

    #[test]
    fn workspace_patch_detail_names_patch_path_instead_of_argument_count() {
        let arguments = json!({
            "patch": "\
*** Begin Patch
*** Update File: crates/merry-cli/src/tui/render.rs
 old
-old
+new
*** End Patch"
        });

        let detail =
            format_tool_call_detail("workspace_patch", arguments.as_object().unwrap()).unwrap();

        assert!(detail.starts_with("patch=crates/merry-cli/src/tui/render.rs"));
        assert!(!detail.contains("args=1"));
    }

    #[test]
    fn workspace_patch_detail_reports_malformed_patch_payload_size() {
        let arguments = json!({ "patch": "not a Merry patch" });

        let detail =
            format_tool_call_detail("workspace_patch", arguments.as_object().unwrap()).unwrap();

        assert_eq!(detail, "patch=17 bytes");
    }

    #[test]
    fn generic_tool_detail_lists_every_argument_by_name() {
        let arguments = json!({
            "source": "docs",
            "limit": 2,
            "include_archived": false,
            "filters": {"kind": "guide"}
        });

        let detail = format_tool_call_detail("custom_lookup", arguments.as_object().unwrap());

        assert_eq!(
            detail.as_deref(),
            Some("filters={\"kind\":\"guide\"} include_archived=false limit=2 source=docs")
        );
        assert!(!detail.unwrap().contains("args="));
    }

    #[test]
    fn known_tool_detail_keeps_all_top_level_arguments() {
        let arguments = json!({
            "query": "tool_call",
            "path": "crates/merry-cli",
            "max_matches": 50
        });

        let detail =
            format_tool_call_detail("workspace_search_text", arguments.as_object().unwrap());

        assert_eq!(
            detail.as_deref(),
            Some("max_matches=50 path=crates/merry-cli query=tool_call")
        );
    }
}
