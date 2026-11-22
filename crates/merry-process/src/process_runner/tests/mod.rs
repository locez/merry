#[cfg(any(target_os = "linux", target_os = "macos"))]
use merry_core::{PendingToolCall, ToolCallArguments, ToolCallId, ToolName};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use merry_runtime::{PermissionRequest, PermissionedAction, ProcessActionIntent, ProcessEnvPolicy};

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use execution::fixture_root;

mod execution;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod path_review;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod permissions;

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn intent(cwd: Option<&str>) -> ProcessActionIntent {
    ProcessActionIntent::new(
        vec!["pwd".to_owned()],
        cwd.map(str::to_owned),
        ProcessEnvPolicy::empty(),
        None,
        1024,
        1024,
    )
    .expect("process intent should be valid")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn permission_request(arguments: serde_json::Value) -> PermissionRequest {
    let call = PendingToolCall::new(
        ToolCallId::new("call-permission").expect("valid call id"),
        ToolName::new("request_permissions").expect("valid tool name"),
        ToolCallArguments::try_from(arguments).expect("valid tool arguments"),
    );
    merry_runtime::parse_permission_request(&call).expect("permission request should parse")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn request_process_intent(request: &PermissionRequest) -> &ProcessActionIntent {
    let PermissionedAction::Process(intent) = request.action();
    intent
}
