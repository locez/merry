use super::{
    Args, CliError, RunExitStatus, RunSession, STDIN_TASK, default_run_session_id,
    reserve_run_session, resolve_task, review_input_channel_for_task, write_agent_loop_output,
};
use crate::coding::{
    ActionProcessBackend, CodingSubagentsConfig, HeadlessCodingRuntimeInput, build_headless_coding,
    fixed_process_backend, resume_headless_coding,
};
use crate::headless_review::ReviewInputChannel;
use crate::testing::{FakeProcessRunner, ScriptedProvider, model_name};
use clap::Parser;
use merry::profiles::DEFAULT_CODING_AGENT_MAX_MODEL_TURNS;
use merry_core::SessionId;
use merry_llm::{FinishReason, ModelEvent, ModelOutput, ModelProvider, ModelResponse};
use merry_process::ProcessSession;
use merry_runtime::{AgentLoopConfig, ProcessRunner, StepContext, StepInput};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[test]
fn default_run_session_id_is_generated() {
    let first = default_run_session_id();
    let second = default_run_session_id();

    assert_ne!(first, second);
    assert_ne!(first.as_str(), "run");
}

fn parse_run_args(argv: &[&str]) -> Args {
    let cli = crate::cli::Cli::try_parse_from(argv).expect("run arguments should parse");
    match cli.command {
        Some(crate::cli::CliCommand::Run(args)) => args,
        other => panic!("expected the run subcommand, got {other:?}"),
    }
}

fn usage_message(error: CliError) -> String {
    match error {
        CliError::DebugUsage(message) => message,
        other => panic!("expected a usage error, got {other:?}"),
    }
}

fn completing_provider(text: &str) -> ScriptedProvider {
    ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text(text)], FinishReason::Stop, None),
    })]])
}

fn fake_process_backend() -> ActionProcessBackend {
    let runner: Arc<dyn ProcessRunner> = Arc::new(FakeProcessRunner::succeeding(""));
    let permissioned_factory = Arc::new(
        merry_runtime::StaticPermissionedProcessRunnerFactory::new(Arc::clone(&runner)),
    );
    fixed_process_backend(ProcessSession::from_parts(
        merry_runtime::AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace(),
        runner,
        permissioned_factory,
    ))
}

fn headless_input<'a>(
    session_id: &'a str,
    workspace: &'a Path,
    provider: Arc<dyn ModelProvider>,
) -> HeadlessCodingRuntimeInput<'a> {
    HeadlessCodingRuntimeInput {
        session_id,
        root: workspace,
        provider,
        model: model_name(),
        process_backend: fake_process_backend(),
        extra_tools: Vec::new(),
        allow_hidden_workspace_paths: false,
        automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
        retry_policy: None,
        context_compaction: None,
        approval_review: None,
        skill_roots: Vec::new(),
        subagents: CodingSubagentsConfig::default(),
        workspace_tool_limits: None,
    }
}

#[test]
fn run_starts_a_new_generated_session_without_session_flags() {
    let session = RunSession::from_args(&parse_run_args(&["merry", "run", "task"]))
        .expect("session should resolve");

    assert!(
        matches!(session, RunSession::New(_)),
        "a run without session flags should start a new session, got {session:?}"
    );
}

#[test]
fn run_writes_to_the_requested_session_id() {
    let session = RunSession::from_args(&parse_run_args(&[
        "merry",
        "run",
        "--session-id",
        "kiln-step-1",
        "task",
    ]))
    .expect("session should resolve");

    assert_eq!(
        session,
        RunSession::New(SessionId::new("kiln-step-1").unwrap())
    );
}

#[test]
fn run_resumes_the_requested_session_id() {
    let session = RunSession::from_args(&parse_run_args(&[
        "merry",
        "run",
        "--resume",
        "kiln-step-1",
        "task",
    ]))
    .expect("session should resolve");

    assert_eq!(
        session,
        RunSession::Resumed(SessionId::new("kiln-step-1").unwrap())
    );
}

#[test]
fn run_rejects_a_session_id_that_would_escape_the_session_store() {
    let error = RunSession::from_args(&parse_run_args(&[
        "merry",
        "run",
        "--session-id",
        "../outside",
        "task",
    ]))
    .expect_err("a path-shaped session id should be refused");

    assert!(
        usage_message(error).contains("--session-id"),
        "the refusal should name the flag that was wrong"
    );
}

#[test]
fn run_rejects_a_resume_id_that_would_escape_the_session_store() {
    let error = RunSession::from_args(&parse_run_args(&["merry", "run", "--resume", "..", "task"]))
        .expect_err("a path-shaped resume id should be refused");

    assert!(
        usage_message(error).contains("--resume"),
        "the refusal should name the flag that was wrong"
    );
}

#[test]
fn run_refuses_to_both_start_and_resume_a_session() {
    crate::cli::Cli::try_parse_from([
        "merry",
        "run",
        "--session-id",
        "one",
        "--resume",
        "two",
        "task",
    ])
    .expect_err("--session-id and --resume should conflict");
}

#[tokio::test(flavor = "current_thread")]
async fn task_comes_from_argv_when_it_is_not_a_dash() {
    let task = resolve_task("fix the failing tests", &b"ignored stdin"[..])
        .await
        .expect("argv task should resolve");

    assert_eq!(task, "fix the failing tests");
}

#[tokio::test(flavor = "current_thread")]
async fn task_comes_from_stdin_when_it_is_a_dash() {
    let prompt = "a task too large and too private for argv\n";

    let task = resolve_task(STDIN_TASK, prompt.as_bytes())
        .await
        .expect("stdin task should resolve");

    assert_eq!(task, prompt);
}

#[tokio::test(flavor = "current_thread")]
async fn a_blank_stdin_task_is_a_usage_error() {
    let error = resolve_task(STDIN_TASK, &b"  \n\t"[..])
        .await
        .expect_err("a blank stdin task should be refused");

    assert!(
        usage_message(error).contains("stdin"),
        "the refusal should say where the empty task came from"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_saved_run_session_resumes_with_its_recorded_transcript() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    let store = merry_runtime::FileSessionStore::new(temp.path().join("sessions"));
    let session_id = "run-resume-session";

    let first = build_headless_coding(headless_input(
        session_id,
        &workspace,
        Arc::new(completing_provider("first answer")),
    ))
    .expect("runtime should build");
    let mut output = Vec::new();
    let status = write_agent_loop_output(
        &first,
        StepInput::user_text("first task").expect("valid input"),
        AgentLoopConfig::new(DEFAULT_CODING_AGENT_MAX_MODEL_TURNS).expect("valid config"),
        StepContext::default(),
        &mut output,
    )
    .await
    .expect("first run should write");
    assert_eq!(status, RunExitStatus::Completed);
    first
        .save_session_to(store.clone())
        .await
        .expect("a finished run should save its session");
    drop(first);

    let resumed = resume_headless_coding(
        headless_input(
            session_id,
            &workspace,
            Arc::new(completing_provider("second answer")),
        ),
        store,
    )
    .await
    .expect("the saved session should resume");

    let transcript = resumed
        .session_transcript()
        .await
        .expect("a resumed session should expose its transcript");
    assert!(
        transcript.iter().any(|item| matches!(
            item,
            merry_runtime::SessionTranscriptItem::UserMessage { text, .. }
                if text == "first task"
        )),
        "the resumed session should keep the first task: {transcript:?}"
    );
    assert!(
        transcript.iter().any(|item| matches!(
            item,
            merry_runtime::SessionTranscriptItem::AssistantText { text }
                if text == "first answer"
        )),
        "the resumed session should keep the first answer: {transcript:?}"
    );
}

#[test]
fn a_stdin_task_moves_permission_review_off_stdin() {
    assert_eq!(
        review_input_channel_for_task(STDIN_TASK),
        ReviewInputChannel::ControllingTerminal,
        "a `-` task consumes stdin, so review cannot also read it"
    );
    assert_eq!(
        review_input_channel_for_task("fix the failing tests"),
        ReviewInputChannel::Stdin,
        "an argv task leaves stdin free for review answers"
    );
}

/// Writes committed state for `session_id` and returns its `state.json` path.
fn save_existing_session_state(sessions_dir: &Path, session_id: &str, bytes: &[u8]) -> PathBuf {
    let session_dir = sessions_dir.join(session_id);
    std::fs::create_dir_all(&session_dir).expect("session dir should be created");
    let state_path = session_dir.join("state.json");
    std::fs::write(&state_path, bytes).expect("existing session state should be written");
    state_path
}

#[tokio::test(flavor = "current_thread")]
async fn a_new_run_refuses_a_session_id_that_already_has_saved_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let sessions_dir = temp.path().join("sessions");
    let state_path = save_existing_session_state(&sessions_dir, "already-saved", b"existing state");
    let store = merry_runtime::FileSessionStore::new(&sessions_dir);
    let session_id = SessionId::new("already-saved").expect("valid session id");

    let error = reserve_run_session(&RunSession::New(session_id), &store)
        .await
        .expect_err("a new run must not take over a saved session id");

    let message = usage_message(error);
    assert!(
        message.contains("--resume already-saved"),
        "the refusal should point at the way to continue the session: {message}"
    );
    assert_eq!(
        std::fs::read(&state_path).expect("saved state should still be readable"),
        b"existing state",
        "a refused run must leave the saved session untouched"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_new_run_accepts_a_session_id_the_store_does_not_hold() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = merry_runtime::FileSessionStore::new(temp.path().join("sessions"));
    let session_id = SessionId::new("never-saved").expect("valid session id");

    reserve_run_session(&RunSession::New(session_id), &store)
        .await
        .expect("an unused session id should start a new run");
}

#[tokio::test(flavor = "current_thread")]
async fn resuming_a_saved_session_id_is_not_a_collision() {
    let temp = tempfile::tempdir().expect("tempdir");
    let sessions_dir = temp.path().join("sessions");
    save_existing_session_state(&sessions_dir, "resume-me", b"existing state");
    let store = merry_runtime::FileSessionStore::new(&sessions_dir);
    let session_id = SessionId::new("resume-me").expect("valid session id");

    reserve_run_session(&RunSession::Resumed(session_id), &store)
        .await
        .expect("resuming is how an existing session id is meant to be reused");
}
