//! Plain-language text for a permission review.

use merry_runtime::{PermissionReviewRequest, RequestedCapability};

/// Lines shown in the review panel, without the yes/no prompt.
pub(crate) fn permission_review_lines(request: &PermissionReviewRequest) -> Vec<String> {
    let permission_request = request.request();
    let mut lines = Vec::new();
    match permission_request.action() {
        merry_runtime::PermissionedAction::Process(intent) => {
            lines.push("Merry wants to run:".to_owned());
            lines.push(String::new());
            lines.push(display_command(intent.argv()));
            lines.push(String::new());
            lines.push(format!(
                "Working directory: {}",
                intent.cwd().unwrap_or(".")
            ));
        }
    }
    if let Some(reason) = request.host_fallback_reason() {
        lines.push(String::new());
        lines.push(reason.to_string());
    }
    lines.push(String::new());
    if permission_request.is_action_review() {
        lines.push(
            "This command can delete or rewrite more than a normal edit. Allow it only if you want this exact command."
                .to_owned(),
        );
    } else if let Some(reason) = permission_request.reason() {
        lines.push(reason.to_owned());
    } else {
        lines.push("Merry is asking before it grants this access.".to_owned());
    }
    if !permission_request.requested().is_empty() {
        lines.push(String::new());
        lines.push("Also requested:".to_owned());
        for capability in permission_request.requested() {
            lines.push(format!("- {}", capability_line(capability)));
        }
    }
    lines
}

/// Headless prompt, including the answer hint.
pub(crate) fn format_permission_prompt(request: &PermissionReviewRequest) -> String {
    let mut lines = permission_review_lines(request);
    lines.push(String::new());
    lines.push("Allow? [y/N] ".to_owned());
    lines.join("\n")
}

fn display_command(argv: &[String]) -> String {
    argv.iter()
        .map(|argument| {
            if argument.is_empty() || argument.chars().any(char::is_whitespace) {
                format!("'{}'", argument.replace('\'', "'\\''"))
            } else {
                argument.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn capability_line(capability: &RequestedCapability) -> String {
    match capability {
        RequestedCapability::Network => "network".to_owned(),
        RequestedCapability::Path(path) => {
            format!("path {} ({})", path.path(), path.access().as_str())
        }
        RequestedCapability::HostIntegration(integration) => {
            format!("host integration {}", integration.as_str())
        }
    }
}
