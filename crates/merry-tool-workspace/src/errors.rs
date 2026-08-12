use merry_core::ErrorInfo;
use merry_runtime::{ToolExecutionError, ToolExecutionOutcome};
use serde::Serialize;

pub(crate) const ERROR_INVALID_ARGUMENTS: &str = "workspace_invalid_arguments";
pub(crate) const ERROR_PATH_DENIED: &str = "workspace_path_denied";
pub(crate) const ERROR_FILE_NOT_FOUND: &str = "workspace_file_not_found";
pub(crate) const ERROR_FILE_ALREADY_EXISTS: &str = "workspace_file_already_exists";
pub(crate) const ERROR_PATH_NOT_FOUND: &str = "workspace_path_not_found";
pub(crate) const ERROR_NOT_FILE: &str = "workspace_path_not_file";
pub(crate) const ERROR_NOT_DIRECTORY: &str = "workspace_path_not_directory";
pub(crate) const ERROR_NOT_SEARCHABLE: &str = "workspace_path_not_searchable";
pub(crate) const ERROR_FILE_TOO_LARGE: &str = "workspace_file_too_large";
pub(crate) const ERROR_NOT_UTF8: &str = "workspace_file_not_utf8";
pub(crate) const ERROR_READ_FAILED: &str = "workspace_read_failed";
pub(crate) const ERROR_WRITE_FAILED: &str = "workspace_write_failed";
pub(crate) const ERROR_PROPOSAL_MISMATCH: &str = "workspace_patch_approved_mismatch";
pub(crate) const WORKSPACE_PATCH_PLAN_CHANGED_MESSAGE: &str =
    "workspace patch plan changed before execution";
pub(crate) const ERROR_PREIMAGE_ABSENT: &str = "workspace_patch_preimage_absent";
pub(crate) const ERROR_PREIMAGE_AMBIGUOUS: &str = "workspace_patch_preimage_ambiguous";

pub(crate) const WORKSPACE_PATH_CONTRACT: &str = "workspace tool path values are relative to a configured workspace root; do not prefix them with a process cwd, repository root, or absolute host path";

const GUIDANCE_INVALID_ARGUMENTS: &str = "Fix the workspace tool arguments before retrying. Use the tool schema exactly; path fields must be workspace-relative and must not include host absolute paths, process cwd prefixes, or parent traversal.";
const GUIDANCE_PATH_RECOVERY: &str = "Use workspace-relative paths from the configured root. If the target is unclear, list or search from \".\" to find the current relative path before retrying.";
const GUIDANCE_FILE_TOO_LARGE: &str = "Do not assume omitted content or rejected patch content is irrelevant. Narrow the target, split the change, use workspace_search_text for discovery, or use an authorized process command for an exact range when needed.";
pub(crate) const GUIDANCE_LIST_TRUNCATED: &str = "The directory listing was truncated. Narrow the path, list a child directory, or search for a specific filename before drawing conclusions from the returned entries.";
const GUIDANCE_SEARCH_TRUNCATED: &str = "The search result was truncated or skipped oversized files. Narrow the query/path, inspect specific files, or use an authorized process command for targeted ranges before drawing conclusions.";
const GUIDANCE_PATCH_PREIMAGE: &str = "Re-read the target file, then retry with a smaller unique preimage that matches the current file exactly. Do not guess file state from an old observation.";
const GUIDANCE_PATCH_PLAN_CHANGED: &str = "The approved patch no longer matches current workspace state. Re-read the target file and submit a fresh localized patch.";

#[derive(Debug)]
pub(crate) struct DomainError {
    pub(crate) code: &'static str,
    pub(crate) message: &'static str,
}

impl DomainError {
    pub(crate) fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }
}

#[derive(Debug)]
pub(crate) enum BlockingToolError {
    Domain(DomainError),
    Cancelled,
}

impl From<DomainError> for BlockingToolError {
    fn from(error: DomainError) -> Self {
        Self::Domain(error)
    }
}

pub(crate) fn blocking_tool_error_into_execution(error: BlockingToolError) -> ToolExecutionError {
    match error {
        BlockingToolError::Domain(error) => ToolExecutionError::infrastructure(error.message),
        BlockingToolError::Cancelled => ToolExecutionError::Cancelled,
    }
}

#[derive(Debug)]
pub(crate) struct PathValidationError {
    pub(crate) code: &'static str,
    pub(crate) message: &'static str,
    pub(crate) path: Option<String>,
}

impl PathValidationError {
    pub(crate) fn new(code: &'static str, message: &'static str, path: Option<String>) -> Self {
        Self {
            code,
            message,
            path,
        }
    }
}

#[derive(Debug, Serialize)]
struct FailureEnvelope<'a> {
    ok: bool,
    tool: &'static str,
    error: FailureError<'a>,
    recovery: FailureRecovery,
    #[serde(skip_serializing_if = "Option::is_none")]
    guidance: Option<WorkspaceGuidance>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct WorkspaceGuidance {
    pub(crate) kind: &'static str,
    pub(crate) message: &'static str,
}

#[derive(Debug, Serialize)]
struct FailureError<'a> {
    code: &'a str,
    message: &'a str,
}

#[derive(Debug, Serialize)]
struct FailureRecovery {
    path_contract: &'static str,
}

pub(crate) fn failed_outcome(
    tool: &'static str,
    code: &'static str,
    message: impl Into<String>,
    path: Option<String>,
) -> ToolExecutionOutcome {
    let message = message.into();
    let envelope = FailureEnvelope {
        ok: false,
        tool,
        error: FailureError {
            code,
            message: &message,
        },
        recovery: FailureRecovery {
            path_contract: WORKSPACE_PATH_CONTRACT,
        },
        guidance: workspace_failure_guidance(code),
        path: path.as_deref(),
    };
    ToolExecutionOutcome::failed_json(
        serde_json::to_string(&envelope).expect("workspace failure envelope serializes"),
        ErrorInfo::new(code, &message).expect("workspace diagnostic is valid"),
    )
}

fn workspace_failure_guidance(code: &str) -> Option<WorkspaceGuidance> {
    match code {
        ERROR_INVALID_ARGUMENTS => Some(WorkspaceGuidance {
            kind: "workspace_invalid_arguments",
            message: GUIDANCE_INVALID_ARGUMENTS,
        }),
        ERROR_PATH_DENIED
        | ERROR_FILE_NOT_FOUND
        | ERROR_FILE_ALREADY_EXISTS
        | ERROR_PATH_NOT_FOUND
        | ERROR_NOT_FILE
        | ERROR_NOT_DIRECTORY
        | ERROR_NOT_SEARCHABLE => Some(WorkspaceGuidance {
            kind: "workspace_path_recovery",
            message: GUIDANCE_PATH_RECOVERY,
        }),
        ERROR_FILE_TOO_LARGE => Some(WorkspaceGuidance {
            kind: "workspace_file_too_large",
            message: GUIDANCE_FILE_TOO_LARGE,
        }),
        ERROR_PREIMAGE_ABSENT | ERROR_PREIMAGE_AMBIGUOUS => Some(WorkspaceGuidance {
            kind: "workspace_patch_preimage_mismatch",
            message: GUIDANCE_PATCH_PREIMAGE,
        }),
        ERROR_PROPOSAL_MISMATCH => Some(WorkspaceGuidance {
            kind: "workspace_patch_plan_changed",
            message: GUIDANCE_PATCH_PLAN_CHANGED,
        }),
        _ => None,
    }
}

pub(crate) fn workspace_search_success_guidance(
    truncated: bool,
    too_large_skipped: usize,
) -> Option<WorkspaceGuidance> {
    (truncated || too_large_skipped > 0).then_some(WorkspaceGuidance {
        kind: "workspace_search_limited",
        message: GUIDANCE_SEARCH_TRUNCATED,
    })
}
