use merry_core::{ErrorInfo, ToolCallResultStatus};
use merry_runtime::{ToolExecutionError, ToolExecutionOutcome};
#[cfg(test)]
use std::{
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::errors::{ERROR_INVALID_ARGUMENTS, failed_outcome};

pub(crate) const TRACE_PATH_MAX_CHARS: usize = 96;

#[cfg(test)]
static PATCH_TEST_AFTER_WRITE_HOOK: OnceLock<Mutex<Option<PatchTestAfterWriteHook>>> =
    OnceLock::new();
#[cfg(test)]
static TRACE_START_TEST_HOOK: OnceLock<Mutex<Option<TraceStartTestHook>>> = OnceLock::new();

#[cfg(test)]
#[derive(Debug)]
struct PatchTestAfterWriteHook {
    root: PathBuf,
    hook: fn(&Path),
    consumed: AtomicBool,
}

#[cfg(test)]
#[derive(Debug)]
struct TraceStartTestHook {
    tool_call_id: String,
    hook: fn(),
    consumed: AtomicBool,
}

#[cfg(test)]
impl PatchTestAfterWriteHook {
    fn new(root: PathBuf, hook: fn(&Path)) -> Self {
        Self {
            root,
            hook,
            consumed: AtomicBool::new(false),
        }
    }
}

#[cfg(test)]
impl TraceStartTestHook {
    fn new(tool_call_id: String, hook: fn()) -> Self {
        Self {
            tool_call_id,
            hook,
            consumed: AtomicBool::new(false),
        }
    }
}

#[cfg(test)]
pub(crate) fn install_patch_test_after_write_hook(root: PathBuf, hook: fn(&Path)) {
    PATCH_TEST_AFTER_WRITE_HOOK
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("patch test hook mutex should not be poisoned")
        .replace(PatchTestAfterWriteHook::new(root, hook));
}

#[cfg(test)]
pub(crate) fn install_trace_start_test_hook(tool_call_id: &str, hook: fn()) {
    TRACE_START_TEST_HOOK
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("trace start hook mutex should not be poisoned")
        .replace(TraceStartTestHook::new(tool_call_id.to_owned(), hook));
}

#[cfg(test)]
pub(crate) fn maybe_run_patch_test_after_write_hook(root: &Path) {
    let Some(hook_slot) = PATCH_TEST_AFTER_WRITE_HOOK.get() else {
        return;
    };
    let hook_guard = hook_slot
        .lock()
        .expect("patch test hook mutex should not be poisoned");
    let Some(hook) = hook_guard.as_ref() else {
        return;
    };
    if root != hook.root {
        return;
    }
    if hook.consumed.swap(true, Ordering::SeqCst) {
        return;
    }
    (hook.hook)(&hook.root);
}

#[cfg(test)]
pub(crate) fn maybe_run_trace_start_test_hook(tool_call_id: &str) {
    let Some(hook_slot) = TRACE_START_TEST_HOOK.get() else {
        return;
    };
    let hook_guard = hook_slot
        .lock()
        .expect("trace start hook mutex should not be poisoned");
    let Some(hook) = hook_guard.as_ref() else {
        return;
    };
    if hook.tool_call_id != tool_call_id {
        return;
    }
    if hook.consumed.swap(true, Ordering::SeqCst) {
        return;
    }
    (hook.hook)();
}

pub(crate) enum WorkspaceTraceTarget<'a> {
    ToolOnly,
    Path(&'a str),
    Search {
        path: Option<&'a str>,
        query_bytes: usize,
    },
    Patch {
        patch_bytes: usize,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceTracePath {
    summary: String,
}

impl WorkspaceTracePath {
    pub(crate) fn new(path: &str) -> Self {
        Self {
            summary: bounded_trace_text(path, TRACE_PATH_MAX_CHARS),
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.summary
    }
}

impl AsRef<str> for WorkspaceTracePath {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

#[derive(Clone, Copy)]
pub(crate) enum WorkspaceTraceFinish<'a> {
    Outcome(&'a ToolExecutionOutcome),
    Cancelled,
    Error(&'static str),
}

impl<'a> WorkspaceTraceFinish<'a> {
    pub(crate) fn from_error(error: &ToolExecutionError) -> Self {
        match error {
            ToolExecutionError::Cancelled => Self::Cancelled,
            ToolExecutionError::Infrastructure { .. } => {
                Self::Error("workspace_infrastructure_error")
            }
        }
    }

    fn status(self) -> &'static str {
        match self {
            Self::Outcome(outcome) => tool_outcome_status(outcome),
            Self::Cancelled => "cancelled",
            Self::Error(_) => "error",
        }
    }

    fn diagnostic_code(self) -> Option<&'a str> {
        match self {
            Self::Outcome(outcome) => outcome.diagnostic().map(ErrorInfo::code),
            Self::Cancelled => Some("workspace_tool_cancelled"),
            Self::Error(code) => Some(code),
        }
    }

    fn output_bytes(self) -> Option<usize> {
        match self {
            Self::Outcome(outcome) => Some(outcome.content().as_bytes().len()),
            Self::Cancelled | Self::Error(_) => None,
        }
    }
}

pub(crate) fn trace_workspace_tool_start(
    tool_name: &'static str,
    tool_call_id: &str,
    target: WorkspaceTraceTarget<'_>,
) {
    match target {
        WorkspaceTraceTarget::ToolOnly => {
            tracing::info!(
                event = "runtime.workspace_tool.start",
                tool_name,
                tool_call_id,
                "workspace tool start"
            );
        }
        WorkspaceTraceTarget::Path(path) => {
            tracing::info!(
                event = "runtime.workspace_tool.start",
                tool_name,
                tool_call_id,
                path,
                "workspace tool start"
            );
        }
        WorkspaceTraceTarget::Search { path, query_bytes } => {
            tracing::info!(
                event = "runtime.workspace_tool.start",
                tool_name,
                tool_call_id,
                path,
                query_bytes,
                "workspace tool start"
            );
        }
        WorkspaceTraceTarget::Patch { patch_bytes } => {
            tracing::info!(
                event = "runtime.workspace_tool.start",
                tool_name,
                tool_call_id,
                patch_bytes,
                "workspace tool start"
            );
        }
    }
    #[cfg(test)]
    maybe_run_trace_start_test_hook(tool_call_id);
}

pub(crate) fn trace_workspace_tool_finish(
    tool_name: &'static str,
    tool_call_id: &str,
    target: WorkspaceTraceTarget<'_>,
    finish: WorkspaceTraceFinish<'_>,
) {
    let status = finish.status();
    let diagnostic_code = finish.diagnostic_code();
    let output_bytes = finish.output_bytes();
    match target {
        WorkspaceTraceTarget::ToolOnly => {
            tracing::info!(
                event = "runtime.workspace_tool.finish",
                tool_name,
                tool_call_id,
                status,
                diagnostic_code,
                output_bytes,
                "workspace tool finish"
            );
        }
        WorkspaceTraceTarget::Path(path) => {
            tracing::info!(
                event = "runtime.workspace_tool.finish",
                tool_name,
                tool_call_id,
                path,
                status,
                diagnostic_code,
                output_bytes,
                "workspace tool finish"
            );
        }
        WorkspaceTraceTarget::Search { path, query_bytes } => {
            tracing::info!(
                event = "runtime.workspace_tool.finish",
                tool_name,
                tool_call_id,
                path,
                query_bytes,
                status,
                diagnostic_code,
                output_bytes,
                "workspace tool finish"
            );
        }
        WorkspaceTraceTarget::Patch { patch_bytes } => {
            tracing::info!(
                event = "runtime.workspace_tool.finish",
                tool_name,
                tool_call_id,
                patch_bytes,
                status,
                diagnostic_code,
                output_bytes,
                "workspace tool finish"
            );
        }
    }
}

fn tool_outcome_status(outcome: &ToolExecutionOutcome) -> &'static str {
    match outcome.status() {
        ToolCallResultStatus::Succeeded => "succeeded",
        ToolCallResultStatus::Failed => "failed",
    }
}

pub(crate) fn invalid_arguments_outcome(
    tool_name: &'static str,
    tool_call_id: &str,
    message: String,
) -> ToolExecutionOutcome {
    let outcome = failed_outcome(tool_name, ERROR_INVALID_ARGUMENTS, message, None::<String>);
    trace_workspace_tool_start(tool_name, tool_call_id, WorkspaceTraceTarget::ToolOnly);
    trace_workspace_tool_finish(
        tool_name,
        tool_call_id,
        WorkspaceTraceTarget::ToolOnly,
        WorkspaceTraceFinish::Outcome(&outcome),
    );
    outcome
}

pub(crate) fn bounded_trace_text(value: &str, max_chars: usize) -> String {
    let mut output = String::new();
    let mut truncated = false;
    for character in value.chars().take(max_chars) {
        output.push(character);
    }
    if value.chars().count() > max_chars {
        truncated = true;
    }
    if truncated {
        output.push_str("...");
    }
    output
}
