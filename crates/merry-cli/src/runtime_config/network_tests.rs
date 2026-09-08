use super::action_process_backend_options;
use crate::{
    config::{MerryConfig, XdgPaths},
    runtime_events::{collect_runtime_step_events, first_pending_tool_call},
    testing::{ScriptedProvider, model_name, tool_call},
};
use merry_core::{RuntimeJournalPayload, SessionId, ToolCallResultStatus};
use merry_process::{LocalProcessBackend, ProcessBackend, ProcessBackendMode};
use merry_runtime::{
    PermissionAdmissionContext, PermissionAdmissionDecision, PermissionAdmissionFuture,
    PermissionAdmissionSource, PermissionRequest, PermissionReviewMode, Runtime, StepContext,
    StepInput, ToolExecutionContext, request_permissions_tool,
};
use serde_json::json;
use std::{
    fs,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Default)]
struct ApprovingReview {
    calls: AtomicUsize,
}

impl ApprovingReview {
    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl PermissionAdmissionSource for ApprovingReview {
    fn review<'a>(
        &'a self,
        _request: PermissionRequest,
        _context: PermissionAdmissionContext,
    ) -> PermissionAdmissionFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Ok(PermissionAdmissionDecision::approved(
                "approve the test action",
            ))
        })
    }
}

#[tokio::test]
async fn network_config_is_a_ceiling_not_preauthorization() {
    for (config_text, allowed, mode) in [
        (None, true, PermissionReviewMode::HostDecisionOnly),
        (Some(""), true, PermissionReviewMode::HostDecisionOnly),
        (
            Some("[permissions]\nnetwork = true\n"),
            true,
            PermissionReviewMode::HostDecisionOnly,
        ),
        (
            Some("[permissions]\nnetwork = false\n"),
            false,
            PermissionReviewMode::HostDecisionOnly,
        ),
        (
            Some("[permissions]\nnetwork = false\n"),
            false,
            PermissionReviewMode::Required,
        ),
        (
            Some("[permissions]\nnetwork = false\n"),
            false,
            PermissionReviewMode::FullyTrusted,
        ),
    ] {
        let fixture = tempfile::tempdir().unwrap();
        let workspace = fixture.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let paths = XdgPaths::from_parts(fixture.path().join("home"), None, None);
        let config = MerryConfig::load_optional_from_text(config_text, &paths).unwrap();
        let options = action_process_backend_options(config.as_ref()).unwrap();
        assert_eq!(options.network_requests_allowed(), allowed);
        let backend =
            LocalProcessBackend::new(&workspace, ProcessBackendMode::Isolated, options).unwrap();
        let provider = ScriptedProvider::new(
            (0..2)
                .map(|index| {
                    vec![Ok(tool_call(
                        &format!("network-{index}"),
                        "request_permissions",
                        json!({
                            "requested": {"network": true},
                            "for_action": {
                                "command": "printf x >> network-executed",
                                "cwd": null
                            }
                        })
                        .as_object()
                        .unwrap()
                        .clone(),
                    )
                    .unwrap())]
                })
                .collect(),
        );
        let review = Arc::new(ApprovingReview::default());
        let runtime = Runtime::builder(SessionId::new("network-ceiling").unwrap())
            .model_provider(Arc::new(provider), model_name())
            .register_tool(request_permissions_tool().unwrap())
            .permissioned_process_runner_factory(backend.new_session().permissioned_factory())
            .permission_review_mode(mode)
            .permission_admission_source(review.clone())
            .build()
            .unwrap();
        for completed in 1..=2 {
            let events = collect_runtime_step_events(
                &runtime,
                StepInput::user_text("Run the network probe after permission admission.").unwrap(),
                StepContext::default(),
            )
            .await
            .unwrap();
            let pending = first_pending_tool_call(&events).unwrap();
            let events = runtime
                .execute_tool_call(pending.id(), ToolExecutionContext::default())
                .await
                .unwrap();
            let result = events
                .iter()
                .find_map(|event| match &event.payload {
                    RuntimeJournalPayload::ToolCallResolved { result } => Some(result),
                    _ => None,
                })
                .unwrap();
            if allowed {
                assert_eq!(result.status(), ToolCallResultStatus::Succeeded);
                assert_eq!(review.call_count(), completed);
                assert_eq!(
                    fs::read(workspace.join("network-executed")).unwrap().len(),
                    completed
                );
            } else {
                assert_eq!(result.status(), ToolCallResultStatus::Failed);
                assert_eq!(
                    result.diagnostic().unwrap().code(),
                    "permission_request_blocked"
                );
                assert_eq!(review.call_count(), 0);
                assert!(!workspace.join("network-executed").exists());
            }
        }
    }
}
