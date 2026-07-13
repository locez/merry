use serde_json::Value;

pub(super) fn compact_failed_tool_body(code: &str, message: &str, output: &str) -> String {
    let mut lines = vec![format!("{code}: {message}")];
    let Some(value) = serde_json::from_str::<Value>(output).ok() else {
        return lines.join("\n");
    };
    if let Some(violation) = value
        .pointer("/error/violations")
        .and_then(Value::as_array)
        .and_then(|violations| violations.first())
    {
        let path = violation.get("path").and_then(Value::as_str).unwrap_or("$");
        if let Some(message) = violation.get("message").and_then(Value::as_str) {
            lines.push(format!("{path}: {message}"));
        }
    }
    if let Some(guidance) = value.pointer("/guidance/message").and_then(Value::as_str) {
        lines.push(guidance.to_owned());
    }
    if let Some(instruction) = value
        .pointer("/retry/instruction")
        .or_else(|| value.pointer("/recovery/instruction"))
        .and_then(Value::as_str)
    {
        lines.push(instruction.to_owned());
    }
    lines.join("\n")
}
