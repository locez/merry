use std::convert::Infallible;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, JsonSchema)]
struct MacroToolInput {
    order_id: String,
}

#[derive(Debug, Serialize)]
struct MacroToolOutput {
    status: String,
}

#[merry::tool(
    name = "macro_definition",
    description = "Build a provider-neutral specification from a typed input definition."
)]
#[derive(Debug, Deserialize, JsonSchema)]
struct MacroToolDefinition {
    #[expect(dead_code)]
    value: String,
}

#[merry::tool(
    name = "macro_lookup_order",
    description = "Look up an order through a typed Rust handler."
)]
async fn lookup_order(input: MacroToolInput) -> Result<MacroToolOutput, Infallible> {
    Ok(MacroToolOutput {
        status: format!("order {} is ready", input.order_id),
    })
}

#[merry::tool(description = "Build a tool using the handler name by default.")]
async fn default_named(input: MacroToolInput) -> Result<MacroToolOutput, Infallible> {
    Ok(MacroToolOutput {
        status: format!("default {}", input.order_id),
    })
}

#[test]
fn tool_attribute_generates_a_typed_tool_factory() {
    let tool = lookup_order_tool().expect("macro-generated tool should build");

    assert_eq!(tool.spec().name().as_str(), "macro_lookup_order");
    assert_eq!(
        tool.spec().description(),
        "Look up an order through a typed Rust handler."
    );
    assert_eq!(
        tool.spec()
            .input_schema()
            .as_schema()
            .as_value()
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .and_then(|properties| properties.get("order_id"))
            .and_then(serde_json::Value::as_object)
            .and_then(|order_id| order_id.get("type"))
            .and_then(serde_json::Value::as_str),
        Some("string")
    );
}

#[test]
fn tool_attribute_defaults_name_to_handler_name() {
    let tool = default_named_tool().expect("default-named tool should build");

    assert_eq!(tool.spec().name().as_str(), "default_named");
}

#[test]
fn tool_attribute_generates_a_definition_spec() {
    let spec = MacroToolDefinition::tool_spec().expect("macro-generated spec should build");

    assert_eq!(spec.name().as_str(), "macro_definition");
    assert_eq!(
        spec.description(),
        "Build a provider-neutral specification from a typed input definition."
    );
    assert_eq!(
        spec.input_schema()
            .as_schema()
            .as_value()
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .and_then(|properties| properties.get("value"))
            .and_then(serde_json::Value::as_object)
            .and_then(|value| value.get("type"))
            .and_then(serde_json::Value::as_str),
        Some("string")
    );
}

#[test]
fn tool_attribute_generates_an_overridable_definition_spec() {
    let spec = MacroToolDefinition::tool_spec_with(
        "macro_definition_override",
        "Override the provider-visible tool identity.",
    )
    .expect("macro-generated overridable spec should build");

    assert_eq!(spec.name().as_str(), "macro_definition_override");
    assert_eq!(
        spec.description(),
        "Override the provider-visible tool identity."
    );
}
