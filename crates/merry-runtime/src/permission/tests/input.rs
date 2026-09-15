//! Request parsing, capability normalization, and the `request_permissions`
//! input schema owned by `permission::input`.

use super::call;
use crate::MAX_PROCESS_CWD_BYTES;
use crate::permission::request_json::requested_capabilities_json;
use crate::permission::*;
use serde_json::{Value, json};

#[test]
fn permission_request_parses_process_action_and_network_capability() {
    let request = permission_request_from_call(
        &call(json!({
            "reason": "Need to fetch dependency metadata",
            "requested": { "network": true },
            "for_action": { "command": "cargo test", "cwd": "." }
        })),
        Vec::new(),
    )
    .expect("request should parse");

    assert_eq!(request.reason(), Some("Need to fetch dependency metadata"));
    assert!(matches!(
        request.requested(),
        [RequestedCapability::Network]
    ));
    let PermissionedAction::Process(intent) = request.action();
    assert_eq!(intent.argv(), ["bash", "-lc", "cargo test"]);
    assert_eq!(intent.cwd(), Some("."));
}

#[test]
fn permission_request_parses_named_host_integrations() {
    let request = permission_request_from_call(
        &call(json!({
            "requested": {
                "host_integrations": ["gpg-agent", "dbus", "ssh-agent"]
            },
            "for_action": { "command": "gh auth status", "cwd": null }
        })),
        Vec::new(),
    )
    .expect("host integration request should parse");

    assert_eq!(
        request.requested(),
        &[
            RequestedCapability::HostIntegration(HostIntegration::SshAgent),
            RequestedCapability::HostIntegration(HostIntegration::SessionBus),
            RequestedCapability::HostIntegration(HostIntegration::GpgAgent),
        ]
    );
    let serialized = requested_capabilities_json(request.requested());
    assert_eq!(
        serialized,
        json!({ "host_integrations": ["ssh-agent", "dbus", "gpg-agent"] })
    );
}

#[test]
fn permission_request_accepts_combined_capabilities_for_one_command() {
    let request = permission_request_from_call(
        &call(json!({
            "requested": {
                "network": true,
                "paths": [{ "path": ".config/gh", "access": "ro" }],
                "host_integrations": ["dbus"]
            },
            "for_action": { "command": "gh issue list", "cwd": null }
        })),
        Vec::new(),
    )
    .expect("combined capability request should parse");

    assert_eq!(
        request.requested(),
        &[
            RequestedCapability::Network,
            RequestedCapability::Path(
                RequestedPathCapability::new(".config/gh".to_owned(), PathAccess::ReadOnly)
                    .expect("test path should normalize"),
            ),
            RequestedCapability::HostIntegration(HostIntegration::SessionBus),
        ]
    );
}

#[test]
fn request_permissions_schema_allows_omitted_process_cwd() {
    let tool = request_permissions_tool().expect("permission tool should build");
    let schema = serde_json::to_value(tool.spec().input_schema().as_schema())
        .expect("schema should serialize");

    assert!(
        schema["properties"]["for_action"]["properties"]["cwd"]["description"]
            .as_str()
            .expect("cwd description should be text")
            .contains("Omit it")
    );
    assert!(
        schema["properties"]["for_action"]["properties"]["cwd"]["description"]
            .as_str()
            .expect("cwd description should be text")
            .contains("current workspace directory")
    );
    let cwd_string_schema = schema["properties"]["for_action"]["properties"]["cwd"]["anyOf"]
        .as_array()
        .expect("permission cwd should have nullable branches")
        .iter()
        .find(|branch| branch["type"] == "string")
        .expect("permission cwd should have a string branch");
    assert_eq!(cwd_string_schema["minLength"], 1);
    assert_eq!(cwd_string_schema["maxLength"], MAX_PROCESS_CWD_BYTES);
    assert_eq!(
        schema["properties"]["for_action"]["required"],
        json!(["command"])
    );
}

#[test]
fn request_permissions_schema_describes_nested_request_objects() {
    let tool = request_permissions_tool().expect("permission tool should build");
    crate::schema_contract::assert_provider_input_schema_fields_have_descriptions(tool.spec());
    let schema = serde_json::to_value(tool.spec().input_schema().as_schema())
        .expect("schema should serialize");
    for path in [
        ["properties", "requested", "description"].as_slice(),
        [
            "properties",
            "requested",
            "properties",
            "paths",
            "description",
        ]
        .as_slice(),
        ["properties", "for_action", "description"].as_slice(),
        [
            "properties",
            "for_action",
            "properties",
            "command",
            "description",
        ]
        .as_slice(),
    ] {
        let mut value = &schema;
        for key in path {
            value = &value[*key];
        }
        assert!(
            !value.as_str().unwrap_or_default().is_empty(),
            "missing description at {path:?}"
        );
    }
}

#[test]
fn request_permissions_schema_matches_runtime_bounds() {
    let tool = request_permissions_tool().expect("permission tool should build");
    let schema = tool.spec().input_schema().as_schema().as_value();
    let validator = jsonschema::validator_for(schema).expect("schema should compile");
    let valid = json!({
        "reason": "Need dependency metadata",
        "requested": { "paths": [{ "path": "/tmp/cache", "access": "ro" }] },
        "for_action": {
            "command": "cargo metadata",
            "cwd": "."
        }
    });
    assert!(validator.is_valid(&valid));

    let host_integration_request = json!({
        "requested": { "host_integrations": ["dbus"] },
        "for_action": {
            "command": "gh auth status",
            "cwd": null
        }
    });
    assert!(validator.is_valid(&host_integration_request));

    let mut nullable_optional_fields = valid.clone();
    nullable_optional_fields["reason"] = Value::Null;
    nullable_optional_fields["for_action"]["cwd"] = Value::Null;
    assert!(validator.is_valid(&nullable_optional_fields));

    let mut empty_requested = valid.clone();
    empty_requested["requested"] = json!({});
    assert!(!validator.is_valid(&empty_requested));

    let mut redundant_kind = valid.clone();
    redundant_kind["for_action"]["kind"] = json!("process");
    assert!(!validator.is_valid(&redundant_kind));

    let mut oversized_reason = valid.clone();
    oversized_reason["reason"] = json!("x".repeat(MAX_PERMISSION_REASON_BYTES + 1));
    assert!(!validator.is_valid(&oversized_reason));

    let mut oversized_command = valid.clone();
    oversized_command["for_action"]["command"] =
        json!("x".repeat(crate::MAX_PROCESS_ARG_BYTES + 1));
    assert!(!validator.is_valid(&oversized_command));
}

#[test]
fn permission_request_treats_empty_process_cwd_as_workspace_root() {
    let request = permission_request_from_call(
        &call(json!({
            "reason": "Need DNS lookup",
            "requested": { "network": true },
            "for_action": { "command": "ping -c 1 baidu.com", "cwd": "" }
        })),
        Vec::new(),
    )
    .expect("request should parse");

    let PermissionedAction::Process(intent) = request.action();
    assert_eq!(intent.argv(), ["bash", "-lc", "ping -c 1 baidu.com"]);
    assert_eq!(intent.cwd(), None);
}

#[test]
fn permission_request_rejects_empty_capability_set() {
    let error = permission_request_from_call(
        &call(json!({
            "requested": {},
            "for_action": { "command": "cargo test", "cwd": null }
        })),
        Vec::new(),
    )
    .expect_err("empty capabilities should fail");

    assert!(error.to_string().contains("requested must include"));
}

#[test]
fn requested_paths_are_normalized_and_identical_duplicates_are_collapsed() {
    let request = permission_request_from_call(
        &call(json!({
            "requested": {
                "paths": [
                    { "path": "./cache/../deps", "access": "ro" },
                    { "path": "deps/./", "access": "ro" }
                ]
            },
            "for_action": { "command": "cargo metadata", "cwd": null }
        })),
        Vec::new(),
    )
    .expect("equivalent path requests should be accepted");

    assert_eq!(request.requested().len(), 1);
    let RequestedCapability::Path(path) = &request.requested()[0] else {
        panic!("expected normalized path capability");
    };
    assert_eq!(path.path(), "deps");
}

#[test]
fn requested_paths_reject_traversal_and_conflicting_duplicates() {
    let traversal = permission_request_from_call(
        &call(json!({
            "requested": { "paths": [{ "path": "../secrets", "access": "ro" }] },
            "for_action": { "command": "cat secrets", "cwd": null }
        })),
        Vec::new(),
    )
    .expect_err("relative traversal must be rejected");
    assert!(traversal.to_string().contains("escape the workspace root"));

    let conflict = permission_request_from_call(
        &call(json!({
            "requested": {
                "paths": [
                    { "path": "deps", "access": "ro" },
                    { "path": "./deps", "access": "rw" }
                ]
            },
            "for_action": { "command": "cargo metadata", "cwd": null }
        })),
        Vec::new(),
    )
    .expect_err("conflicting normalized paths must be rejected");
    assert!(conflict.to_string().contains("conflicting access"));
}

#[test]
fn permission_request_defaults_omitted_process_cwd_to_workspace_root() {
    let request = permission_request_from_call(
        &call(json!({
            "reason": "Need the current directory",
            "requested": { "network": true },
            "for_action": { "command": "pwd" }
        })),
        Vec::new(),
    )
    .expect("request should parse");

    let PermissionedAction::Process(intent) = request.action();
    assert_eq!(intent.cwd(), None);
}
