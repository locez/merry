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
    use super::{CliError, CliExit, report_cli_exit};
    use crate::cli;
    use crate::debug::CodingLoopTaskSmokeTask;
    use crate::debug::coding_loop::{
        CodingLoopTaskSmokeFixture, assert_coding_loop_smoke_result,
        assert_coding_loop_task_smoke_result, assert_coding_loop_task_smoke_uses_small_patch,
        build_coding_loop_smoke_runtime, build_coding_loop_task_smoke_runtime,
        coding_loop_smoke_initial_source, coding_loop_subagent_live_smoke_task,
        coding_loop_task_fixture_manifest, run_smoke as run_debug_coding_loop_smoke,
    };
    use crate::runtime_config::effective_log_settings;
    use crate::test_support::{FakeProcessRunner, FakeProcessRunnerStep};
    use merry_core::{
        ArtifactKind, ArtifactRef, PendingToolCall, RuntimeEvent, RuntimeEventKind, ToolCallId,
        ToolCallResult, ToolName,
    };
    use merry_runtime::{
        AcceptedLocalWorkspaceProcessAdmission, AgentLoopConfig, StepContext, StepInput,
    };
    use merry_tool_workspace::WORKSPACE_PATCH_TOOL;
    use serde_json::{Map, Value};
    use std::{path::PathBuf, process::ExitCode, sync::Arc};

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
}
