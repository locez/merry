use std::convert::Infallible;
use schemars::JsonSchema;
use serde::Deserialize;

type HandlerResult<T> = Result<T, Infallible>;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Input {
    /// The value supplied by the caller.
    value: String,
}

#[sdk::tool(description = "Echo the supplied value.")]
async fn echo(input: Input) -> HandlerResult<String> {
    Ok(input.value)
}

#[sdk::tool(crate = "sdk", name = "explicit", description = "Use an explicit API path.")]
async fn explicit_path(input: Input) -> HandlerResult<String> {
    Ok(input.value)
}

#[sdk::tool(description = "Support raw Rust identifiers.")]
async fn r#type(input: Input) -> HandlerResult<String> {
    Ok(input.value)
}

fn main() -> Result<(), sdk::ToolBuildError> {
    let tool = echo_tool()?;
    assert_eq!(tool.spec().name().as_str(), "echo");
    assert_eq!(tool.spec().input_schema().as_schema().as_value()["properties"]["value"]["description"].as_str(), Some("The value supplied by the caller."));
    assert_eq!(explicit_path_tool()?.spec().name().as_str(), "explicit");
    assert_eq!(type_tool()?.spec().name().as_str(), "type");
    Ok(())
}
