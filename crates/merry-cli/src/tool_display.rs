use merry::events::PendingToolCall;
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
        "apply_patch" => format_apply_patch_call_detail(arguments),
        "run_process" => format_process_call_detail(arguments),
        "enter_plan_mode" => Some("enter read-only mode".to_owned()),
        "write_plan" => format_write_plan_call_detail(arguments),
        "read_plan" => format_read_plan_call_detail(arguments),
        "exit_plan_mode" => format_exit_plan_call_detail(arguments),
        "update_todos" => format_update_todos_call_detail(arguments),
        merry::mcp::MCP_CALL_TOOL => format_mcp_call_detail(arguments),
        merry::mcp::MCP_SEARCH_TOOL => format_mcp_search_detail(arguments),
        _ => format_generic_tool_call_detail(arguments),
    }
}

fn format_mcp_call_detail(arguments: &Map<String, Value>) -> Option<String> {
    let server = arguments.get("server").and_then(Value::as_str)?;
    let tool = arguments.get("tool").and_then(Value::as_str)?;
    let header = format!(
        "{}/{}",
        crate::text::without_control_chars(server),
        crate::text::without_control_chars(tool)
    );
    let inner = arguments
        .get("arguments")
        .and_then(Value::as_object)
        .and_then(format_generic_tool_call_detail);
    match inner {
        Some(detail) if !detail.is_empty() => Some(format!("{header} {detail}")),
        _ => Some(header),
    }
}

fn format_mcp_search_detail(arguments: &Map<String, Value>) -> Option<String> {
    let refresh = arguments
        .get("refresh")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let query = arguments
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|query| !query.is_empty());
    let server = arguments
        .get("server")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|server| !server.is_empty())
        .map(crate::text::without_control_chars);

    let verb = if refresh {
        "refreshing"
    } else if query.is_some() {
        "searching"
    } else {
        "listing"
    };
    let detail = match (server.as_deref(), query) {
        (Some(server), Some(query)) => {
            format!("{verb} {server} · \"{}\"", compact_inline(query, 60))
        }
        (None, Some(query)) => format!("{verb} \"{}\"", compact_inline(query, 60)),
        (Some(server), None) => format!("{verb} {server}"),
        (None, None) => format!("{verb} servers"),
    };
    Some(detail)
}

fn format_write_plan_call_detail(arguments: &Map<String, Value>) -> Option<String> {
    let verb = if arguments
        .get("create_new")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        "new"
    } else if let Some(plan_id) = arguments
        .get("plan_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        return Some(format_write_plan_summary(
            &format!("update {}", compact_inline(plan_id, 40)),
            arguments,
        ));
    } else {
        "write"
    };
    Some(format_write_plan_summary(verb, arguments))
}

fn format_write_plan_summary(verb: &str, arguments: &Map<String, Value>) -> String {
    let title = arguments
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(|title| compact_inline(title, 60));
    let lines = arguments
        .get("content")
        .and_then(Value::as_str)
        .map(|content| content.lines().count());
    match (title, lines) {
        (Some(title), Some(lines)) => format!("{verb} \"{title}\" ({lines} lines)"),
        (Some(title), None) => format!("{verb} \"{title}\""),
        (None, Some(lines)) => format!("{verb} ({lines} lines)"),
        (None, None) => verb.to_owned(),
    }
}

fn format_read_plan_call_detail(arguments: &Map<String, Value>) -> Option<String> {
    match arguments
        .get("plan_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        Some(plan_id) => Some(format!("read {}", compact_inline(plan_id, 40))),
        None => Some("read active plan".to_owned()),
    }
}

fn format_exit_plan_call_detail(arguments: &Map<String, Value>) -> Option<String> {
    match arguments
        .get("plan_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        Some(plan_id) => Some(format!("submit {} for review", compact_inline(plan_id, 40))),
        None => Some("submit for review".to_owned()),
    }
}

fn format_update_todos_call_detail(arguments: &Map<String, Value>) -> Option<String> {
    let merge = arguments
        .get("merge")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let verb = if merge { "update" } else { "set" };
    let count = arguments
        .get("todos")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let noun = if count == 1 { "item" } else { "items" };
    Some(format!("{verb} {count} {noun}"))
}

fn format_process_call_detail(arguments: &Map<String, Value>) -> Option<String> {
    let command = arguments.get("command").and_then(Value::as_str)?;
    let cwd = arguments.get("cwd").and_then(Value::as_str).unwrap_or(".");
    Some(format!(
        "{} ({})",
        display_shell_command(command),
        crate::text::without_control_chars(cwd)
    ))
}

fn join_progress_parts(prefix: &str, detail: Option<&str>) -> String {
    match detail {
        Some(detail) if !detail.is_empty() => format!("{prefix} {detail}"),
        _ => prefix.to_owned(),
    }
}

fn format_apply_patch_call_detail(arguments: &Map<String, Value>) -> Option<String> {
    let patch = arguments.get("patch")?.as_str()?;
    let paths = apply_patch_paths(patch);
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

fn apply_patch_paths(patch: &str) -> Vec<&str> {
    patch
        .lines()
        .filter_map(|line| crate::apply_patch_argument::section_header(line).map(|(_, path)| path))
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

fn display_shell_command(value: &str) -> String {
    let output = crate::text::with_control_chars_as_spaces(value);
    let output = output.trim();
    if output.is_empty() {
        "\"\"".to_owned()
    } else {
        output.to_owned()
    }
}

fn compact_inline(value: &str, max_chars: usize) -> String {
    let value = crate::text::without_control_chars(value);
    let mut output = value.chars().take(max_chars).collect::<String>();
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
    fn apply_patch_detail_names_patch_path_instead_of_argument_count() {
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
            format_tool_call_detail("apply_patch", arguments.as_object().unwrap()).unwrap();

        assert!(detail.starts_with("patch=crates/merry-cli/src/tui/render.rs"));
        assert!(!detail.contains("args=1"));
    }

    #[test]
    fn apply_patch_detail_names_add_file_path() {
        let arguments = json!({
            "patch": "*** Begin Patch\n*** Add File: notes/new.txt\n+hello\n*** End Patch"
        });

        let detail =
            format_tool_call_detail("apply_patch", arguments.as_object().unwrap()).unwrap();

        assert!(detail.starts_with("patch=notes/new.txt"));
    }

    #[test]
    fn apply_patch_detail_names_delete_file_path() {
        let arguments = json!({
            "patch": "*** Begin Patch\n*** Delete File: notes/obsolete.txt\n*** End Patch"
        });

        let detail =
            format_tool_call_detail("apply_patch", arguments.as_object().unwrap()).unwrap();

        assert!(
            detail.starts_with("patch=notes/obsolete.txt"),
            "a delete must name the file it removes, not only the payload size: {detail}"
        );
    }

    #[test]
    fn apply_patch_detail_reports_malformed_patch_payload_size() {
        let arguments = json!({ "patch": "not a Merry patch" });

        let detail =
            format_tool_call_detail("apply_patch", arguments.as_object().unwrap()).unwrap();

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
            "path": "crates/merry-cli",
            "start_line": 10,
            "max_lines": 50
        });

        let detail = format_tool_call_detail("read_file", arguments.as_object().unwrap());

        assert_eq!(
            detail.as_deref(),
            Some("max_lines=50 path=crates/merry-cli start_line=10")
        );
    }

    #[test]
    fn process_detail_shows_command_and_cwd_without_field_labels() {
        let arguments = json!({
            "command": "rg -n 'select!' crates/merry-runtime",
            "cwd": "."
        });

        let detail =
            format_tool_call_detail("run_process", arguments.as_object().unwrap()).unwrap();

        assert_eq!(detail, "rg -n 'select!' crates/merry-runtime (.)");
        assert!(!detail.contains("command="));
        assert!(!detail.contains("cwd="));
    }

    #[test]
    fn process_detail_keeps_long_commands_and_working_directories_complete() {
        let command = format!("printf '%s' '{}'", "完整-command-".repeat(30));
        let cwd = format!("src/{}/workspace", "nested/".repeat(20));
        let arguments = json!({"command": command, "cwd": cwd});

        let detail =
            format_tool_call_detail("run_process", arguments.as_object().unwrap()).unwrap();

        assert_eq!(detail, format!("{command} ({cwd})"));
    }

    #[test]
    fn process_detail_sanitizes_terminal_controls_without_losing_the_command_tail() {
        let command = format!("printf '{}'\n\t&& echo done\r", "x".repeat(150));
        let arguments = json!({"command": command, "cwd": "src\u{1b}"});

        let detail =
            format_tool_call_detail("run_process", arguments.as_object().unwrap()).unwrap();

        assert_eq!(
            detail,
            format!("printf '{}'  && echo done (src)", "x".repeat(150))
        );
        assert!(!detail.chars().any(char::is_control));
    }
}
