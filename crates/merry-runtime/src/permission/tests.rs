use super::review::{parse_permission_review_model_output, requested_capabilities_json};
use super::*;
use crate::MAX_PROCESS_CWD_BYTES;
use merry_core::{ToolCallArguments, ToolCallId};
use serde_json::{Value, json};
use std::sync::Arc;

fn call(arguments: Value) -> PendingToolCall {
    PendingToolCall::new(
        ToolCallId::new("call-permission").expect("valid id"),
        ToolName::new("request_permissions").expect("valid tool name"),
        ToolCallArguments::try_from(arguments).expect("valid arguments"),
    )
}

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
fn model_review_parser_maps_approve_and_deny() {
    let approved = parse_permission_review_model_output(
            r#"{"schema_version":"permission_review.v1","decision":"approve","risk":"low","user_authorization":"high","rationale":"Task explicitly asks for it."}"#,
        )
        .expect("approve parses");
    assert!(approved.is_approved());

    let denied = parse_permission_review_model_output(
            r#"{"schema_version":"permission_review.v1","decision":"deny","risk":"high","user_authorization":"unknown","rationale":"No user authorization."}"#,
        )
        .expect("deny parses");
    assert!(!denied.is_approved());
}

#[test]
fn model_review_does_not_auto_approve_inconsistent_risk_or_authorization() {
    let decision = parse_permission_review_model_output(
            r#"{"schema_version":"permission_review.v1","decision":"approve","risk":"high","user_authorization":"unknown","rationale":"The command may be useful."}"#,
        )
        .expect("inconsistent approval should become a structured denial");

    assert!(!decision.is_approved());
    assert!(
        decision
            .review()
            .rationale()
            .contains("not internally consistent")
    );
}

#[tokio::test]
async fn channel_human_review_waits_for_a_correlated_typed_response() {
    let (source, mut requests) = ChannelPermissionAdmissionSource::channel(1);
    let source = Arc::new(source);
    let request = permission_request_from_call(
        &call(json!({
            "requested": { "network": true },
            "for_action": { "command": "cargo test", "cwd": null }
        })),
        Vec::new(),
    )
    .expect("request should parse");
    let approval_id = request.approval_id();
    let fingerprint = request.fingerprint();
    let token = CancellationToken::new();
    let source_for_task = Arc::clone(&source);
    let task = tokio::spawn(async move {
        source_for_task
            .review(
                request,
                PermissionAdmissionContext::new(token)
                    .with_review_failure("approval provider was unavailable"),
            )
            .await
    });

    let pending = requests
        .recv()
        .await
        .expect("host should receive review request");
    assert_eq!(pending.approval_id(), approval_id);
    assert_eq!(pending.fingerprint(), fingerprint);
    assert_eq!(
        pending.review_failure(),
        Some("approval provider was unavailable")
    );
    pending
        .respond(PermissionReviewResponse::allow(
            approval_id,
            fingerprint,
            "Host confirmed the exact command.",
        ))
        .expect("typed response should be delivered");

    let decision = task
        .await
        .expect("review task should join")
        .expect("review should resolve");
    assert!(decision.is_approved());
    assert_eq!(
        decision.review().rationale(),
        "Host confirmed the exact command."
    );
}

#[tokio::test]
async fn channel_human_review_rejects_stale_response_identity() {
    let (source, mut requests) = ChannelPermissionAdmissionSource::channel(1);
    let source = Arc::new(source);
    let request = permission_request_from_call(
        &call(json!({
            "requested": { "network": true },
            "for_action": { "command": "cargo test", "cwd": null }
        })),
        Vec::new(),
    )
    .expect("request should parse");
    let token = CancellationToken::new();
    let source_for_task = Arc::clone(&source);
    let task = tokio::spawn(async move {
        source_for_task
            .review(request, PermissionAdmissionContext::new(token))
            .await
    });
    let pending = requests
        .recv()
        .await
        .expect("host should receive review request");
    pending
        .respond(PermissionReviewResponse::allow(
            "stale-approval",
            "stale-fingerprint",
            "This must not grant the request.",
        ))
        .expect("stale response should still reach runtime validation");

    let error = task
        .await
        .expect("review task should join")
        .expect_err("stale response must be rejected");
    assert!(matches!(
        error,
        PermissionAdmissionError::StaleReviewResponse { .. }
    ));
}

#[tokio::test]
async fn channel_human_review_marks_queued_request_cancelled() {
    let (source, mut requests) = ChannelPermissionAdmissionSource::channel(1);
    let source = Arc::new(source);
    let request = permission_request_from_call(
        &call(json!({
            "requested": { "network": true },
            "for_action": { "command": "cargo test", "cwd": null }
        })),
        Vec::new(),
    )
    .expect("request should parse");
    let token = CancellationToken::new();
    let task_token = token.clone();
    let source_for_task = Arc::clone(&source);
    let task = tokio::spawn(async move {
        source_for_task
            .review(request, PermissionAdmissionContext::new(task_token))
            .await
    });
    let pending = requests
        .recv()
        .await
        .expect("host should receive review request");
    assert!(!pending.is_cancelled());
    token.cancel();
    assert!(pending.is_cancelled());

    let error = task
        .await
        .expect("review task should join")
        .expect_err("cancelled review must not remain pending");
    assert!(matches!(error, PermissionAdmissionError::Cancelled));
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
