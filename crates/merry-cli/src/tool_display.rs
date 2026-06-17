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
        "run_process" => format_process_call_detail(arguments),
        "request_permissions" => format_permission_call_detail(arguments),
        _ => format_generic_tool_call_detail(arguments),
    }
}

fn join_progress_parts(prefix: &str, detail: Option<&str>) -> String {
    match detail {
        Some(detail) if !detail.is_empty() => format!("{prefix} {detail}"),
        _ => prefix.to_owned(),
    }
}

fn format_process_call_detail(arguments: &Map<String, Value>) -> Option<String> {
    let argv = arguments.get("argv")?.as_array()?;
    let mut detail = format_argv(argv)?;
    if let Some(cwd) = arguments
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
    {
        detail.push_str(" (cwd: ");
        detail.push_str(&compact_inline(cwd, 80));
        detail.push(')');
    }
    Some(detail)
}

fn format_permission_call_detail(arguments: &Map<String, Value>) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(requested) = arguments.get("requested").and_then(Value::as_object) {
        if let Some(network) = requested.get("network").and_then(Value::as_bool) {
            parts.push(format!("network={network}"));
        }
        if let Some(paths) = requested.get("paths").and_then(Value::as_array) {
            let paths = format_permission_paths(paths);
            if !paths.is_empty() {
                parts.push(format!("paths={paths}"));
            }
        }
    }
    if let Some(argv) = arguments
        .get("for_action")
        .and_then(Value::as_object)
        .and_then(|action| action.get("argv"))
        .and_then(Value::as_array)
        .and_then(|argv| format_argv(argv))
    {
        parts.push(format!("for: {argv}"));
    }
    non_empty_parts(parts)
}

fn format_permission_paths(paths: &[Value]) -> String {
    let mut formatted = paths
        .iter()
        .take(3)
        .filter_map(|path| {
            let object = path.as_object()?;
            let path = object.get("path")?.as_str()?;
            let access = object.get("access").and_then(Value::as_str);
            Some(match access {
                Some(access) => {
                    format!("{}:{access}", compact_inline(path, 80))
                }
                None => compact_inline(path, 80),
            })
        })
        .collect::<Vec<_>>();
    if paths.len() > formatted.len() {
        formatted.push(format!("+{}", paths.len() - formatted.len()));
    }
    formatted.join(",")
}

fn format_generic_tool_call_detail(arguments: &Map<String, Value>) -> Option<String> {
    let mut parts = Vec::new();
    for key in ["path", "cwd", "query", "pattern", "target", "command"] {
        if let Some(value) = arguments.get(key).and_then(format_inline_value) {
            parts.push(format!("{key}={value}"));
        }
    }
    if parts.is_empty() && !arguments.is_empty() {
        parts.push(format!("args={}", arguments.len()));
    }
    non_empty_parts(parts)
}

fn format_argv(argv: &[Value]) -> Option<String> {
    let words = argv
        .iter()
        .filter_map(Value::as_str)
        .map(compact_shell_word)
        .collect::<Vec<_>>();
    non_empty_parts(words)
}

fn format_inline_value(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(compact_shell_word(value)),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        Value::Array(values) => Some(format!("[{} items]", values.len())),
        Value::Object(values) => Some(format!("{{{} fields}}", values.len())),
        Value::Null => None,
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
