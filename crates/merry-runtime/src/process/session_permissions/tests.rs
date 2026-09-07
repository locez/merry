use super::*;
use crate::{PathAccessRuleSource, parse_permission_request};
use merry_core::{PendingToolCall, ToolCallArguments, ToolCallId, ToolName};
use serde_json::json;
use std::path::Path;
use tokio_util::sync::CancellationToken;

struct Backend {
    reject: bool,
}

impl PermissionedProcessRunnerFactory for Backend {
    fn validate_request(&self, _request: &PermissionRequest) -> Result<(), ProcessRunnerError> {
        if self.reject {
            Err(ProcessRunnerError::infrastructure(
                "fixture policy rejected request",
            ))
        } else {
            Ok(())
        }
    }

    fn prepare_approved_request(
        &self,
        request: &PermissionRequest,
    ) -> Result<PreparedProcessPermission, ProcessRunnerError> {
        self.validate_request(request)?;
        let grants = request
            .requested()
            .iter()
            .filter_map(|capability| {
                let RequestedCapability::Path(path) = capability else {
                    return None;
                };
                let constraint = if path.path().starts_with("/reviewed") {
                    ProcessPathGrantConstraint::ReviewRequired
                } else if path.path().starts_with("/metadata") {
                    ProcessPathGrantConstraint::ProtectedMetadata
                } else {
                    ProcessPathGrantConstraint::Ordinary
                };
                Some(ProcessPathGrant::new(
                    PathAccessRule::new(
                        Path::new(path.path()),
                        path.access(),
                        PathAccessRuleSource::PermissionReview,
                    ),
                    constraint,
                ))
            })
            .collect();
        Ok(PreparedProcessPermission::new(
            self.runner_for(request),
            grants,
        ))
    }

    fn runner_for(&self, _request: &PermissionRequest) -> Arc<dyn ProcessRunner> {
        Arc::new(RejectedRunner(ProcessRunnerError::infrastructure(
            "unused fixture runner",
        )))
    }
}

fn request(requested: serde_json::Value) -> PermissionRequest {
    parse_permission_request(&PendingToolCall::new(
        ToolCallId::new("permission-fixture").unwrap(),
        ToolName::new("request_permissions").unwrap(),
        ToolCallArguments::try_from(json!({
            "requested": requested,
            "for_action": { "command": "true", "cwd": null }
        }))
        .unwrap(),
    ))
    .unwrap()
}

#[test]
fn runtime_retains_only_ordinary_paths_and_host_integrations() {
    let permissions = ProcessSessionPermissions::new();
    let view = permissions.view();
    let factory = permissions.with_factory(Backend { reject: false });
    let request = request(json!({
        "network": true,
        "host_integrations": ["ssh-agent"],
        "paths": [
            { "path": "/ordinary", "access": "ro" },
            { "path": "/reviewed", "access": "ro" },
            { "path": "/metadata", "access": "rw" }
        ]
    }));
    factory.validate_request(&request).unwrap();
    assert!(
        !factory
            .request_capabilities_are_satisfied(&request)
            .unwrap()
    );
    assert!(view.snapshot().unwrap().path_rules().is_empty());
    let _runner = factory.runner_for(&request);
    let snapshot = view.snapshot().unwrap();
    assert_eq!(snapshot.path_rules().len(), 1);
    assert_eq!(snapshot.path_rules()[0].path(), Path::new("/ordinary"));
    assert_eq!(snapshot.host_integrations(), &[HostIntegration::SshAgent]);
    assert!(
        !factory
            .request_capabilities_are_satisfied(&request)
            .unwrap()
    );
}

#[test]
fn repeated_grants_upgrade_and_coalesce_without_cross_session_leaks() {
    let permissions = ProcessSessionPermissions::new();
    let view = permissions.view();
    let other = ProcessSessionPermissions::new();
    let factory = permissions.with_factory(Backend { reject: false });
    for (path, access) in [
        ("/ordinary/child", "ro"),
        ("/ordinary", "ro"),
        ("/ordinary", "rw"),
        ("/ordinary/child", "rw"),
    ] {
        let request = request(json!({"paths": [{"path": path, "access": access}]}));
        let _runner = factory.runner_for(&request);
    }
    let snapshot = view.snapshot().unwrap();
    assert_eq!(snapshot.path_rules().len(), 1);
    assert_eq!(snapshot.path_rules()[0].path(), Path::new("/ordinary"));
    assert_eq!(snapshot.path_rules()[0].access(), PathAccess::ReadWrite);
    assert!(other.view().snapshot().unwrap().path_rules().is_empty());
}

#[tokio::test]
async fn rejected_preparation_does_not_retain_any_capability() {
    let permissions = ProcessSessionPermissions::new();
    let view = permissions.view();
    let factory = permissions.with_factory(Backend { reject: true });
    let request = request(json!({
        "host_integrations": ["ssh-agent"],
        "paths": [{"path": "/ordinary", "access": "rw"}]
    }));
    let crate::PermissionedAction::Process(intent) = request.action();
    let runner = factory.runner_for(&request);
    let error = runner
        .run(
            intent.clone(),
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("fixture policy rejected request")
    );
    let snapshot = view.snapshot().unwrap();
    assert!(snapshot.path_rules().is_empty());
    assert!(snapshot.host_integrations().is_empty());
}
