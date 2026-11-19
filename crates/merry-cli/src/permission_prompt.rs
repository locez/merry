//! Structured presentation and plain-text formatting for permission reviews.

use merry_runtime::{
    HostFallbackReason, PathAccess, PermissionReviewRequest, PermissionReviewRisk,
    PermissionUserAuthorization, RequestedCapability, shell_command_for_argv,
};

/// One requested capability badge shown in the permission review UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PermissionCapabilityBadge {
    Network,
    Path { path: String, access: PathAccess },
    HostIntegration(String),
}

impl PermissionCapabilityBadge {
    pub(crate) fn chip_label(&self) -> String {
        match self {
            Self::Network => "network".to_owned(),
            Self::Path { path, access } => format!("{} {path}", access.as_str()),
            Self::HostIntegration(name) => name.clone(),
        }
    }

    pub(crate) fn plain_label(&self) -> String {
        match self {
            Self::Network => "network".to_owned(),
            Self::Path { path, access } => format!("path {path} ({})", access.as_str()),
            Self::HostIntegration(name) => format!("host integration {name}"),
        }
    }
}

/// Why a permission request was handed to the human reviewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PermissionFallbackNotice {
    ModelDenied {
        risk: PermissionReviewRisk,
        user_authorization: PermissionUserAuthorization,
        rationale: String,
    },
    ReviewFailed {
        message: String,
    },
    ReviewModelUnavailable,
}

/// Structured view model for one permission review dialog or CLI prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PermissionReviewView {
    pub(crate) approval_id: String,
    pub(crate) command: String,
    pub(crate) cwd: String,
    pub(crate) capabilities: Vec<PermissionCapabilityBadge>,
    pub(crate) review_only: bool,
    pub(crate) agent_reason: Option<String>,
    pub(crate) fallback: Option<PermissionFallbackNotice>,
    pub(crate) queue_total: usize,
    pub(crate) plain_body: Option<String>,
}

impl PermissionReviewView {
    pub(crate) fn from_request(request: &PermissionReviewRequest, queue_total: usize) -> Self {
        let permission_request = request.request();
        let (command, cwd) = match permission_request.action() {
            merry_runtime::PermissionedAction::Process(intent) => (
                shell_command_for_argv(intent.argv()),
                intent.cwd().unwrap_or(".").to_owned(),
            ),
        };
        let capabilities = permission_request
            .requested()
            .iter()
            .map(capability_badge)
            .collect();
        let review_only = permission_request.is_action_review();
        let agent_reason = if review_only {
            Some(
                "This command triggers an action review (git state, recursive delete, shell/privilege, or external path). Allow only if you want this exact command."
                    .to_owned(),
            )
        } else {
            permission_request
                .reason()
                .map(str::trim)
                .filter(|reason| !reason.is_empty())
                .map(str::to_owned)
        };
        let fallback = request.host_fallback_reason().map(|reason| match reason {
            HostFallbackReason::ModelDenied(review) => PermissionFallbackNotice::ModelDenied {
                risk: review.risk(),
                user_authorization: review.user_authorization(),
                rationale: review.rationale().to_owned(),
            },
            HostFallbackReason::ReviewFailed { message } => {
                PermissionFallbackNotice::ReviewFailed {
                    message: message.clone(),
                }
            }
            HostFallbackReason::ReviewModelUnavailable => {
                PermissionFallbackNotice::ReviewModelUnavailable
            }
        });

        Self {
            approval_id: request.approval_id(),
            command,
            cwd,
            capabilities,
            review_only,
            agent_reason,
            fallback,
            queue_total: queue_total.max(1),
            plain_body: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_plain_text(approval_id: String, body: String) -> Self {
        Self {
            approval_id,
            command: String::new(),
            cwd: ".".to_owned(),
            capabilities: Vec::new(),
            review_only: false,
            agent_reason: None,
            fallback: None,
            queue_total: 1,
            plain_body: Some(body),
        }
    }
}

/// Lines shown in the plain-text headless review prompt, without the yes/no line.
pub(crate) fn permission_review_lines(request: &PermissionReviewRequest) -> Vec<String> {
    let view = PermissionReviewView::from_request(request, 1);
    let mut lines = Vec::new();
    lines.push(format!("Command:  {}", view.command));
    lines.push(format!("Cwd:      {}", view.cwd));
    if !view.capabilities.is_empty() {
        let joined = view
            .capabilities
            .iter()
            .map(PermissionCapabilityBadge::plain_label)
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("Access:   {joined}"));
    } else if view.review_only {
        lines.push("Access:   action review (no extra capabilities)".to_owned());
    }
    if let Some(fallback) = &view.fallback {
        lines.push(String::new());
        match fallback {
            PermissionFallbackNotice::ModelDenied {
                risk,
                user_authorization,
                rationale,
            } => {
                lines.push(format!(
                    "AI review: held (risk: {}, auth: {}) — {rationale}",
                    risk.as_str(),
                    user_authorization.as_str()
                ));
            }
            PermissionFallbackNotice::ReviewFailed { message } => {
                lines.push(format!("AI review unavailable: {message}"));
            }
            PermissionFallbackNotice::ReviewModelUnavailable => {
                lines.push(
                    "AI review unavailable: no approval-review model is configured".to_owned(),
                );
            }
        }
    }
    if let Some(reason) = &view.agent_reason {
        lines.push(String::new());
        lines.push(format!("Reason:   {reason}"));
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

fn capability_badge(capability: &RequestedCapability) -> PermissionCapabilityBadge {
    match capability {
        RequestedCapability::Network => PermissionCapabilityBadge::Network,
        RequestedCapability::Path(path) => PermissionCapabilityBadge::Path {
            path: path.path().to_owned(),
            access: path.access(),
        },
        RequestedCapability::HostIntegration(integration) => {
            PermissionCapabilityBadge::HostIntegration(integration.as_str().to_owned())
        }
    }
}
