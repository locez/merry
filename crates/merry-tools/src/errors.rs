use merry_core::ErrorInfo;
use merry_runtime::ToolExecutionOutcome;
use serde::Serialize;

pub(crate) const ERROR_INVALID_ARGUMENTS: &str = "workspace_invalid_arguments";
pub(crate) const ERROR_PATH_DENIED: &str = "workspace_path_denied";
pub(crate) const ERROR_FILE_NOT_FOUND: &str = "workspace_file_not_found";
pub(crate) const ERROR_FILE_ALREADY_EXISTS: &str = "workspace_file_already_exists";
pub(crate) const ERROR_NOT_FILE: &str = "workspace_path_not_file";
pub(crate) const ERROR_NOT_DIRECTORY: &str = "workspace_path_not_directory";
pub(crate) const ERROR_FILE_TOO_LARGE: &str = "workspace_file_too_large";
pub(crate) const ERROR_NOT_UTF8: &str = "workspace_file_not_utf8";
pub(crate) const ERROR_READ_FAILED: &str = "workspace_read_failed";
pub(crate) const ERROR_WRITE_FAILED: &str = "workspace_write_failed";
pub(crate) const ERROR_PROPOSAL_MISMATCH: &str = "apply_patch_approved_mismatch";
pub(crate) const WORKSPACE_PATCH_PLAN_CHANGED_MESSAGE: &str =
    "workspace patch plan changed before execution";
pub(crate) const ERROR_PATCH_SYNTAX: &str = "apply_patch_syntax";
pub(crate) const ERROR_PATCH_NOOP: &str = "apply_patch_noop";
pub(crate) const ERROR_PREIMAGE_ABSENT: &str = "apply_patch_preimage_absent";
pub(crate) const ERROR_PREIMAGE_AMBIGUOUS: &str = "apply_patch_preimage_ambiguous";

pub(crate) const WORKSPACE_PATH_CONTRACT: &str = "workspace tool path values are resolved against a configured workspace root when they are relative and used as named when they are absolute: an absolute path inside a root and the matching relative path address the same file, an absolute path may also address a file outside the workspace, every spelling including dot-prefixed components is accepted, and only the reachability the sandbox grants decides whether the path can be used";

const MAX_FAILURE_DIAGNOSTIC_CHARS: usize = 512;

const GUIDANCE_INVALID_ARGUMENTS: &str = "Fix the workspace tool arguments before retrying. Use the tool schema exactly; a path field names a file either relative to a workspace root or as an absolute path, and no path shape is rejected.";
const GUIDANCE_PATH_RECOVERY: &str = "Use the path that names the target file: relative to a configured workspace root or absolute, including a file outside the workspace, where the sandbox decides what is reachable and writable. If the target is unclear, use `run_process` for focused discovery when available, or ask for the exact path before retrying.";
const GUIDANCE_FILE_TOO_LARGE: &str = "Do not assume omitted content or rejected patch content is irrelevant. Narrow the read or patch range, split the change, use `read_text` for focused ranges, or use an authorized process command for exact inspection when available.";
const GUIDANCE_PATCH_PREIMAGE: &str = "Re-read the target file at the reported lines, then retry with a smaller unique preimage that matches the current bytes exactly. Do not guess file state from an old observation.";
const GUIDANCE_PATCH_SYNTAX: &str = "Fix the patch text itself: send exactly one `*** Begin Patch` ... `*** End Patch` envelope, at most one `*** Add File:` or `*** Delete File:` section per file, and prefix every hunk line with one space, `+`, or `-`. Repeated `*** Update File:` sections for one file merge into a single change. Use `read_text` for the exact current lines instead of guessing them.";
const GUIDANCE_PATCH_NOOP: &str = "The patch had no `+` or `-` lines, so nothing was written. Add the added or removed lines to the hunk when you mean to edit the file, or use `read_text` when you only need to inspect it.";
const GUIDANCE_PATCH_PLAN_CHANGED: &str = "The approved patch no longer matches current workspace state. Re-read the target file and submit a fresh localized patch.";

/// A failure raised by a workspace tool operation, with a stable code.
///
/// The message is owned because self-locating diagnostics embed bounded
/// previews of the patch text and the current file content. Text that never
/// depends on input stays a `&'static str` in [`PathValidationError`], and every
/// message that reaches a caller passes through [`failure_diagnostic`], which
/// sanitizes text that cannot be represented as a diagnostic.
#[derive(Debug)]
pub(crate) struct DomainError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl DomainError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery: Option<FailureRecovery>,
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
    let class = FailureClass::of(code);
    let envelope = FailureEnvelope {
        ok: false,
        tool,
        error: FailureError {
            code,
            message: &message,
        },
        recovery: class.includes_path_contract().then_some(FailureRecovery {
            path_contract: WORKSPACE_PATH_CONTRACT,
        }),
        guidance: class.guidance(),
        path: path.as_deref(),
    };
    ToolExecutionOutcome::failed_json(
        serde_json::to_string(&envelope).expect("workspace failure envelope serializes"),
        failure_diagnostic(code, &message),
    )
}

/// Model-facing recovery class of a workspace failure code.
///
/// One classification decides both whether the workspace path contract is
/// repeated and which guidance the caller receives, so a new failure code
/// cannot end up with guidance that contradicts the recovery block next to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureClass {
    /// The arguments are unusable before any path or file is considered.
    InvalidArguments,
    /// The patch text does not follow the patch grammar.
    PatchSyntax,
    /// The patch is well formed but would change nothing.
    PatchNoOp,
    /// The patch is valid but its preimage no longer matches the file.
    PatchPreimage,
    /// The approved patch no longer matches current workspace state.
    PatchPlanChanged,
    /// The named path is denied, missing, or the wrong kind of entry.
    PathRecovery,
    /// Requested content or the resulting file exceeds a configured limit.
    FileTooLarge,
    /// A failure with no recovery text of its own.
    Other,
}

impl FailureClass {
    /// Classifies a failure code for model-facing recovery text.
    fn of(code: &str) -> Self {
        match code {
            ERROR_INVALID_ARGUMENTS => Self::InvalidArguments,
            ERROR_PATCH_SYNTAX => Self::PatchSyntax,
            ERROR_PATCH_NOOP => Self::PatchNoOp,
            ERROR_PREIMAGE_ABSENT | ERROR_PREIMAGE_AMBIGUOUS => Self::PatchPreimage,
            ERROR_PROPOSAL_MISMATCH => Self::PatchPlanChanged,
            ERROR_PATH_DENIED
            | ERROR_FILE_NOT_FOUND
            | ERROR_FILE_ALREADY_EXISTS
            | ERROR_NOT_FILE
            | ERROR_NOT_DIRECTORY => Self::PathRecovery,
            ERROR_FILE_TOO_LARGE => Self::FileTooLarge,
            _ => Self::Other,
        }
    }

    /// Reports whether the workspace path contract helps explain this failure.
    ///
    /// Patch-text and preimage failures are about the patch body rather than
    /// about where a path points, so repeating the path contract there misleads
    /// the caller. A patch that names an unwritable or missing path keeps its
    /// own path code and the contract.
    fn includes_path_contract(self) -> bool {
        !matches!(
            self,
            Self::PatchSyntax | Self::PatchNoOp | Self::PatchPreimage
        )
    }

    /// Returns the guidance a caller can act on, when the class has one.
    fn guidance(self) -> Option<WorkspaceGuidance> {
        match self {
            Self::InvalidArguments => Some(WorkspaceGuidance {
                kind: "workspace_invalid_arguments",
                message: GUIDANCE_INVALID_ARGUMENTS,
            }),
            Self::PatchSyntax => Some(WorkspaceGuidance {
                kind: "apply_patch_syntax",
                message: GUIDANCE_PATCH_SYNTAX,
            }),
            Self::PatchNoOp => Some(WorkspaceGuidance {
                kind: "apply_patch_noop",
                message: GUIDANCE_PATCH_NOOP,
            }),
            Self::PatchPreimage => Some(WorkspaceGuidance {
                kind: "apply_patch_preimage_mismatch",
                message: GUIDANCE_PATCH_PREIMAGE,
            }),
            Self::PatchPlanChanged => Some(WorkspaceGuidance {
                kind: "apply_patch_plan_changed",
                message: GUIDANCE_PATCH_PLAN_CHANGED,
            }),
            Self::PathRecovery => Some(WorkspaceGuidance {
                kind: "workspace_path_recovery",
                message: GUIDANCE_PATH_RECOVERY,
            }),
            Self::FileTooLarge => Some(WorkspaceGuidance {
                kind: "workspace_file_too_large",
                message: GUIDANCE_FILE_TOO_LARGE,
            }),
            Self::Other => None,
        }
    }
}

/// Builds a validated diagnostic without aborting the tool call on bad text.
///
/// Failure messages combine fixed guidance with previews of patch and file
/// text, so an unexpected control character or an over-long message must degrade
/// into a sanitized diagnostic instead of panicking mid-call.
fn failure_diagnostic(code: &str, message: &str) -> ErrorInfo {
    if let Ok(diagnostic) = ErrorInfo::new(code, message) {
        return diagnostic;
    }

    let sanitized = message
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_FAILURE_DIAGNOSTIC_CHARS)
        .collect::<String>();
    ErrorInfo::new(code, &sanitized).unwrap_or_else(|_| {
        ErrorInfo::new(code, "workspace tool failure")
            .expect("fallback workspace diagnostic is always valid")
    })
}
