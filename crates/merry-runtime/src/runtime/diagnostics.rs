use crate::RuntimeError;
use merry_core::ErrorInfo;

pub(super) const DIAGNOSTIC_TOOL_CALL_RESULT_REQUIRED: &str = "tool_call_result_required";
pub(super) const DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED: &str = "action_policy_denied";
pub(super) const DIAGNOSTIC_TOOL_NOT_ADMITTED: &str = "tool_not_admitted";
pub(super) const DIAGNOSTIC_TOOL_NOT_REGISTERED: &str = "tool_not_registered";
pub(super) const TOOL_ACTION_POLICY_DENIED_MESSAGE: &str =
    "tool action was blocked by runtime policy";
pub(super) const WORKSPACE_PATCH_TOOL_NAME: &str = "workspace_patch";

pub(super) fn diagnostic_from_text(code: &'static str, message: impl AsRef<str>) -> ErrorInfo {
    let message = sanitize_diagnostic_message(message.as_ref());
    ErrorInfo::new(code, &message).expect("runtime diagnostic is sanitized and uses static code")
}

pub(super) fn runtime_error_message(error: &RuntimeError) -> String {
    match error {
        RuntimeError::CompactionModelRequest { message }
        | RuntimeError::CompactionModelSetup { message }
        | RuntimeError::CompactionModelStream { message } => message.clone(),
        _ => error.to_string(),
    }
}

fn sanitize_diagnostic_message(message: &str) -> String {
    let sanitized: String = message
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    let trimmed = sanitized.trim();

    let source = if trimmed.is_empty() {
        "provider returned an empty error message"
    } else {
        trimmed
    };

    source.chars().take(4096).collect()
}
