use super::*;

#[test]
fn invalid_attributes_are_rejected() {
    for attributes in [
        "",
        "name = \"x\"",
        "description = \"x\", description = \"y\"",
        "description = \"x\", unknown = \"y\"",
        "description = \"x\", crate = \"not a path\"",
    ] {
        assert!(
            syn::parse_str::<ToolArguments>(attributes).is_err(),
            "invalid attributes accepted: {attributes}"
        );
    }
}

#[test]
fn unsupported_handler_shapes_are_rejected() {
    for source in [
        "fn handler(input: Input) {}",
        "async fn handler() {}",
        "async fn handler(first: Input, second: Input) {}",
        "async fn handler<T>(input: T) {}",
    ] {
        let arguments =
            syn::parse_str::<ToolArguments>("description = \"test\", crate = \"crate\"")
                .expect("valid attributes");
        let function = syn::parse_str::<ItemFn>(source).expect("valid Rust function");
        assert!(
            expand_handler(arguments, function).is_err(),
            "unsupported handler accepted: {source}"
        );
    }
}
