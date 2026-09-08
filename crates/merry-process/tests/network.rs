#![cfg(target_os = "linux")]

use merry_core::{PendingToolCall, ToolCallArguments, ToolCallId, ToolName};
use merry_process::{
    BwrapPermissionedProcessRunnerFactory, LocalProcessBackend, ProcessBackend, ProcessBackendMode,
    ProcessBackendOptions,
};
use merry_runtime::{
    PermissionRequest, PermissionedAction, PermissionedProcessRunnerFactory, ProcessRunner,
    ProcessRunnerContext, parse_permission_request,
};
use serde_json::{Value, json};
use std::{fs, sync::Arc};
use tokio_util::sync::CancellationToken;

fn request(requested: Value, command: &str) -> PermissionRequest {
    parse_permission_request(&PendingToolCall::new(
        ToolCallId::new("network-probe").unwrap(),
        ToolName::new("request_permissions").unwrap(),
        ToolCallArguments::try_from(json!({
            "requested": requested,
            "for_action": {"command": command, "cwd": null}
        }))
        .unwrap(),
    ))
    .unwrap()
}

#[tokio::test]
async fn disabled_network_ceiling_rejects_every_grant_entrypoint_and_child_session() {
    let fixture = tempfile::tempdir().unwrap();
    let workspace = fixture.path().join("workspace");
    let shared = fixture.path().join("shared");
    fs::create_dir(&workspace).unwrap();
    fs::create_dir(&shared).unwrap();
    let backend = LocalProcessBackend::new(
        &workspace,
        ProcessBackendMode::Isolated,
        ProcessBackendOptions::new().with_network_requests_allowed(false),
    )
    .unwrap();
    let standalone: Arc<dyn PermissionedProcessRunnerFactory> = Arc::new(
        BwrapPermissionedProcessRunnerFactory::new_at_workspace_root(&workspace)
            .with_network_requests_allowed(false),
    );
    let network_only = request(
        json!({"network": true}),
        "printf executed > network-executed",
    );
    let combined = request(
        json!({"network": true, "paths": [{"path": shared, "access": "rw"}]}),
        "printf executed > network-executed",
    );
    let path_only = request(json!({"paths": [{"path": shared, "access": "rw"}]}), "true");
    for factory in [
        standalone,
        backend.new_session().permissioned_factory(),
        backend.new_session().permissioned_factory(),
    ] {
        for request in [&network_only, &combined] {
            let error = factory.validate_request(request).unwrap_err();
            assert!(error.to_string().contains("network requests are disabled"));
            assert!(factory.request_capabilities_are_satisfied(request).is_err());
            assert!(factory.prepare_approved_request(request).is_err());
            let PermissionedAction::Process(intent) = request.action();
            let error = factory
                .runner_for(request)
                .run(
                    intent.clone(),
                    ProcessRunnerContext::new(CancellationToken::new()),
                )
                .await
                .unwrap_err();
            assert!(error.to_string().contains("network requests are disabled"));
        }
        factory.validate_request(&path_only).unwrap();
        assert!(
            !factory
                .request_capabilities_are_satisfied(&path_only)
                .unwrap()
        );
        assert!(!workspace.join("network-executed").exists());
    }
}

#[tokio::test]
async fn network_ceiling_never_enables_baseline_network_or_retains_an_action_grant() {
    let fixture = tempfile::tempdir().unwrap();
    let parent_namespace = fs::read_link("/proc/self/ns/net").unwrap();
    let parent_namespace = parent_namespace.to_str().unwrap();
    let request = request(json!({"network": true}), "readlink /proc/self/ns/net");
    for options in [
        ProcessBackendOptions::new(),
        ProcessBackendOptions::new().with_network_requests_allowed(true),
        ProcessBackendOptions::new().with_network_requests_allowed(false),
    ] {
        let allowed = options.network_requests_allowed();
        let backend =
            LocalProcessBackend::new(fixture.path(), ProcessBackendMode::Isolated, options)
                .unwrap();
        for _ in 0..2 {
            let session = backend.new_session();
            let baseline = session.runner();
            assert_ne!(namespace(&*baseline, &request).await, parent_namespace);
            let factory = session.permissioned_factory();
            if allowed {
                factory.validate_request(&request).unwrap();
                assert!(
                    !factory
                        .request_capabilities_are_satisfied(&request)
                        .unwrap()
                );
                let approved = factory.runner_for(&request);
                assert_eq!(namespace(&*approved, &request).await, parent_namespace);
                assert!(
                    !factory
                        .request_capabilities_are_satisfied(&request)
                        .unwrap()
                );
            } else {
                assert!(factory.validate_request(&request).is_err());
            }
            assert_ne!(namespace(&*baseline, &request).await, parent_namespace);
        }
    }
}

async fn namespace(runner: &dyn ProcessRunner, request: &PermissionRequest) -> String {
    let PermissionedAction::Process(intent) = request.action();
    let output = runner
        .run(
            intent.clone(),
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .unwrap();
    assert!(output.ok(), "{output:?}");
    output.stdout_text().trim().to_owned()
}
