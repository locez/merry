use serde_json::Value;
use std::{borrow::Cow, fmt::Write as _};

/// Parses nested tool-call arguments after recovering raw JSON controls inside
/// string values. Providers sometimes decode the outer response JSON before
/// exposing the nested function arguments, leaving a model-emitted newline or
/// tab as a literal control character in the inner JSON document.
pub(crate) fn parse_tool_arguments(input: &str) -> Result<Value, serde_json::Error> {
    let normalized = escape_string_controls(input);
    serde_json::from_str(normalized.as_ref())
}

fn escape_string_controls(input: &str) -> Cow<'_, str> {
    let mut normalized: Option<String> = None;
    let mut in_string = false;
    let mut escaped = false;

    for (index, character) in input.char_indices() {
        if escaped {
            if let Some(normalized) = normalized.as_mut() {
                normalized.push(character);
            }
            escaped = false;
            continue;
        }

        if in_string && character <= '\u{001f}' {
            let normalized = normalized.get_or_insert_with(|| input[..index].to_owned());
            match character {
                '\u{0008}' => normalized.push_str("\\b"),
                '\t' => normalized.push_str("\\t"),
                '\n' => normalized.push_str("\\n"),
                '\u{000c}' => normalized.push_str("\\f"),
                '\r' => normalized.push_str("\\r"),
                other => write!(normalized, "\\u{:04x}", other as u32)
                    .expect("writing to a String cannot fail"),
            }
            continue;
        }

        if let Some(normalized) = normalized.as_mut() {
            normalized.push(character);
        }
        if in_string && character == '\\' {
            escaped = true;
        } else if character == '"' {
            in_string = !in_string;
        }
    }

    match normalized {
        Some(normalized) => Cow::Owned(normalized),
        None => Cow::Borrowed(input),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_tool_arguments;
    use serde_json::json;

    #[test]
    fn recovers_literal_controls_inside_string_values() {
        let arguments = "{\"command\":\"line one\nline two\tvalue\"}";

        let value = parse_tool_arguments(arguments).expect("raw string controls should recover");

        assert_eq!(value, json!({"command": "line one\nline two\tvalue"}));
    }

    #[test]
    fn preserves_valid_json_escapes_and_rejects_controls_outside_strings() {
        let escaped = parse_tool_arguments(r#"{"command":"line\nvalue"}"#)
            .expect("valid escaped controls should parse");
        assert_eq!(escaped, json!({"command": "line\nvalue"}));

        let error = parse_tool_arguments("{\"command\":\n}")
            .expect_err("controls outside strings must remain invalid");
        assert!(error.to_string().contains("expected value"));
    }
}
