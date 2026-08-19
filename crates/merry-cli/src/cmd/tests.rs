use super::*;
use crate::runtime_events::{collect_runtime_step_events, first_pending_tool_call};
use crate::testing::{ScriptedProvider, tool_call, workspace_tool_call};
use merry_core::RuntimeJournalPayload;
use merry_llm::{FinishReason, ModelEvent, ModelName, ModelOutput, ModelResponse};
use merry_runtime::{ArtifactContent, StepContext, StepInput, ToolExecutionContext};
use merry_tool_workspace::{
    CODING_LOOP_PROCESS_TOOL, WORKSPACE_PATCH_TOOL, WORKSPACE_READ_FILE_TOOL,
};
use serde_json::Value;
use std::{
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::io::AsyncWrite;

#[derive(Default)]
struct FlushCountingWriter {
    bytes: Vec<u8>,
    flushes: usize,
}

impl AsyncWrite for FlushCountingWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.bytes.extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.flushes += 1;
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[test]
fn default_cmd_session_id_is_generated() {
    let first = default_cmd_session_id();
    let second = default_cmd_session_id();

    assert_ne!(first, second);
    assert_ne!(first.as_str(), "cmd");
}

#[test]
fn command_plan_final_output_contract_has_described_fields() {
    let contract = command_plan_final_output_contract()
        .expect("command plan final output contract should build");

    assert_eq!(
        contract.tool_name().as_str(),
        merry_runtime::FINAL_OUTPUT_TOOL_NAME
    );
    let schema = serde_json::to_value(contract.tool_spec().input_schema().as_schema())
        .expect("schema should serialize");
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .expect("schema should have object properties");

    for field in ["shell_command", "notes", "cautions"] {
        let description = properties
            .get(field)
            .and_then(|schema| schema.get("description"))
            .and_then(Value::as_str)
            .expect("field should have a description");
        assert!(!description.trim().is_empty());
    }
}

#[test]
fn command_check_schema_describes_program_items() {
    let temp = tempfile::tempdir().expect("tempdir");
    let tool = cmd_check_command_tool(CommandGenerationEnvironment::detect(temp.path()))
        .expect("command check tool should build");
    let schema = serde_json::to_value(tool.spec().input_schema().as_schema())
        .expect("schema should serialize");
    assert_eq!(schema["properties"]["programs"]["minItems"], 1);
    assert_eq!(schema["properties"]["programs"]["items"]["minLength"], 1);
    assert!(
        !schema["properties"]["programs"]["items"]["description"]
            .as_str()
            .unwrap_or_default()
            .is_empty()
    );
}

#[test]
fn command_generation_prompt_treats_file_search_as_recursive_by_default() {
    let temp = tempfile::tempdir().expect("tempdir");
    let environment = CommandGenerationEnvironment::detect(temp.path());
    let prompt = command_generation_prompt("列出当前目录的 rs 文件", &environment);

    assert!(prompt.contains("recursive by default"));
    assert!(prompt.contains("find -maxdepth 1 only when the user explicitly asks"));
    assert!(prompt.contains("user's current input language"));
    assert!(prompt.contains("Runtime environment:"));
    assert!(prompt.contains(CHECK_COMMAND_TOOL_NAME));
    assert!(prompt.contains("prefer a single shell pipeline"));
}

#[tokio::test(flavor = "current_thread")]
async fn command_generation_runtime_is_read_only_workspace_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
    })]]);

    let runtime = build_runtime(RuntimeInput {
        session_id: "cmd-generation-runtime",
        root: &workspace,
        environment: CommandGenerationEnvironment::detect(&workspace),
        provider: Arc::new(provider.clone()),
        model: ModelName::new("debug-model").expect("valid model name"),
        allow_hidden_workspace_paths: false,
        automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
        retry_policy: None,
        context_compaction: None,
        skill_roots: Vec::new(),
    })
    .expect("runtime should build");

    collect_runtime_step_events(
        &runtime,
        StepInput::user_text("Inspect workspace.").expect("valid input"),
        StepContext::default(),
    )
    .await
    .expect("runtime step should complete");

    let request = provider.recorded_requests()[0].clone();
    let tool_names = request
        .tools()
        .iter()
        .map(|tool| tool.name().as_str())
        .collect::<Vec<_>>();
    assert!(tool_names.contains(&WORKSPACE_READ_FILE_TOOL));
    assert!(tool_names.contains(&CHECK_COMMAND_TOOL_NAME));
    assert!(!tool_names.contains(&WORKSPACE_PATCH_TOOL));
    assert!(!tool_names.contains(&CODING_LOOP_PROCESS_TOOL));
}

#[tokio::test(flavor = "current_thread")]
async fn generate_command_plan_reads_structured_final_output() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    let provider = ScriptedProvider::new(vec![vec![Ok(workspace_tool_call(
        "call-final-command-plan",
        merry_runtime::FINAL_OUTPUT_TOOL_NAME,
        [
            (
                "shell_command",
                Value::String("find . -name '*.rs' -print".to_owned()),
            ),
            (
                "notes",
                Value::Array(vec![Value::String(
                    "Searches from the current directory.".to_owned(),
                )]),
            ),
            (
                "cautions",
                Value::Array(vec![Value::String("May print many paths.".to_owned())]),
            ),
        ],
    )
    .expect("final output call should build"))]]);
    let runtime = build_runtime(RuntimeInput {
        session_id: "cmd-final-output",
        root: &workspace,
        environment: CommandGenerationEnvironment::detect(&workspace),
        provider: Arc::new(provider),
        model: ModelName::new("debug-model").expect("valid model name"),
        allow_hidden_workspace_paths: false,
        automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
        retry_policy: None,
        context_compaction: None,
        skill_roots: Vec::new(),
    })
    .expect("runtime should build");

    let plan = generate_command_plan(
        &runtime,
        "find rust files",
        &CommandGenerationEnvironment::detect(&workspace),
    )
    .await
    .expect("command plan should generate");

    assert_eq!(plan.shell_command, "find . -name '*.rs' -print");
    assert_eq!(plan.notes, ["Searches from the current directory."]);
    assert_eq!(plan.cautions, ["May print many paths."]);
}

#[tokio::test(flavor = "current_thread")]
async fn cmd_check_command_tool_reports_path_availability() {
    let temp = tempfile::tempdir().expect("tempdir");
    let bin = temp.path().join("bin");
    std::fs::create_dir_all(&bin).expect("mkdir bin");
    let program_path = bin.join("available-tool");
    std::fs::write(&program_path, "#!/bin/sh\n").expect("write tool");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&program_path)
            .expect("metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&program_path, permissions).expect("chmod tool");
    }

    let environment = CommandGenerationEnvironment {
        os: "linux",
        arch: "x86_64",
        family: "unix",
        shell: "/bin/sh".to_owned(),
        cwd: temp.path().to_owned(),
        path: Some(bin.display().to_string()),
    };
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        "programs".to_owned(),
        serde_json::json!(["available-tool", "missing-tool", "printf"]),
    );
    let provider = ScriptedProvider::new(vec![vec![Ok(tool_call(
        "call-check-command",
        CHECK_COMMAND_TOOL_NAME,
        arguments,
    )
    .expect("check command tool call should build"))]]);
    let runtime = build_runtime(RuntimeInput {
        session_id: "cmd-check-command-tool",
        root: temp.path(),
        environment,
        provider: Arc::new(provider),
        model: ModelName::new("debug-model").expect("valid model name"),
        allow_hidden_workspace_paths: false,
        automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
        retry_policy: None,
        context_compaction: None,
        skill_roots: Vec::new(),
    })
    .expect("runtime should build");
    let events = collect_runtime_step_events(
        &runtime,
        StepInput::user_text("check commands").expect("valid input"),
        StepContext::default(),
    )
    .await
    .expect("runtime step should collect pending call");
    let pending = first_pending_tool_call(&events).expect("pending check command call");
    let resolved = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("tool should execute");
    let artifact_id = resolved
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => {
                Some(result.artifact().id().clone())
            }
            _ => None,
        })
        .expect("tool result artifact should be recorded");
    let content = runtime
        .read_artifact_content(&artifact_id)
        .await
        .expect("tool result artifact should be readable");
    let payload = match content {
        ArtifactContent::Json { content } => {
            serde_json::from_str::<serde_json::Value>(&content).expect("json payload")
        }
        other => panic!("expected json content, got {other:?}"),
    };

    assert_eq!(payload["ok"], true);
    assert_eq!(payload["results"][0]["available"], true);
    assert_eq!(payload["results"][1]["available"], false);
    assert_eq!(payload["results"][2]["kind"], "shell_builtin");
}

#[tokio::test(flavor = "current_thread")]
async fn prompt_execute_command_plan_defaults_to_no() {
    let plan = CommandPlan {
        shell_command: "printf should-not-run".to_owned(),
        notes: Vec::new(),
        cautions: Vec::new(),
    };
    let mut output = Vec::new();

    let accepted =
        prompt_execute_command_plan(&plan, tokio::io::BufReader::new("".as_bytes()), &mut output)
            .await
            .expect("prompt should read");

    assert!(!accepted);
    assert_eq!(String::from_utf8(output).expect("utf8"), "execute? [y/N] ");
}

#[tokio::test(flavor = "current_thread")]
async fn execute_shell_command_to_writer_writes_complete_output() {
    let mut output = FlushCountingWriter::default();

    execute_shell_command_to_writer(
        "printf 'stdout-line\\n'; sleep 0.01; printf 'stderr-line\\n' >&2",
        &mut output,
    )
    .await
    .expect("shell command should execute");

    assert!(
        output.flushes > 1,
        "shell execution output should flush while streams are read"
    );
    let text = String::from_utf8(output.bytes).expect("utf8");
    assert!(text.contains("stdout-line\n"));
    assert!(text.contains("stderr-line\n"));
}
