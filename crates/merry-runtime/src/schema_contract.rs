use merry_core::ToolSpec;
use serde_json::Value;
use std::collections::BTreeSet;

/// Asserts that every provider-visible object field carries model-facing help.
pub(crate) fn assert_provider_input_schema_fields_have_descriptions(tool: &ToolSpec) {
    let schema = serde_json::to_value(tool.input_schema().as_schema())
        .expect("tool input schema should serialize");

    fn walk(
        root: &Value,
        schema: &Value,
        tool_name: &str,
        path: &str,
        visited_refs: &mut BTreeSet<String>,
    ) {
        if let Some(reference) = schema.get("$ref").and_then(Value::as_str)
            && visited_refs.insert(reference.to_owned())
        {
            let target = reference
                .strip_prefix('#')
                .and_then(|pointer| root.pointer(pointer))
                .unwrap_or_else(|| panic!("{tool_name} has unresolved schema ref {reference}"));
            walk(root, target, tool_name, path, visited_refs);
        }

        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            for (field, field_schema) in properties {
                // Serde's tagged-enum discriminator is structural syntax, not
                // a user-authored argument that needs model guidance.
                if field == "type" {
                    continue;
                }
                let description = field_schema
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                assert!(
                    !description.trim().is_empty(),
                    "{tool_name} schema field {path}.{field} is missing a description"
                );
                walk(
                    root,
                    field_schema,
                    tool_name,
                    &format!("{path}.{field}"),
                    visited_refs,
                );
            }
        }

        if let Some(items) = schema.get("items") {
            walk(root, items, tool_name, &format!("{path}[]"), visited_refs);
        }

        for keyword in ["oneOf", "anyOf", "allOf"] {
            if let Some(branches) = schema.get(keyword).and_then(Value::as_array) {
                for (index, branch) in branches.iter().enumerate() {
                    walk(
                        root,
                        branch,
                        tool_name,
                        &format!("{path}.{keyword}[{index}]"),
                        visited_refs,
                    );
                }
            }
        }

        if let Some(definitions) = schema.get("$defs").and_then(Value::as_object) {
            for (name, definition) in definitions {
                walk(
                    root,
                    definition,
                    tool_name,
                    &format!("{path}.$defs.{name}"),
                    visited_refs,
                );
            }
        }
    }

    walk(
        &schema,
        &schema,
        tool.name().as_str(),
        "$",
        &mut BTreeSet::new(),
    );
}
