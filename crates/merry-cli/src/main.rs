//! Debug and demonstration CLI for Merry.

mod cli;
mod cli_error;
mod cmd;
mod coding_runtime;
mod config;
mod debug;
mod observability;
mod provider_config;
mod run;
mod runtime_config;
mod runtime_events;
mod sandbox;
#[cfg(test)]
mod test_support;

use clap::Parser;
use config::{MerryConfig, XdgPaths};
use merry_tool_workspace::{
    CODING_LOOP_PROCESS_TOOL, WORKSPACE_PATCH_TOOL, WORKSPACE_READ_FILE_TOOL,
};
use std::{
    env, io,
    process::{ExitCode, Termination},
};

use cli::{Cli, CliCommand};
use cli_error::CliError;
use debug::{
    Args as DebugArgs, CodingLoopLiveSmokeArgs as DebugCodingLoopLiveSmokeArgs,
    CodingLoopSubagentLiveSmokeArgs as DebugCodingLoopSubagentLiveSmokeArgs,
    CodingLoopTaskLiveSmokeArgs as DebugCodingLoopTaskLiveSmokeArgs, Command as DebugCommand,
    OpenAiArgs as DebugOpenAiArgs, PermissionNetworkSmokeArgs as DebugPermissionNetworkSmokeArgs,
};
use runtime_config::{effective_log_settings, validate_loaded_config};

const DEFAULT_SESSION_ID: &str = "debug-session";
const DEFAULT_INPUT: &str = "debug step";
const DEBUG_TOOL_NAME: &str = "debug_echo";
const DEBUG_TOOL_CONTINUATION_INPUT: &str = "continue after debug tool";
const CODING_LOOP_SMOKE_SESSION_ID: &str = "coding-loop-smoke";
const CODING_LOOP_LIVE_SMOKE_SESSION_ID: &str = "coding-loop-live-smoke";
const CODING_LOOP_TASK_SMOKE_SESSION_ID: &str = "coding-loop-task-smoke";
const CODING_LOOP_TASK_LIVE_SMOKE_SESSION_ID: &str = "coding-loop-task-live-smoke";
const CODING_LOOP_SUBAGENT_LIVE_SMOKE_SESSION_ID: &str = "coding-loop-subagent-live-smoke";
const PERMISSION_NETWORK_SMOKE_SESSION_ID: &str = "permission-network-smoke";
const PERMISSION_NETWORK_SMOKE_ARGV: [&str; 3] = ["getent", "hosts", "example.com"];
const CODING_LOOP_LIVE_SMOKE_INITIAL_VALUE: &str = "unfixed";
const CODING_LOOP_LIVE_SMOKE_TARGET_VALUE: &str = "fixed-by-live-llm";
const CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE: &str = "subagent-output.txt";
const CODING_LOOP_SUBAGENT_LIVE_SMOKE_INITIAL: &str = "status: pending\n";
const CODING_LOOP_SUBAGENT_LIVE_SMOKE_TARGET: &str = "status: subagent-live-smoke-complete\n";
const CODING_LOOP_TASK_SMOKE_MAX_PATCH_BYTES: usize = 256;
fn main() -> CliExit {
    let argv = env::args_os().collect::<Vec<_>>();
    let cli = match Cli::try_parse_from(argv.clone()) {
        Ok(cli) => cli,
        Err(error) => return CliExit::Clap(error),
    };

    let config_paths = match XdgPaths::from_env() {
        Ok(paths) => paths,
        Err(error) => return CliExit::Unexpected(error.to_string()),
    };
    let _config = match MerryConfig::load_optional(&config_paths) {
        Ok(config) => config,
        Err(error) => return CliExit::Unexpected(error.to_string()),
    };
    if let Err(error) = validate_loaded_config(_config.as_ref(), &config_paths) {
        return CliExit::Unexpected(error.to_string());
    }
    let log_settings = match effective_log_settings(_config.as_ref(), &config_paths) {
        Ok(settings) => settings,
        Err(error) => return CliExit::Unexpected(error.to_string()),
    };

    if let Err(error) =
        sandbox::maybe_reexec(cli.with_sandbox, argv.iter().skip(1).cloned().collect())
    {
        return CliExit::Unexpected(error.to_string());
    }

    let _observability_guard = match observability::init_observability(log_settings.as_ref()) {
        Ok(guard) => guard,
        Err(error) => return CliExit::Unexpected(error.to_string()),
    };

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => return CliExit::Unexpected(err.to_string()),
    };

    runtime.block_on(async_main(cli, _config))
}

async fn async_main(cli: Cli, merry_config: Option<MerryConfig>) -> CliExit {
    let sandbox_child_handoff = cli.sandbox_child_handoff;

    match cli.command {
        CliCommand::Run(args) => {
            match run::run(&args, sandbox_child_handoff, merry_config.as_ref()).await {
                Ok(()) => CliExit::Success,
                Err(CliError::BrokenPipe) => CliExit::Success,
                Err(CliError::DebugUsage(message)) => CliExit::Usage {
                    message,
                    usage: cli::run_usage(),
                },
                Err(CliError::DebugOpenAiUsage(message)) => CliExit::Usage {
                    message,
                    usage: cli::run_usage(),
                },
                Err(CliError::ShellUsage(message)) => CliExit::Usage {
                    message,
                    usage: cli::shell_usage(),
                },
                Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
            }
        }
        CliCommand::Cmd(args) => match cmd::run(&args, merry_config.as_ref()).await {
            Ok(()) => CliExit::Success,
            Err(CliError::BrokenPipe) => CliExit::Success,
            Err(CliError::DebugUsage(message)) => CliExit::Usage {
                message,
                usage: cli::cmd_usage(),
            },
            Err(CliError::DebugOpenAiUsage(message)) => CliExit::Usage {
                message,
                usage: cli::cmd_usage(),
            },
            Err(CliError::ShellUsage(message)) => CliExit::Usage {
                message,
                usage: cli::shell_usage(),
            },
            Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
        },
        CliCommand::Debug(DebugArgs {
            session_id,
            input,
            command: None,
        }) => match debug::basic::run(&session_id, &input, merry_config.as_ref()).await {
            Ok(()) => CliExit::Success,
            Err(CliError::BrokenPipe) => CliExit::Success,
            Err(CliError::DebugUsage(message)) => CliExit::Usage {
                message,
                usage: cli::debug_usage(),
            },
            Err(CliError::DebugOpenAiUsage(message)) => CliExit::Usage {
                message,
                usage: cli::debug_openai_usage(),
            },
            Err(CliError::ShellUsage(message)) => CliExit::Usage {
                message,
                usage: cli::shell_usage(),
            },
            Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
        },
        CliCommand::Debug(DebugArgs {
            command:
                Some(DebugCommand::OpenAi(DebugOpenAiArgs {
                    input,
                    model,
                    max_output_tokens,
                    debug_tool_result,
                })),
            ..
        }) => match debug::openai::run(
            &input,
            model.as_deref(),
            max_output_tokens,
            debug_tool_result.as_deref(),
            merry_config.as_ref(),
        )
        .await
        {
            Ok(()) => CliExit::Success,
            Err(CliError::BrokenPipe) => CliExit::Success,
            Err(CliError::DebugOpenAiUsage(message)) => CliExit::Usage {
                message,
                usage: cli::debug_openai_usage(),
            },
            Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
            Err(CliError::DebugUsage(message)) => CliExit::Usage {
                message,
                usage: cli::debug_usage(),
            },
            Err(CliError::ShellUsage(message)) => CliExit::Usage {
                message,
                usage: cli::shell_usage(),
            },
        },
        CliCommand::Debug(DebugArgs {
            command: Some(DebugCommand::Shell(args)),
            ..
        }) => match debug::shell::run(args, sandbox_child_handoff, merry_config.as_ref()).await {
            Ok(()) => CliExit::Success,
            Err(CliError::BrokenPipe) => CliExit::Success,
            Err(CliError::ShellUsage(message)) => CliExit::Usage {
                message,
                usage: cli::shell_usage(),
            },
            Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
            Err(CliError::DebugUsage(message)) => CliExit::Usage {
                message,
                usage: cli::debug_usage(),
            },
            Err(CliError::DebugOpenAiUsage(message)) => CliExit::Usage {
                message,
                usage: cli::debug_openai_usage(),
            },
        },
        CliCommand::Debug(DebugArgs {
            command: Some(DebugCommand::CodingLoopSmoke),
            ..
        }) => match debug::coding_loop::run_smoke(sandbox_child_handoff, merry_config.as_ref())
            .await
        {
            Ok(()) => CliExit::Success,
            Err(CliError::BrokenPipe) => CliExit::Success,
            Err(CliError::DebugUsage(message)) => CliExit::Usage {
                message,
                usage: cli::debug_usage(),
            },
            Err(CliError::DebugOpenAiUsage(message)) => CliExit::Usage {
                message,
                usage: cli::debug_openai_usage(),
            },
            Err(CliError::ShellUsage(message)) => CliExit::Usage {
                message,
                usage: cli::shell_usage(),
            },
            Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
        },
        CliCommand::Debug(DebugArgs {
            command:
                Some(DebugCommand::PermissionNetworkSmoke(DebugPermissionNetworkSmokeArgs {
                    model,
                    max_output_tokens,
                })),
            ..
        }) => {
            match debug::coding_loop::run_permission_network_smoke(
                sandbox_child_handoff,
                model.as_deref(),
                max_output_tokens,
                merry_config.as_ref(),
            )
            .await
            {
                Ok(()) => CliExit::Success,
                Err(CliError::BrokenPipe) => CliExit::Success,
                Err(CliError::DebugUsage(message)) => CliExit::Usage {
                    message,
                    usage: cli::debug_usage(),
                },
                Err(CliError::DebugOpenAiUsage(message)) => CliExit::Usage {
                    message,
                    usage: cli::debug_openai_usage(),
                },
                Err(CliError::ShellUsage(message)) => CliExit::Usage {
                    message,
                    usage: cli::shell_usage(),
                },
                Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
            }
        }
        CliCommand::Debug(DebugArgs {
            command: Some(DebugCommand::CodingLoopTaskSmoke(args)),
            ..
        }) => match debug::coding_loop::run_task_smoke(
            sandbox_child_handoff,
            args.task,
            merry_config.as_ref(),
        )
        .await
        {
            Ok(()) => CliExit::Success,
            Err(CliError::BrokenPipe) => CliExit::Success,
            Err(CliError::DebugUsage(message)) => CliExit::Usage {
                message,
                usage: cli::debug_usage(),
            },
            Err(CliError::DebugOpenAiUsage(message)) => CliExit::Usage {
                message,
                usage: cli::debug_openai_usage(),
            },
            Err(CliError::ShellUsage(message)) => CliExit::Usage {
                message,
                usage: cli::shell_usage(),
            },
            Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
        },
        CliCommand::Debug(DebugArgs {
            command:
                Some(DebugCommand::CodingLoopLiveSmoke(DebugCodingLoopLiveSmokeArgs {
                    model,
                    max_output_tokens,
                })),
            ..
        }) => match debug::coding_loop::run_live_smoke(
            sandbox_child_handoff,
            model.as_deref(),
            max_output_tokens,
            merry_config.as_ref(),
        )
        .await
        {
            Ok(()) => CliExit::Success,
            Err(CliError::BrokenPipe) => CliExit::Success,
            Err(CliError::DebugUsage(message)) => CliExit::Usage {
                message,
                usage: cli::debug_usage(),
            },
            Err(CliError::DebugOpenAiUsage(message)) => CliExit::Usage {
                message,
                usage: cli::debug_coding_loop_live_smoke_usage(),
            },
            Err(CliError::ShellUsage(message)) => CliExit::Usage {
                message,
                usage: cli::shell_usage(),
            },
            Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
        },
        CliCommand::Debug(DebugArgs {
            command:
                Some(DebugCommand::CodingLoopTaskLiveSmoke(DebugCodingLoopTaskLiveSmokeArgs {
                    task,
                    model,
                    max_output_tokens,
                })),
            ..
        }) => match debug::coding_loop::run_task_live_smoke(
            sandbox_child_handoff,
            task,
            model.as_deref(),
            max_output_tokens,
            merry_config.as_ref(),
        )
        .await
        {
            Ok(()) => CliExit::Success,
            Err(CliError::BrokenPipe) => CliExit::Success,
            Err(CliError::DebugUsage(message)) => CliExit::Usage {
                message,
                usage: cli::debug_usage(),
            },
            Err(CliError::DebugOpenAiUsage(message)) => CliExit::Usage {
                message,
                usage: cli::debug_coding_loop_task_live_smoke_usage(),
            },
            Err(CliError::ShellUsage(message)) => CliExit::Usage {
                message,
                usage: cli::shell_usage(),
            },
            Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
        },
        CliCommand::Debug(DebugArgs {
            command:
                Some(DebugCommand::CodingLoopSubagentLiveSmoke(DebugCodingLoopSubagentLiveSmokeArgs {
                    model,
                    max_output_tokens,
                })),
            ..
        }) => match debug::coding_loop::run_subagent_live_smoke(
            sandbox_child_handoff,
            model.as_deref(),
            max_output_tokens,
            merry_config.as_ref(),
        )
        .await
        {
            Ok(()) => CliExit::Success,
            Err(CliError::BrokenPipe) => CliExit::Success,
            Err(CliError::DebugUsage(message)) => CliExit::Usage {
                message,
                usage: cli::debug_usage(),
            },
            Err(CliError::DebugOpenAiUsage(message)) => CliExit::Usage {
                message,
                usage: cli::debug_coding_loop_subagent_live_smoke_usage(),
            },
            Err(CliError::ShellUsage(message)) => CliExit::Usage {
                message,
                usage: cli::shell_usage(),
            },
            Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
        },
    }
}

enum CliExit {
    Success,
    Usage { message: String, usage: String },
    Clap(clap::Error),
    Unexpected(String),
}

fn report_cli_exit<W: io::Write>(exit: CliExit, stderr: &mut W) -> ExitCode {
    match exit {
        CliExit::Success => ExitCode::SUCCESS,
        CliExit::Usage { message, usage } => {
            writeln!(stderr, "{message}\n\n{usage}").expect("failed to write usage to stderr");
            ExitCode::from(2)
        }
        CliExit::Clap(error) => {
            let exit_code = error.exit_code();
            error.print().expect("failed to write clap output");
            ExitCode::from(exit_code as u8)
        }
        CliExit::Unexpected(message) => {
            writeln!(stderr, "{message}").expect("failed to write error to stderr");
            ExitCode::FAILURE
        }
    }
}

impl Termination for CliExit {
    fn report(self) -> ExitCode {
        report_cli_exit(self, &mut io::stderr())
    }
}

#[cfg(test)]
mod tests {
    use super::DEBUG_TOOL_NAME;
    use super::{CliError, CliExit, DEBUG_TOOL_CONTINUATION_INPUT, report_cli_exit};
    use crate::cli;
    use crate::debug::CodingLoopTaskSmokeTask;
    use crate::debug::coding_loop::{
        CodingLoopTaskSmokeFixture, assert_coding_loop_smoke_result,
        assert_coding_loop_task_smoke_result, assert_coding_loop_task_smoke_uses_small_patch,
        assert_permission_network_smoke_result, build_coding_loop_smoke_runtime,
        build_coding_loop_task_smoke_runtime, build_scripted_permission_network_smoke_runtime,
        coding_loop_process_call, coding_loop_smoke_initial_source,
        coding_loop_subagent_live_smoke_task, coding_loop_task_fixture_manifest,
        coding_loop_tool_call, coding_loop_workspace_call,
        run_smoke as run_debug_coding_loop_smoke, write_coding_loop_task_live_smoke_report,
        write_permission_network_smoke_report,
    };
    use crate::debug::openai::{
        echo_tool as debug_echo_tool, write_tool_events as write_debug_openai_tool_events,
    };
    use crate::debug::shell::{
        process_action_intent as shell_process_action_intent, run_to_writer as run_shell_to_writer,
        runtime_admission as shell_runtime_admission,
    };
    use crate::runtime_config::{automatic_compaction_config, effective_log_settings};
    use crate::runtime_events::{collect_runtime_step_events, first_pending_tool_call};
    use crate::sandbox::{
        ChildHandoff as SandboxChildHandoff, RuntimeProfile as SandboxRuntimeProfile, os,
    };
    use crate::test_support::{FakeProcessRunner, FakeProcessRunnerStep, ScriptedProvider};
    use merry_core::{
        ArtifactKind, ArtifactRef, PendingToolCall, RuntimeEvent, RuntimeEventKind, ToolCallId,
        ToolCallResult, ToolCallResultStatus, ToolName,
    };
    use merry_llm::{
        FinishReason, GenerationConfig, ModelEvent, ModelName, ModelOutput, ModelResponse,
        ModelToolCall, ModelToolCallId, ToolArguments,
    };
    use merry_runtime::{
        AcceptedLocalWorkspaceProcessAdmission, AgentLoopConfig, ArtifactContent, CheckpointId,
        CheckpointRef, CheckpointRefId, CheckpointRefManifest, CheckpointSequenceRange,
        CheckpointSourceKind, CheckpointValidationPolicy, CitationBackedCheckpoint,
        CompactedCheckpoint, CompactedCheckpointCandidate, MAX_PROCESS_OUTPUT_LIMIT_BYTES,
        ProcessEnvPolicy, Runtime, RuntimeProfile, StepContext, StepInput, ToolExecutionContext,
    };
    use merry_tool_workspace::{
        CODING_LOOP_PROCESS_TOOL, WORKSPACE_PATCH_TOOL, WORKSPACE_READ_FILE_TOOL,
        WorkspaceCodingLoopProfile, WorkspaceRuntimeProfileBuilderExt, WorkspaceToolsConfig,
    };
    use serde_json::{Map, Value};
    use std::{
        ffi::OsStr,
        io,
        path::PathBuf,
        pin::Pin,
        process::ExitCode,
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
    fn command_plan_final_output_contract_has_described_fields() {
        let contract = super::cmd::command_plan_final_output_contract()
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
    fn command_generation_prompt_treats_file_search_as_recursive_by_default() {
        let temp = tempfile::tempdir().expect("tempdir");
        let environment = super::cmd::CommandGenerationEnvironment::detect(temp.path());
        let prompt = super::cmd::command_generation_prompt("列出当前目录的 rs 文件", &environment);

        assert!(prompt.contains("recursive by default"));
        assert!(prompt.contains("find -maxdepth 1 only when the user explicitly asks"));
        assert!(prompt.contains("user's current input language"));
        assert!(prompt.contains("Runtime environment:"));
        assert!(prompt.contains(super::cmd::CHECK_COMMAND_TOOL_NAME));
        assert!(prompt.contains("prefer a single shell pipeline"));
    }

    #[test]
    fn coding_loop_subagent_live_prompt_forces_parent_delegation_and_child_patch() {
        let prompt = coding_loop_subagent_live_smoke_task();

        assert!(prompt.contains("Call `spawn_subagents` with exactly one child task"));
        assert!(prompt.contains("call `wait_subagents`"));
        assert!(prompt.contains("The parent agent must not patch"));
        assert!(prompt.contains("\"workspace_read_file\", \"workspace_patch\""));
        assert!(prompt.contains("\"subagent-output.txt\""));
        assert!(prompt.contains(super::CODING_LOOP_SUBAGENT_LIVE_SMOKE_TARGET.trim()));
    }

    #[tokio::test]
    async fn coding_loop_smoke_admission_requires_real_sandbox_handoff() {
        let err = run_debug_coding_loop_smoke(None, None)
            .await
            .expect_err("coding-loop-smoke should require real sandbox handoff");

        match err {
            CliError::DebugUsage(message) => {
                assert!(message.contains("--with-sandbox"));
                assert!(message.contains("coding-loop-smoke"));
            }
            _ => panic!("expected debug usage error"),
        }
    }

    #[tokio::test]
    async fn coding_loop_task_smoke_patches_fixture_and_verifies_with_fake_runner() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let fixture = CodingLoopTaskSmokeFixture::for_task(CodingLoopTaskSmokeTask::StatusText);
        let smoke_root = temp.path().join("coding-loop-task-smoke-fixture");
        std::fs::create_dir_all(smoke_root.join("src")).expect("fixture src dir should exist");
        std::fs::create_dir_all(smoke_root.join("tests")).expect("fixture tests dir should exist");
        std::fs::write(
            smoke_root.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[lib]\npath = \"src/lib.rs\"\n",
                fixture.package_name()
            ),
        )
        .expect("fixture Cargo.toml should write");
        std::fs::write(smoke_root.join("src/lib.rs"), fixture.initial_source())
            .expect("fixture source should write");
        std::fs::write(smoke_root.join("AGENTS.md"), fixture.agents_source())
            .expect("fixture AGENTS.md should write");
        std::fs::write(
            smoke_root.join("tests/status.rs"),
            fixture.integration_test_source(),
        )
        .expect("fixture integration test should write");
        std::fs::write(smoke_root.join("tests.md"), fixture.test_source())
            .expect("fixture test note should write");

        let runner = FakeProcessRunner::scripted([
            FakeProcessRunnerStep::success(
                "AGENTS.md\nCargo.toml\nsrc/lib.rs\ntests.md\ntests/status.rs\n",
            ),
            FakeProcessRunnerStep::failure("pattern not found\n"),
            FakeProcessRunnerStep::success("src/lib.rs:    \"done\"\n"),
        ]);
        let runtime = build_coding_loop_task_smoke_runtime(
            &smoke_root,
            None,
            AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            Arc::new(runner.clone()),
            None,
            fixture,
            merry_runtime::AutomaticCompactionConfig::default(),
        )
        .expect("coding-loop task smoke runtime should build");

        let result = runtime
            .run_agent_loop(
                StepInput::user_text(fixture.task_prompt()).expect("valid step input"),
                StepContext::default(),
                AgentLoopConfig::new(10).expect("valid loop config"),
            )
            .await
            .expect("coding-loop task smoke should run");
        assert_coding_loop_task_smoke_result(&runtime, &result, &smoke_root, fixture)
            .await
            .expect("coding-loop task smoke result should validate");
        assert_eq!(
            runner.observed_argv(),
            [
                vec!["rg".to_owned(), "--files".to_owned()],
                vec!["rg".to_owned(), "done".to_owned(), "src/lib.rs".to_owned()],
                vec!["rg".to_owned(), "done".to_owned(), "src/lib.rs".to_owned()],
            ]
        );
        assert_eq!(runner.observed_cwd(), [None, None, None]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn command_generation_runtime_is_read_only_workspace_only() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("mkdir workspace");
        let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
        })]]);

        let runtime = super::cmd::build_runtime(super::cmd::RuntimeInput {
            session_id: "cmd-generation-runtime",
            root: &workspace,
            environment: super::cmd::CommandGenerationEnvironment::detect(&workspace),
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
        assert!(tool_names.contains(&super::cmd::CHECK_COMMAND_TOOL_NAME));
        assert!(!tool_names.contains(&WORKSPACE_PATCH_TOOL));
        assert!(!tool_names.contains(&CODING_LOOP_PROCESS_TOOL));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generate_command_plan_reads_structured_final_output() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("mkdir workspace");
        let provider = ScriptedProvider::new(vec![vec![Ok(coding_loop_workspace_call(
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
        let runtime = super::cmd::build_runtime(super::cmd::RuntimeInput {
            session_id: "cmd-final-output",
            root: &workspace,
            environment: super::cmd::CommandGenerationEnvironment::detect(&workspace),
            provider: Arc::new(provider),
            model: ModelName::new("debug-model").expect("valid model name"),
            allow_hidden_workspace_paths: false,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            skill_roots: Vec::new(),
        })
        .expect("runtime should build");

        let plan = super::cmd::generate_command_plan(
            &runtime,
            "find rust files",
            &super::cmd::CommandGenerationEnvironment::detect(&workspace),
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

        let environment = super::cmd::CommandGenerationEnvironment {
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
        let provider = ScriptedProvider::new(vec![vec![Ok(coding_loop_tool_call(
            "call-check-command",
            super::cmd::CHECK_COMMAND_TOOL_NAME,
            arguments,
        )
        .expect("check command tool call should build"))]]);
        let runtime = super::cmd::build_runtime(super::cmd::RuntimeInput {
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
            .find_map(|event| match &event.kind {
                RuntimeEventKind::ToolCallResolved { result } => {
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
            ArtifactContent::Json(content) => {
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
        let plan = super::cmd::CommandPlan {
            shell_command: "printf should-not-run".to_owned(),
            notes: Vec::new(),
            cautions: Vec::new(),
        };
        let mut output = Vec::new();

        let accepted = super::cmd::prompt_execute_command_plan(
            &plan,
            tokio::io::BufReader::new("".as_bytes()),
            &mut output,
        )
        .await
        .expect("prompt should read");

        assert!(!accepted);
        assert_eq!(String::from_utf8(output).expect("utf8"), "execute? [y/N] ");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn execute_shell_command_to_writer_writes_complete_output() {
        let mut output = FlushCountingWriter::default();

        super::cmd::execute_shell_command_to_writer(
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

    #[test]
    fn coding_loop_task_live_prompt_delegates_to_default_prompt_and_agents() {
        let fixture = CodingLoopTaskSmokeFixture::for_task(CodingLoopTaskSmokeTask::StatusText);
        let prompt = fixture.live_task_prompt(None);

        assert!(prompt.contains("status-text behavior"));
        assert!(prompt.contains("inspect, edit, and verify"));
        assert!(!prompt.contains(super::CODING_LOOP_PROCESS_TOOL));
        assert!(!prompt.contains(super::WORKSPACE_READ_FILE_TOOL));
        assert!(!prompt.contains(super::WORKSPACE_PATCH_TOOL));
        assert!(!prompt.contains("*** Begin Workspace Patch"));
        assert!(!prompt.contains("src/lib.rs"));
        assert!(!prompt.contains("cargo check"));
        assert!(!prompt.contains("rg done"));
        assert!(!prompt.contains(".merry/local/coding-loop-task-live-smoke/src/lib.rs"));
    }

    #[test]
    fn coding_loop_task_status_text_fixture_forces_disambiguated_localized_patch() {
        let fixture = CodingLoopTaskSmokeFixture::for_task(CodingLoopTaskSmokeTask::StatusText);

        let initial_source = fixture.initial_source();
        let patched_source = fixture.patched_source();

        assert!(initial_source.len() > super::CODING_LOOP_TASK_SMOKE_MAX_PATCH_BYTES);
        assert!(patched_source.len() > super::CODING_LOOP_TASK_SMOKE_MAX_PATCH_BYTES);
        assert!(fixture.patch_text().len() < super::CODING_LOOP_TASK_SMOKE_MAX_PATCH_BYTES);
        assert_eq!(initial_source.matches("value: \"todo\"").count(), 3);
        assert_eq!(patched_source.matches("value: \"todo\"").count(), 2);
        assert!(patched_source.contains("Entry { key: \"default\", value: \"todo\" },"));
        assert!(patched_source.contains("Entry { key: \"status\", value: \"done\" },"));
        assert!(patched_source.contains("Entry { key: \"preview\", value: \"todo\" },"));
        assert!(fixture.source_satisfies_task(&patched_source));
        assert!(!fixture.source_satisfies_task(&initial_source));
        assert_eq!(fixture.package_name(), "merry-coding-loop-task-status-text");
        assert_eq!(fixture.crate_name(), "merry_coding_loop_task_status_text");
        let agents_source = fixture.agents_source();
        assert!(agents_source.contains("Read `tests/status.rs`"));
        assert!(agents_source.contains("cargo check -p merry-coding-loop-task-status-text"));
        assert!(agents_source.contains("cargo test -p merry-coding-loop-task-status-text"));
        let integration_test = fixture.integration_test_source();
        assert!(integration_test.contains("merry_coding_loop_task_status_text"));
        assert!(integration_test.contains("assert_eq!(status(), \"done\")"));
        assert!(
            initial_source.contains("pub fn status() -> &'static str {\n    resolve(\"status\")")
        );
        assert!(initial_source.contains(fixture.patch_remove_line()));
        assert_eq!(
            initial_source.matches(fixture.patch_remove_line()).count(),
            1
        );
        assert!(
            fixture
                .patch_text()
                .starts_with("*** Begin Workspace Patch\n")
        );
        assert!(
            fixture
                .patch_text()
                .contains("*** Update File: src/lib.rs\n")
        );
        assert_eq!(
            initial_source.replacen(fixture.patch_remove_line(), fixture.patch_add_line(), 1),
            patched_source
        );
    }

    #[test]
    fn coding_loop_task_fixture_manifest_opts_out_of_parent_workspace() {
        let fixture = CodingLoopTaskSmokeFixture::for_task(CodingLoopTaskSmokeTask::StatusText);
        let manifest = coding_loop_task_fixture_manifest(fixture);

        assert!(manifest.contains("[package]\n"));
        assert!(manifest.contains("name = \"merry-coding-loop-task-status-text\""));
        assert!(
            manifest.ends_with("\n[workspace]\n"),
            "fixture manifest must opt out of parent Cargo workspaces"
        );
    }

    #[test]
    fn coding_loop_task_patch_assertion_accepts_standard_patch_envelope_alias() {
        let fixture = CodingLoopTaskSmokeFixture::for_task(CodingLoopTaskSmokeTask::StatusText);
        let call_id = ToolCallId::new("call-standard-patch").expect("valid call id");
        let patch = "\
*** Begin Patch
*** Update File: src/lib.rs
@@
-    Entry { key: \"status\", value: \"todo\" },
+    Entry { key: \"status\", value: \"done\" },
*** End Patch";
        let pending = RuntimeEvent::new(
            merry_core::SessionId::new("coding-loop-task-live-smoke").unwrap(),
            1,
            RuntimeEventKind::ToolCallPending {
                call: PendingToolCall::new(
                    call_id.clone(),
                    ToolName::new(WORKSPACE_PATCH_TOOL).expect("valid tool name"),
                    merry_core::ToolCallArguments::new(Map::from_iter([(
                        "patch".to_owned(),
                        Value::String(patch.to_owned()),
                    )])),
                ),
            },
        );
        let resolved = RuntimeEvent::new(
            merry_core::SessionId::new("coding-loop-task-live-smoke").unwrap(),
            2,
            RuntimeEventKind::ToolCallResolved {
                result: ToolCallResult::succeeded(
                    call_id,
                    ArtifactRef::new(
                        merry_core::ArtifactId::new("tool-result-2").unwrap(),
                        ArtifactKind::Json,
                    ),
                ),
            },
        );

        assert_coding_loop_task_smoke_uses_small_patch(&[pending, resolved], fixture)
            .expect("standard patch envelope alias should pass smoke patch assertion");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn coding_loop_smoke_respects_configured_log_path_and_keeps_payloads_out_of_events() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let config_root = temp.path().join("config");
        let state_root = temp.path().join("state");
        let expected_log_path = state_root.join("merry/logs/merry.jsonl");
        let paths = super::config::XdgPaths::from_parts(
            PathBuf::from("/home/alice"),
            Some(config_root),
            Some(state_root),
        );
        let config = super::config::MerryConfig::load_optional_from_text(
            Some("[observability.log]\nenabled = true\nlevel = \"debug\"\nformat = \"json\"\n"),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");
        let log_settings = effective_log_settings(Some(&config), &paths)
            .expect("log settings should validate")
            .expect("logging should be enabled");
        assert_eq!(log_settings.path, expected_log_path);
        let smoke_root = temp.path().join("coding-loop-smoke-fixture");
        std::fs::create_dir_all(smoke_root.join("src")).expect("fixture src dir should exist");
        std::fs::write(
            smoke_root.join("Cargo.toml"),
            "[package]\nname = \"merry-coding-loop-smoke\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[lib]\npath = \"src/lib.rs\"\n",
        )
        .expect("fixture Cargo.toml should write");
        std::fs::write(
            smoke_root.join("src/lib.rs"),
            coding_loop_smoke_initial_source(),
        )
        .expect("fixture source should write");
        let runtime = build_coding_loop_smoke_runtime(
            &smoke_root,
            None,
            AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            Arc::new(FakeProcessRunner::succeeding(
                "sensitive process stdout must not leak\n",
            )),
            None,
            merry_runtime::AutomaticCompactionConfig::default(),
        )
        .expect("coding-loop smoke runtime should build");

        let result = runtime
            .run_agent_loop(
                StepInput::user_text("Run the sandboxed coding-loop smoke.")
                    .expect("valid step input"),
                StepContext::default(),
                AgentLoopConfig::new(8).expect("valid loop config"),
            )
            .await
            .expect("coding-loop smoke should run");
        assert_coding_loop_smoke_result(&runtime, &result, &smoke_root)
            .await
            .expect("coding-loop smoke result should validate");
    }

    #[test]
    fn shell_process_action_intent_uses_exact_cli_argv_and_empty_env() {
        let intent =
            match shell_process_action_intent(vec!["rustc".to_owned(), "--version".to_owned()]) {
                Ok(intent) => intent,
                Err(_) => panic!("shell process intent should be valid"),
            };

        assert_eq!(intent.argv(), ["rustc", "--version"]);
        assert_eq!(intent.cwd(), Some("."));
        assert_eq!(intent.env_policy(), ProcessEnvPolicy::empty());
        assert!(intent.stdin_text().is_none());
        assert_eq!(intent.stdout_limit_bytes(), MAX_PROCESS_OUTPUT_LIMIT_BYTES);
        assert_eq!(intent.stderr_limit_bytes(), MAX_PROCESS_OUTPUT_LIMIT_BYTES);
    }

    #[test]
    fn shell_runtime_admission_requires_accept_handoff_and_exact_sandbox_markers() {
        assert_eq!(
            shell_runtime_admission(
                true,
                Some(SandboxChildHandoff::CliBwrapV1),
                Some(SandboxRuntimeProfile::CliBwrapV1),
                Some(OsStr::new("1")),
                Some(OsStr::new("1")),
            ),
            Some(AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1())
        );

        for (accept, handoff, profile, sandbox, version) in [
            (
                false,
                Some(SandboxChildHandoff::CliBwrapV1),
                Some(SandboxRuntimeProfile::CliBwrapV1),
                Some(os("1")),
                Some(os("1")),
            ),
            (
                true,
                Some(SandboxChildHandoff::CliBwrapV1),
                None,
                Some(os("1")),
                Some(os("1")),
            ),
            (true, None, None, None, None),
            (
                false,
                Some(SandboxChildHandoff::CliBwrapV1),
                None,
                None,
                None,
            ),
            (
                true,
                None,
                Some(SandboxRuntimeProfile::CliBwrapV1),
                Some(os("1")),
                Some(os("1")),
            ),
            (
                false,
                None,
                Some(SandboxRuntimeProfile::CliBwrapV1),
                Some(os("1")),
                Some(os("1")),
            ),
            (
                true,
                Some(SandboxChildHandoff::CliBwrapV1),
                Some(SandboxRuntimeProfile::CliBwrapV1),
                None,
                Some(os("1")),
            ),
            (
                true,
                Some(SandboxChildHandoff::CliBwrapV1),
                Some(SandboxRuntimeProfile::CliBwrapV1),
                Some(os("1")),
                None,
            ),
            (
                true,
                Some(SandboxChildHandoff::CliBwrapV1),
                Some(SandboxRuntimeProfile::CliBwrapV1),
                Some(os("0")),
                Some(os("1")),
            ),
            (
                true,
                Some(SandboxChildHandoff::CliBwrapV1),
                Some(SandboxRuntimeProfile::CliBwrapV1),
                Some(os("1")),
                Some(os("2")),
            ),
            (
                true,
                Some(SandboxChildHandoff::CliBwrapV1),
                Some(SandboxRuntimeProfile::CliBwrapV1),
                Some(os("true")),
                Some(os("1")),
            ),
            (
                true,
                Some(SandboxChildHandoff::CliBwrapV1),
                Some(SandboxRuntimeProfile::CliBwrapV1),
                Some(os("1")),
                Some(os("")),
            ),
        ] {
            assert_eq!(
                shell_runtime_admission(
                    accept,
                    handoff,
                    profile,
                    sandbox.as_deref(),
                    version.as_deref(),
                ),
                None
            );
        }
    }

    #[tokio::test]
    async fn shell_helper_simulated_sandbox_runs_local_workspace_effect_with_fake_runner() {
        let intent = shell_process_action_intent(vec![
            "cargo".to_owned(),
            "test".to_owned(),
            "-p".to_owned(),
            "merry-runtime".to_owned(),
        ])
        .unwrap_or_else(|_| panic!("shell process intent should be valid"));
        let admission = shell_runtime_admission(
            true,
            Some(SandboxChildHandoff::CliBwrapV1),
            Some(SandboxRuntimeProfile::CliBwrapV1),
            Some(OsStr::new("1")),
            Some(OsStr::new("1")),
        );
        let runner = FakeProcessRunner::succeeding("simulated cargo success\n");
        let mut output = Vec::new();

        run_shell_to_writer(
            intent,
            admission,
            Arc::new(runner.clone()),
            false,
            &mut output,
        )
        .await
        .unwrap_or_else(|_| panic!("accepted local workspace shell command should resolve"));

        assert_eq!(runner.call_count(), 1);
        assert_eq!(
            runner.observed_argv(),
            vec![vec!["cargo", "test", "-p", "merry-runtime"]]
        );
        let text = String::from_utf8(output).expect("output should be utf-8");
        assert_eq!(text, "simulated cargo success\n");
    }

    #[tokio::test]
    async fn shell_helper_simulated_sandbox_marker_still_denies_forbidden_command() {
        let intent = shell_process_action_intent(vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "echo bad".to_owned(),
        ])
        .unwrap_or_else(|_| panic!("shell process intent should be valid"));
        let admission = shell_runtime_admission(
            true,
            Some(SandboxChildHandoff::CliBwrapV1),
            Some(SandboxRuntimeProfile::CliBwrapV1),
            Some(OsStr::new("1")),
            Some(OsStr::new("1")),
        );
        let runner = FakeProcessRunner::succeeding("bad\n");
        let mut output = Vec::new();

        run_shell_to_writer(
            intent,
            admission,
            Arc::new(runner.clone()),
            true,
            &mut output,
        )
        .await
        .unwrap_or_else(|_| panic!("forbidden command should resolve as a policy denial"));

        assert_eq!(runner.call_count(), 0);
        let text = String::from_utf8(output).expect("output should be utf-8");
        let events = parse_runtime_events(&text);
        let resolved = resolved_tool_result(&events);
        assert_eq!(resolved.status(), ToolCallResultStatus::Failed);
        assert_eq!(
            resolved
                .diagnostic()
                .expect("denied result should include a diagnostic")
                .code(),
            "action_policy_denied"
        );
    }

    fn parse_runtime_events(text: &str) -> Vec<RuntimeEvent> {
        assert!(
            text.ends_with('\n'),
            "runtime JSONL should end with newline"
        );
        text.lines()
            .map(|line| serde_json::from_str::<RuntimeEvent>(line).expect("line should be JSON"))
            .collect()
    }

    fn resolved_tool_result(events: &[RuntimeEvent]) -> &merry_core::ToolCallResult {
        events
            .iter()
            .find_map(|event| match &event.kind {
                merry_core::RuntimeEventKind::ToolCallResolved { result } => Some(result),
                _ => None,
            })
            .expect("shell command should resolve a tool call")
    }

    #[test]
    fn cli_exit_unexpected_reports_failure_without_usage() {
        let mut stderr = Vec::new();

        let exit_code = report_cli_exit(
            CliExit::Unexpected(
                "debug tool `debug_echo` was not called on the first step".to_owned(),
            ),
            &mut stderr,
        );

        assert_eq!(exit_code, ExitCode::FAILURE);
        let stderr = String::from_utf8(stderr).expect("stderr should be utf-8");
        assert_eq!(
            stderr,
            "debug tool `debug_echo` was not called on the first step\n"
        );
        assert!(!stderr.contains("Usage: merry debug openai"));
    }

    #[test]
    fn cli_exit_usage_reports_exit_two_and_usage() {
        let mut stderr = Vec::new();

        let exit_code = report_cli_exit(
            CliExit::Usage {
                message: "--input requires a value".to_owned(),
                usage: cli::debug_openai_usage(),
            },
            &mut stderr,
        );

        assert_eq!(exit_code, ExitCode::from(2));
        let stderr = String::from_utf8(stderr).expect("stderr should be utf-8");
        assert!(stderr.starts_with("--input requires a value\n\n"));
        assert!(stderr.contains("Usage: merry debug openai"));
        assert!(stderr.contains("MERRY_OPENAI_DEBUG=1"));
    }

    #[tokio::test]
    async fn debug_openai_tool_helper_executes_one_pending_call_and_continues() {
        let call = ModelToolCall::new(
            ModelToolCallId::new("call-debug").expect("valid tool call id"),
            ToolName::new(DEBUG_TOOL_NAME).expect("valid tool name"),
            ToolArguments::new(Map::new()),
        );
        let provider = ScriptedProvider::new(vec![
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::tool_call(call)],
                    FinishReason::ToolCalls,
                    None,
                ),
            })],
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("continued after tool")],
                    FinishReason::Stop,
                    None,
                ),
            })],
        ]);
        let runtime = Runtime::builder(merry_core::SessionId::new("debug-openai-tool").unwrap())
            .register_tool(
                debug_echo_tool("debug result").unwrap_or_else(|_| panic!("valid debug tool")),
            )
            .model_provider(
                Arc::new(provider.clone()),
                ModelName::new("debug-model").unwrap(),
            )
            .build()
            .expect("runtime should build");
        let input = StepInput::user_text("please call the tool").expect("valid input");
        let context = StepContext::default().with_generation_config(
            GenerationConfig::new(Some(16), false).expect("valid generation config"),
        );
        let mut output = Vec::new();

        write_debug_openai_tool_events(&runtime, input, context, &mut output)
            .await
            .unwrap_or_else(|_| panic!("tool events should write"));

        let text = String::from_utf8(output).expect("output should be utf-8");
        let events = text
            .lines()
            .map(|line| serde_json::from_str::<RuntimeEvent>(line).expect("line should be JSON"))
            .collect::<Vec<_>>();
        let event_types = events
            .iter()
            .map(|event| {
                let value = serde_json::to_value(event).expect("event should serialize");
                value["kind"]["type"].as_str().unwrap().to_owned()
            })
            .collect::<Vec<_>>();

        assert_eq!(
            event_types,
            [
                "session_started",
                "step_started",
                "tool_call_pending",
                "artifact_recorded",
                "tool_call_resolved",
                "step_started",
                "artifact_recorded",
                "step_completed",
            ]
        );

        let resolved = events
            .iter()
            .find_map(|event| match &event.kind {
                merry_core::RuntimeEventKind::ToolCallResolved { result } => Some(result),
                _ => None,
            })
            .expect("tool should be resolved");
        assert_eq!(resolved.status(), ToolCallResultStatus::Succeeded);

        let requests = provider.recorded_requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].tools().len(), 1);
        assert_eq!(requests[0].tools()[0].name().as_str(), DEBUG_TOOL_NAME);
        assert!(requests[0].continuations().is_empty());
        assert_eq!(requests[0].generation().max_output_tokens(), Some(16));
        assert!(!requests[0].generation().allow_parallel_tool_calls());

        assert_eq!(requests[1].tools().len(), 1);
        assert_eq!(requests[1].tools()[0].name().as_str(), DEBUG_TOOL_NAME);
        assert_eq!(requests[1].continuations().len(), 1);
        let continuation = &requests[1].continuations()[0];
        assert_eq!(continuation.call().id().as_str(), "call-debug");
        assert_eq!(
            continuation.result().status(),
            ToolCallResultStatus::Succeeded
        );
        assert_eq!(
            continuation.result().content().as_text(),
            Some("debug result")
        );
        assert!(
            requests[1]
                .messages()
                .iter()
                .any(|message| message.content().as_text() == DEBUG_TOOL_CONTINUATION_INPUT)
        );
        assert_eq!(requests[1].generation().max_output_tokens(), Some(16));
        assert!(!requests[1].generation().allow_parallel_tool_calls());
    }

    #[tokio::test]
    async fn debug_openai_tool_helper_errors_when_first_step_calls_wrong_tool() {
        let wrong_tool_name = "wrong_tool";
        let call = ModelToolCall::new(
            ModelToolCallId::new("call-wrong").expect("valid tool call id"),
            ToolName::new(wrong_tool_name).expect("valid tool name"),
            ToolArguments::new(Map::new()),
        );
        let provider = ScriptedProvider::new(vec![
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::tool_call(call)],
                    FinishReason::ToolCalls,
                    None,
                ),
            })],
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("should not continue")],
                    FinishReason::Stop,
                    None,
                ),
            })],
        ]);
        let runtime =
            Runtime::builder(merry_core::SessionId::new("debug-openai-wrong-tool").unwrap())
                .register_tool(
                    debug_echo_tool("debug result").unwrap_or_else(|_| panic!("valid debug tool")),
                )
                .model_provider(
                    Arc::new(provider.clone()),
                    ModelName::new("debug-model").unwrap(),
                )
                .build()
                .expect("runtime should build");
        let input = StepInput::user_text("please call the tool").expect("valid input");
        let mut output = Vec::new();

        let error =
            write_debug_openai_tool_events(&runtime, input, StepContext::default(), &mut output)
                .await
                .expect_err("wrong first-step tool call should fail");

        match error {
            CliError::Unexpected(message) => {
                assert!(message.contains(DEBUG_TOOL_NAME));
                assert!(message.contains(wrong_tool_name));
            }
            _ => panic!("expected unexpected error for wrong tool call"),
        }

        let text = String::from_utf8(output).expect("output should be utf-8");
        let events = text
            .lines()
            .map(|line| serde_json::from_str::<RuntimeEvent>(line).expect("line should be JSON"))
            .collect::<Vec<_>>();
        assert!(
            !events.is_empty(),
            "first-step runtime events should be preserved"
        );
        let event_types = events
            .iter()
            .map(|event| {
                let value = serde_json::to_value(event).expect("event should serialize");
                value["kind"]["type"].as_str().unwrap().to_owned()
            })
            .collect::<Vec<_>>();

        assert!(event_types.iter().any(|kind| kind == "tool_call_pending"));
        assert!(!event_types.iter().any(|kind| kind == "tool_call_resolved"));
        let pending = events
            .iter()
            .find_map(|event| match &event.kind {
                merry_core::RuntimeEventKind::ToolCallPending { call } => Some(call),
                _ => None,
            })
            .expect("wrong tool call should remain pending in first-step events");
        assert_eq!(pending.name().as_str(), wrong_tool_name);

        let requests = provider.recorded_requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].continuations().is_empty());
    }

    #[tokio::test]
    async fn debug_openai_tool_helper_errors_when_first_step_does_not_call_debug_echo() {
        let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("completed without tool")],
                FinishReason::Stop,
                None,
            ),
        })]]);
        let runtime = Runtime::builder(merry_core::SessionId::new("debug-openai-no-tool").unwrap())
            .register_tool(
                debug_echo_tool("debug result").unwrap_or_else(|_| panic!("valid debug tool")),
            )
            .model_provider(
                Arc::new(provider.clone()),
                ModelName::new("debug-model").unwrap(),
            )
            .build()
            .expect("runtime should build");
        let input = StepInput::user_text("do not call the tool").expect("valid input");
        let mut output = Vec::new();

        let error =
            write_debug_openai_tool_events(&runtime, input, StepContext::default(), &mut output)
                .await
                .expect_err("missing first-step tool call should fail");

        match error {
            CliError::Unexpected(message) => {
                assert!(message.contains(DEBUG_TOOL_NAME));
                assert!(message.contains("no tool call was pending"));
            }
            _ => panic!("expected unexpected error for missing tool call"),
        }

        let text = String::from_utf8(output).expect("output should be utf-8");
        let events = text
            .lines()
            .map(|line| serde_json::from_str::<RuntimeEvent>(line).expect("line should be JSON"))
            .collect::<Vec<_>>();
        assert!(
            !events.is_empty(),
            "first-step runtime events should be preserved"
        );
        let event_types = events
            .iter()
            .map(|event| {
                let value = serde_json::to_value(event).expect("event should serialize");
                value["kind"]["type"].as_str().unwrap().to_owned()
            })
            .collect::<Vec<_>>();

        assert_eq!(
            event_types,
            [
                "session_started",
                "step_started",
                "artifact_recorded",
                "step_completed",
            ]
        );
        assert!(!event_types.iter().any(|kind| kind == "tool_call_pending"));
        assert!(!event_types.iter().any(|kind| kind == "tool_call_resolved"));

        let requests = provider.recorded_requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].continuations().is_empty());
    }

    #[tokio::test]
    async fn task_live_smoke_report_preserves_runtime_events_on_failure() {
        let runtime =
            Runtime::builder(merry_core::SessionId::new("coding-loop-task-live-smoke").unwrap())
                .build()
                .expect("runtime should build");
        let event = RuntimeEvent::new(
            merry_core::SessionId::new("coding-loop-task-live-smoke").unwrap(),
            1,
            merry_core::RuntimeEventKind::StepStarted,
        );
        let mut output = Vec::new();

        write_coding_loop_task_live_smoke_report(
            &runtime,
            merry_runtime::AutomaticCompactionConfig::default(),
            false,
            &[event],
            &mut output,
        )
        .await
        .unwrap_or_else(|_| panic!("task live smoke report should write"));

        let text = String::from_utf8(output).expect("output should be utf-8");
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("coding-loop-task-live-smoke: failed"));
        let event = lines
            .next()
            .map(|line| serde_json::from_str::<RuntimeEvent>(line).expect("event should parse"))
            .expect("failure report should include runtime event JSONL");
        assert!(matches!(
            event.kind,
            merry_core::RuntimeEventKind::StepStarted
        ));
        let config_summary = lines
            .next()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("line should parse"))
            .expect("failure report should include compaction config summary");
        assert_eq!(
            config_summary["type"],
            serde_json::Value::String("runtime_compaction_config_summary".to_owned())
        );
        let compaction_summary = lines
            .next()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("line should parse"))
            .expect("failure report should include compaction summary");
        assert_eq!(
            compaction_summary["type"],
            serde_json::Value::String("runtime_compaction_summary".to_owned())
        );
        assert_eq!(compaction_summary["checkpoint_present"], false);
        assert!(lines.next().is_none());
    }

    #[tokio::test]
    async fn task_live_smoke_report_includes_process_artifact_preview() {
        let call_id = "call-check";
        let provider = ScriptedProvider::new(vec![vec![Ok(coding_loop_process_call(
            call_id,
            &["cargo", "check"],
            Some("."),
        )
        .expect("process call event should build"))]]);
        let runtime =
            Runtime::builder(merry_core::SessionId::new("coding-loop-task-live-smoke").unwrap())
                .model_provider(Arc::new(provider), ModelName::new("debug-model").unwrap());
        let temp = tempfile::tempdir().expect("temp dir should be created");
        let profile = WorkspaceCodingLoopProfile::new(WorkspaceToolsConfig::new(vec![
            temp.path().to_path_buf(),
        ]))
        .expect("workspace profile should build")
        .with_cli_bwrap_process_runner(
            AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            Arc::new(FakeProcessRunner::scripted([
                FakeProcessRunnerStep::failure("cargo failed\n"),
            ])),
        );
        let profile = RuntimeProfile::builder()
            .with_workspace_coding_loop(profile)
            .expect("workspace profile should apply")
            .build()
            .expect("runtime profile should build");
        let runtime = runtime
            .with_profile(profile)
            .expect("runtime profile should apply")
            .build()
            .expect("runtime should build");
        let events = collect_runtime_step_events(
            &runtime,
            StepInput::user_text("run check").expect("valid step input"),
            StepContext::default(),
        )
        .await
        .expect("step should collect process call");
        let pending = first_pending_tool_call(&events).expect("pending tool call should exist");
        let execution_events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("process call should execute");
        let mut output = Vec::new();

        write_coding_loop_task_live_smoke_report(
            &runtime,
            merry_runtime::AutomaticCompactionConfig::default(),
            false,
            &execution_events,
            &mut output,
        )
        .await
        .expect("task live smoke report should write");

        let text = String::from_utf8(output).expect("output should be utf-8");
        assert!(text.contains("\"type\":\"tool_call_resolved\""));
        assert!(text.contains("\"type\":\"process_artifact_preview\""));
        assert!(text.contains("\"call_id\":\"call-check\""));
        assert!(text.contains("\"stderr\":\"cargo failed\\n\""));
    }

    #[tokio::test]
    async fn permission_network_smoke_report_includes_tool_calls_and_process_previews() {
        let temp = tempfile::tempdir().expect("temp dir should be created");
        let runner = Arc::new(FakeProcessRunner::scripted([
            FakeProcessRunnerStep::failure("network unreachable\n"),
            FakeProcessRunnerStep::success("93.184.216.34 example.com\n"),
        ]));
        let runtime = build_scripted_permission_network_smoke_runtime(
            temp.path(),
            AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            runner.clone(),
            Arc::new(merry_runtime::StaticPermissionedProcessRunnerFactory::new(
                runner,
            )),
            merry_runtime::AutomaticCompactionConfig::default(),
        )
        .expect("permission network smoke runtime should build");
        let result = runtime
            .run_agent_loop(
                StepInput::user_text("run permission network smoke").expect("valid step input"),
                StepContext::default(),
                AgentLoopConfig::new(6).expect("valid loop config"),
            )
            .await
            .expect("permission network smoke should run");
        assert_permission_network_smoke_result(&runtime, &result)
            .await
            .expect("permission network smoke assertions should pass");

        let mut output = Vec::new();
        write_permission_network_smoke_report(&runtime, result.events(), &mut output)
            .await
            .expect("permission network smoke report should write");

        let text = String::from_utf8(output).expect("output should be utf-8");
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("permission-network-smoke: ok"));
        assert!(text.contains("\"type\":\"tool_call_pending\""));
        assert!(text.contains("\"name\":\"run_process\""));
        assert!(text.contains("\"name\":\"request_permissions\""));
        assert!(text.contains("\"network\":true"));
        assert!(text.contains("\"type\":\"tool_call_resolved\""));
        assert!(text.contains("\"type\":\"process_artifact_preview\""));
        assert!(text.contains("\"call_id\":\"permission-network-smoke-initial-network\""));
        assert!(text.contains("\"call_id\":\"permission-network-smoke-request-network\""));
        assert!(text.contains("\"stderr\":\"network unreachable\\n\""));
        assert!(text.contains("\"stdout\":\"93.184.216.34 example.com\\n\""));
    }

    #[tokio::test]
    async fn task_live_smoke_report_includes_compaction_summary_without_checkpoint_text() {
        let manifest = CheckpointRefManifest::new(
            CheckpointId::new("checkpoint-task-live-smoke").expect("valid checkpoint id"),
            vec![
                CheckpointRef::new(
                    CheckpointRefId::new("r1").expect("valid ref id"),
                    CheckpointSourceKind::UserMessage,
                    "history:1",
                    CheckpointSequenceRange::new(1, 1).expect("valid sequence range"),
                    "body[0]",
                    "sensitive old task detail should not appear in smoke report",
                )
                .expect("valid ref"),
            ],
        )
        .expect("valid manifest");
        let candidate = CompactedCheckpointCandidate::from_json(
            r#"{
              "claims": [
                {
                  "id": "c1",
                  "kind": "current_state",
                  "text": "The old task window was compacted.",
                  "refs": ["r1"]
                }
              ],
              "working_intent": null
            }"#,
        )
        .expect("valid candidate");
        let citation = CitationBackedCheckpoint::from_candidate(
            CheckpointId::new("checkpoint-task-live-smoke").expect("valid checkpoint id"),
            candidate,
            manifest,
            CheckpointValidationPolicy::default(),
        )
        .expect("valid citation-backed checkpoint");
        let checkpoint =
            CompactedCheckpoint::from_citation_backed(citation).expect("valid checkpoint");
        let runtime =
            Runtime::builder(merry_core::SessionId::new("coding-loop-task-live-smoke").unwrap())
                .compacted_checkpoint(checkpoint)
                .build()
                .expect("runtime should build");
        let mut output = Vec::new();

        write_coding_loop_task_live_smoke_report(
            &runtime,
            merry_runtime::AutomaticCompactionConfig::default(),
            true,
            &[],
            &mut output,
        )
        .await
        .expect("task live smoke report should write");

        let text = String::from_utf8(output).expect("output should be utf-8");
        assert!(text.contains("\"type\":\"runtime_compaction_summary\""));
        assert!(text.contains("\"checkpoint_present\":true"));
        assert!(text.contains("\"citation_backed\":true"));
        assert!(text.contains("\"checkpoint_id\":\"checkpoint-task-live-smoke\""));
        assert!(text.contains("\"claim_count\":1"));
        assert!(text.contains("\"ref_count\":1"));
        assert!(!text.contains("The old task window was compacted."));
        assert!(!text.contains("sensitive old task detail"));
    }

    #[tokio::test]
    async fn task_live_smoke_report_includes_effective_compaction_config_from_toml() {
        let paths = super::config::XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
        let config = super::config::MerryConfig::load_optional_from_text(
            Some(
                r#"
[runtime.auto_compaction]
enabled = true
target_output_tokens = 144
model_output_token_limit = 233
max_accepted_output_bytes = 3456
retained_raw_tail_items = 5
max_ref_excerpt_bytes = 789
max_carried_prior_refs = 10
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");
        let auto_compaction =
            automatic_compaction_config(Some(&config)).expect("auto compaction should validate");
        let runtime =
            Runtime::builder(merry_core::SessionId::new("coding-loop-task-live-smoke").unwrap())
                .build()
                .expect("runtime should build");
        let mut output = Vec::new();

        write_coding_loop_task_live_smoke_report(&runtime, auto_compaction, true, &[], &mut output)
            .await
            .expect("task live smoke report should write");

        let text = String::from_utf8(output).expect("output should be utf-8");
        assert!(text.contains("\"type\":\"runtime_compaction_config_summary\""));
        assert!(text.contains("\"auto_compaction_enabled\":true"));
        assert!(text.contains("\"target_output_tokens\":144"));
        assert!(text.contains("\"model_output_token_limit\":233"));
        assert!(text.contains("\"max_accepted_output_bytes\":3456"));
        assert!(text.contains("\"retained_raw_tail_items\":5"));
        assert!(text.contains("\"max_ref_excerpt_bytes\":789"));
        assert!(text.contains("\"max_carried_prior_refs\":10"));
    }
}
