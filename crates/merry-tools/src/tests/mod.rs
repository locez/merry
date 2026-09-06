use super::*;
use crate::{
    errors::{
        ERROR_FILE_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_FILE_TOO_LARGE,
        ERROR_INVALID_ARGUMENTS, ERROR_NOT_DIRECTORY, ERROR_NOT_FILE, ERROR_NOT_UTF8,
        ERROR_PATH_DENIED, ERROR_PREIMAGE_ABSENT, ERROR_PREIMAGE_AMBIGUOUS,
        ERROR_PROPOSAL_MISMATCH, WORKSPACE_PATCH_PLAN_CHANGED_MESSAGE, WORKSPACE_PATH_CONTRACT,
    },
    patch::{
        ApplyPatchExecutor, ApplyPatchInput, apply_patch_blocking, apply_patch_blocking_checked,
        propose_apply_patch_blocking_checked, stable_content_fingerprint,
    },
    path::validate_relative_path,
    read::{ReadTextExecutor, ReadTextInput, read_text_blocking},
    trace::{
        TRACE_PATH_MAX_CHARS, bounded_trace_text, install_patch_test_after_write_hook,
        install_trace_start_test_hook,
    },
};
use merry_core::{
    PendingToolCall, ToolCallArguments, ToolCallId, ToolCallResultStatus, ToolName, ToolSpec,
};
use merry_runtime::{
    ActionExecutionEvidence, ArtifactContentKind, ToolConcurrency, ToolExecutionError,
};
use merry_runtime::{
    ActionProposal, ActionProposalEvidence, ToolActionKind, ToolActionPreflight,
    ToolExecutionContext, ToolExecutionOutcome, ToolExecutor,
};
use serde_json::{Value, json};
use std::{
    cell::Cell,
    env,
    ffi::OsStr,
    fs::{self, File},
    future::Future,
    io::Write,
    sync::{Arc as StdArc, Mutex as StdMutex, OnceLock as StdOnceLock},
    time::{SystemTime, UNIX_EPOCH},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(unix)]
use std::os::unix::fs::symlink;

fn trace_output_buffer() -> &'static StdArc<StdMutex<Vec<u8>>> {
    #[derive(Clone)]
    struct Buffer(StdArc<StdMutex<Vec<u8>>>);

    impl std::io::Write for Buffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("buffer mutex should not be poisoned")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    static TRACE_OUTPUT: StdOnceLock<StdArc<StdMutex<Vec<u8>>>> = StdOnceLock::new();
    TRACE_OUTPUT.get_or_init(|| {
        use tracing_subscriber::{fmt, prelude::*};

        let bytes = StdArc::new(StdMutex::new(Vec::new()));
        let writer_bytes = StdArc::clone(&bytes);
        let subscriber = tracing_subscriber::registry().with(
            fmt::layer()
                .json()
                .with_writer(move || Buffer(StdArc::clone(&writer_bytes))),
        );
        tracing::subscriber::set_global_default(subscriber)
            .expect("test tracing subscriber should install once");
        bytes
    })
}

async fn capture_traces_for<F, R>(trace_marker: &str, future: F) -> (R, String)
where
    F: Future<Output = R>,
{
    let bytes = StdArc::clone(trace_output_buffer());
    let start = bytes
        .lock()
        .expect("buffer mutex should not be poisoned")
        .len();
    let result = future.await;
    let text = {
        let guard = bytes.lock().expect("buffer mutex should not be poisoned");
        String::from_utf8(guard[start..].to_vec()).expect("trace output should be UTF-8")
    };
    let text = text
        .lines()
        .filter(|line| line.contains(trace_marker))
        .collect::<Vec<_>>()
        .join("\n");
    (result, text)
}

struct TempWorkspace {
    root: PathBuf,
}

impl TempWorkspace {
    fn new(name: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after unix epoch")
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "merry-tools-{name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("temp workspace should be created");
        Self { root }
    }

    fn path(&self) -> &Path {
        &self.root
    }

    fn write_text(&self, relative: &str, content: &str) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent directory should be created");
        }
        fs::write(path, content).expect("text file should be written");
    }

    fn write_bytes(&self, relative: &str, content: &[u8]) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent directory should be created");
        }
        let mut file = File::create(path).expect("binary file should be created");
        file.write_all(content)
            .expect("binary file should be written");
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn tools_for(root: &Path) -> WorkspaceTools {
    WorkspaceTools::new(WorkspaceToolsConfig::new(vec![root.to_path_buf()]))
        .expect("workspace tools should construct")
}

#[test]
fn workspace_tool_schemas_describe_bounded_file_operations() {
    fn assert_fields(spec: ToolSpec, fields: &[&str]) {
        let value = serde_json::to_value(spec.input_schema().as_schema())
            .expect("workspace schema should serialize");
        for field in fields {
            assert!(
                !value["properties"][*field]["description"]
                    .as_str()
                    .unwrap_or_default()
                    .is_empty(),
                "missing workspace field description for {field}"
            );
        }
    }

    assert_fields(
        read_text_spec_default(),
        &["path", "start_line", "max_lines"],
    );
    assert_fields(apply_patch_spec(), &["patch"]);

    let read_schema = serde_json::to_value(read_text_spec_default().input_schema().as_schema())
        .expect("read schema should serialize");
    let patch_schema = serde_json::to_value(apply_patch_spec().input_schema().as_schema())
        .expect("patch schema should serialize");
    assert_eq!(read_schema["properties"]["path"]["minLength"], 1);
    assert_eq!(read_schema["properties"]["start_line"]["minimum"], 1);
    assert_eq!(read_schema["properties"]["max_lines"]["minimum"], 1);
    assert_eq!(patch_schema["properties"]["patch"]["minLength"], 1);
}

#[test]
fn workspace_schemas_project_session_limits() {
    let temp = TempWorkspace::new("schema-limits");
    let limits = WorkspaceToolLimits {
        max_read_lines: 17,
        max_patch_bytes: 29,
        ..WorkspaceToolLimits::default()
    };
    let tools = WorkspaceTools::new(
        WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(limits),
    )
    .expect("workspace tools should construct");
    let registered = tools.into_registered_tools_with_patch();
    let read = registered
        .iter()
        .find(|tool| tool.spec().name().as_str() == READ_TEXT_TOOL)
        .expect("read_text tool should be registered");
    let patch = registered
        .iter()
        .find(|tool| tool.spec().name().as_str() == APPLY_PATCH_TOOL)
        .expect("apply_patch tool should be registered");
    let read_schema = read.spec().input_schema().as_schema().as_value();
    let patch_schema = patch.spec().input_schema().as_schema().as_value();

    fn has_constraint(schema: &Value, keyword: &str, expected: &Value) -> bool {
        if schema.get(keyword) == Some(expected) {
            return true;
        }
        ["anyOf", "oneOf", "allOf"].iter().any(|branch_keyword| {
            schema
                .get(*branch_keyword)
                .and_then(Value::as_array)
                .is_some_and(|branches| {
                    branches
                        .iter()
                        .any(|branch| has_constraint(branch, keyword, expected))
                })
        })
    }

    assert!(has_constraint(
        &read_schema["properties"]["max_lines"],
        "maximum",
        &json!(17)
    ));
    assert_eq!(patch_schema["properties"]["patch"]["maxLength"], 29);
}

fn read_outcome(tools: &WorkspaceTools, path: &str) -> ToolExecutionOutcome {
    read_text_blocking(
        &tools.state,
        ReadTextInput {
            path: path.to_owned(),
            start_line: None,
            max_lines: None,
        },
    )
}

fn patch_outcome(
    tools: &WorkspaceTools,
    path: &str,
    old_text: &str,
    new_text: &str,
) -> ToolExecutionOutcome {
    apply_patch_blocking(
        &tools.state,
        ApplyPatchInput {
            patch: update_patch(path, old_text, new_text),
        },
    )
}

fn patch_text_outcome(tools: &WorkspaceTools, patch: &str) -> ToolExecutionOutcome {
    apply_patch_blocking(
        &tools.state,
        ApplyPatchInput {
            patch: patch.to_owned(),
        },
    )
}

fn patch_proposal(
    tools: &WorkspaceTools,
    path: &str,
    old_text: &str,
    new_text: &str,
) -> Option<ActionProposal> {
    match patch_preflight(tools, path, old_text, new_text) {
        ToolActionPreflight::Proposal(proposal) => Some(proposal),
        ToolActionPreflight::NoProposal | ToolActionPreflight::Outcome(_) => None,
    }
}

fn patch_preflight(
    tools: &WorkspaceTools,
    path: &str,
    old_text: &str,
    new_text: &str,
) -> ToolActionPreflight {
    let patch = update_patch(path, old_text, new_text);
    let call = pending_call_for(
        APPLY_PATCH_TOOL,
        json!({
            "patch": patch
        }),
    );
    propose_apply_patch_blocking_checked(&tools.state, ApplyPatchInput { patch }, &call, &|| false)
        .expect("uncancelled workspace patch proposal should not return cancellation")
}

fn update_patch(path: &str, old_text: &str, new_text: &str) -> String {
    format!(
        "*** Begin Workspace Patch\n*** Update File: {path}\n-{old_text}\n+{new_text}\n*** End Workspace Patch"
    )
}

fn add_patch(path: &str, lines: &[&str]) -> String {
    let additions = lines
        .iter()
        .map(|line| format!("+{line}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("*** Begin Workspace Patch\n*** Add File: {path}\n{additions}\n*** End Workspace Patch")
}

fn add_patch_preflight(tools: &WorkspaceTools, path: &str, lines: &[&str]) -> ToolActionPreflight {
    let patch = add_patch(path, lines);
    let call = pending_call_for(
        APPLY_PATCH_TOOL,
        json!({
            "patch": patch
        }),
    );
    propose_apply_patch_blocking_checked(&tools.state, ApplyPatchInput { patch }, &call, &|| false)
        .expect("uncancelled workspace patch proposal should not return cancellation")
}

fn json_content(outcome: &ToolExecutionOutcome) -> Value {
    serde_json::from_str(
        outcome
            .content()
            .as_text()
            .expect("json content should be text"),
    )
    .expect("tool outcome should be JSON")
}

fn assert_failed_json(
    outcome: &ToolExecutionOutcome,
    code: &str,
    path: Option<&str>,
    host_root: &Path,
) {
    assert_failed_json_for_tool(outcome, READ_TEXT_TOOL, code, path, host_root);
}

fn assert_failed_json_for_tool(
    outcome: &ToolExecutionOutcome,
    tool: &str,
    code: &str,
    path: Option<&str>,
    host_root: &Path,
) {
    assert_eq!(outcome.status(), ToolCallResultStatus::Failed);
    assert_eq!(outcome.content().kind(), ArtifactContentKind::Json);
    assert_eq!(outcome.diagnostic().expect("diagnostic").code(), code);

    let payload = json_content(outcome);
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["tool"], tool);
    assert_eq!(payload["error"]["code"], code);
    assert_eq!(
        payload["recovery"]["path_contract"],
        WORKSPACE_PATH_CONTRACT
    );
    if let Some(expected_guidance_kind) = expected_guidance_kind_for_code(code) {
        assert_eq!(payload["guidance"]["kind"], expected_guidance_kind);
        assert!(
            payload["guidance"]["message"]
                .as_str()
                .expect("guidance should be text")
                .len()
                > 20,
            "guidance should contain an actionable model hint"
        );
    } else {
        assert!(
            payload.get("guidance").is_none(),
            "unexpected guidance for {code}"
        );
    }
    if let Some(path) = path {
        assert_eq!(payload["path"], path);
    } else {
        assert!(
            payload.get("path").is_none(),
            "failure payload should omit path"
        );
    }

    assert!(
        !outcome
            .content()
            .as_text()
            .expect("json content")
            .contains(host_root.to_str().expect("temp path utf8")),
        "tool output must not include absolute host roots"
    );
}

fn expected_guidance_kind_for_code(code: &str) -> Option<&'static str> {
    match code {
        ERROR_INVALID_ARGUMENTS => Some("workspace_invalid_arguments"),
        ERROR_PATH_DENIED
        | ERROR_FILE_NOT_FOUND
        | ERROR_FILE_ALREADY_EXISTS
        | ERROR_NOT_FILE
        | ERROR_NOT_DIRECTORY => Some("workspace_path_recovery"),
        ERROR_FILE_TOO_LARGE => Some("workspace_file_too_large"),
        ERROR_PREIMAGE_ABSENT | ERROR_PREIMAGE_AMBIGUOUS => Some("apply_patch_preimage_mismatch"),
        ERROR_PROPOSAL_MISMATCH => Some("apply_patch_plan_changed"),
        _ => None,
    }
}

fn assert_no_provider_visible_patch_metadata(outcome: &ToolExecutionOutcome) {
    let text = outcome
        .content()
        .as_text()
        .expect("json content should be text");
    for forbidden in [
        "approved workspace patch",
        "fingerprint",
        "proposal",
        "fnv1a64",
        "file_fingerprint_before",
        "file_fingerprint_after",
    ] {
        assert!(
            !text.contains(forbidden),
            "provider-visible patch output leaked {forbidden}: {text}"
        );
    }
    if let Some(diagnostic) = outcome.diagnostic() {
        assert_eq!(
            diagnostic.message(),
            WORKSPACE_PATCH_PLAN_CHANGED_MESSAGE,
            "patch mismatch diagnostic should stay neutral"
        );
        let diagnostic_text = format!("{} {}", diagnostic.code(), diagnostic.message());
        for forbidden in [
            "approved workspace patch",
            "fingerprint",
            "proposal",
            "fnv1a64",
            "file_fingerprint_before",
            "file_fingerprint_after",
        ] {
            assert!(
                !diagnostic_text.contains(forbidden),
                "provider-visible patch diagnostic leaked {forbidden}: {diagnostic_text}"
            );
        }
    }
}

fn pending_call(arguments: Value) -> PendingToolCall {
    pending_call_for(READ_TEXT_TOOL, arguments)
}

fn pending_call_with_id(tool: &str, call_id: &str, arguments: Value) -> PendingToolCall {
    let arguments = ToolCallArguments::try_from(arguments).expect("arguments object");
    PendingToolCall::new(
        ToolCallId::new(call_id).expect("valid call id"),
        ToolName::new(tool).expect("valid tool name"),
        arguments,
    )
}

fn pending_call_for(tool: &str, arguments: Value) -> PendingToolCall {
    pending_call_with_id(tool, "call-1", arguments)
}

fn assert_invalid_arguments_trace(
    outcome: ToolExecutionOutcome,
    logs: &str,
    tool_name: &str,
    tool_call_id: &str,
) {
    assert_eq!(outcome.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        outcome.diagnostic().expect("diagnostic").code(),
        ERROR_INVALID_ARGUMENTS
    );
    assert!(logs.contains("\"event\":\"runtime.workspace_tool.finish\""));
    assert!(logs.contains("\"status\":\"failed\""));
    assert!(logs.contains("\"diagnostic_code\":\"workspace_invalid_arguments\""));
    assert!(logs.contains(&format!("\"tool_name\":\"{tool_name}\"")));
    assert!(logs.contains(&format!("\"tool_call_id\":\"{tool_call_id}\"")));
    assert!(!logs.contains("sensitive invalid payload"));
}

fn read_text(path: &Path) -> String {
    fs::read_to_string(path).expect("workspace text file should be readable")
}

static TRACE_START_CANCEL_TOKEN: StdOnceLock<
    StdMutex<Option<tokio_util::sync::CancellationToken>>,
> = StdOnceLock::new();

fn install_trace_start_cancellation_token(token: tokio_util::sync::CancellationToken) {
    TRACE_START_CANCEL_TOKEN
        .get_or_init(|| StdMutex::new(None))
        .lock()
        .expect("trace start cancel token mutex should not be poisoned")
        .replace(token);
}

fn cancel_trace_start_token() {
    let Some(slot) = TRACE_START_CANCEL_TOKEN.get() else {
        return;
    };
    let token = slot
        .lock()
        .expect("trace start cancel token mutex should not be poisoned")
        .take();
    if let Some(token) = token {
        token.cancel();
    }
}

mod config;
mod patch;
mod patch_policy;
mod read;
mod trace;

fn read_text_spec_default() -> ToolSpec {
    crate::read::spec(&WorkspaceToolLimits::default()).expect("valid read tool schema")
}

fn apply_patch_spec() -> ToolSpec {
    crate::patch::spec(&WorkspaceToolLimits::default()).expect("valid patch tool schema")
}
