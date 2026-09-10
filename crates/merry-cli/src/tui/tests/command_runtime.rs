use crate::{
    coding::{
        CodingSubagentsConfig, HeadlessCodingRuntimeInput, build_headless_coding,
        fixed_process_backend,
    },
    testing::{ScriptedProvider, model_name, process_tool_call},
    tui::{
        keymap::Keymap, projector::TuiProjector, render::render_to_text, state::TuiState,
        theme::TuiTheme,
    },
};
use merry_core::{InteractiveRunState, RuntimeEvent};
use merry_llm::{FinishReason, ModelEvent, ModelOutput, ModelResponse};
use merry_process::ProcessSession;
use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, AgentLoopConfig, AutomaticCompactionConfig,
    ProcessActionIntent, ProcessExitStatus, ProcessRunner, ProcessRunnerContext,
    ProcessRunnerError, ProcessRunnerFuture, ProcessRunnerOutput,
    StaticPermissionedProcessRunnerFactory, StepContext,
};
use std::{sync::Arc, time::Duration};
use tokio::sync::Notify;

struct GatedProcessRunner {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

impl GatedProcessRunner {
    fn new(started: Arc<Notify>, release: Arc<Notify>) -> Self {
        Self { started, release }
    }
}

impl ProcessRunner for GatedProcessRunner {
    fn run<'a>(
        &'a self,
        intent: ProcessActionIntent,
        context: ProcessRunnerContext,
    ) -> ProcessRunnerFuture<'a> {
        Box::pin(async move {
            self.started.notify_one();
            tokio::select! {
                _ = self.release.notified() => {}
                _ = context.cancellation_token().cancelled() => {
                    return Err(ProcessRunnerError::Cancelled);
                }
            }
            ProcessRunnerOutput::new(
                &intent,
                ProcessExitStatus::Exited(0),
                "done",
                false,
                "",
                false,
            )
            .map_err(|error| ProcessRunnerError::infrastructure(error.to_string()))
        })
    }
}

#[tokio::test(start_paused = true)]
async fn runtime_process_stays_running_and_animates_until_the_backend_completes() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let workspace = tempfile::tempdir().expect("workspace should exist");
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let runner: Arc<dyn ProcessRunner> = Arc::new(GatedProcessRunner::new(
            Arc::clone(&started),
            Arc::clone(&release),
        ));
        let permissioned_factory = Arc::new(StaticPermissionedProcessRunnerFactory::new(
            Arc::clone(&runner),
        ));
        let provider = ScriptedProvider::new(vec![
            vec![Ok(process_tool_call(
                "running-process",
                &["pwd"],
                Some("."),
            )
            .unwrap())],
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("finished")],
                    FinishReason::Stop,
                    None,
                ),
            })],
        ]);
        let runtime = build_headless_coding(HeadlessCodingRuntimeInput {
            session_id: "tui-running-process",
            root: workspace.path(),
            provider: Arc::new(provider),
            model: model_name(),
            process_backend: fixed_process_backend(ProcessSession::from_parts(
                AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace(),
                runner,
                permissioned_factory,
            )),
            extra_tools: Vec::new(),
            allow_hidden_workspace_paths: false,
            automatic_compaction: AutomaticCompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            approval_review: None,
            skill_roots: Vec::new(),
            subagents: CodingSubagentsConfig::default(),
            workspace_tool_limits: None,
        })
        .expect("coding runtime should build");
        let run = runtime
            .start_interactive_agent_run(StepContext::default(), AgentLoopConfig::default())
            .expect("interactive run should start");
        let (mut stream, input, control) = run.split();
        let mut state = TuiState::new(
            workspace.path().to_owned(),
            "test-model".to_owned(),
            Keymap::default(),
            TuiTheme::default(),
        );
        let mut projector = TuiProjector::default();
        input
            .submit_next("Print the current directory.")
            .await
            .expect("input should be accepted");

        while let Some(event) = stream
            .next_event()
            .await
            .expect("runtime stream should succeed")
        {
            let running_tool = matches!(
                event,
                RuntimeEvent::InteractiveRunStateChanged {
                    state: InteractiveRunState::RunningTool,
                }
            );
            projector.apply(event, &mut state);
            if running_tool {
                break;
            }
        }
        started.notified().await;
        let first = render_to_text(&state, 80, 24);
        tokio::time::advance(Duration::from_millis(100)).await;
        let second = render_to_text(&state, 80, 24);
        let active_before_completion = state.is_active_run();

        release.notify_one();
        while let Some(event) = stream
            .next_event()
            .await
            .expect("runtime stream should succeed")
        {
            let completed = matches!(event, RuntimeEvent::ToolCallFinished { .. });
            projector.apply(event, &mut state);
            if completed {
                break;
            }
        }
        let finished = render_to_text(&state, 80, 24);
        control.close().await.expect("runtime should close");
        while stream
            .next_event()
            .await
            .expect("runtime should drain")
            .is_some()
        {}

        let effect = crate::tui::controller::handle_key_action(
            crate::tui::keymap::KeyAction::OpenCommandDetails,
            &mut state,
        );
        let crate::tui::controller::ControllerEffect::LoadCommandOutput(artifact_id) = effect
        else {
            panic!("completed runtime command should expose its output artifact");
        };
        let output = crate::tui::command_details::load_output(&runtime, &artifact_id).await;
        crate::tui::command_details::apply_output(&mut state, &artifact_id, output);
        let inspected = render_to_text(&state, 100, 24);
        assert!(inspected.contains("stdout"), "{inspected}");
        assert!(inspected.contains("done"), "{inspected}");

        assert!(active_before_completion);
        assert!(
            first.lines().any(|line| line.starts_with(" Running ")),
            "{first}"
        );
        assert!(first.contains("pwd (.)"), "{first}");
        assert!(!first.contains(" Ran pwd"));
        assert_ne!(first, second);
        assert!(
            second.lines().any(|line| line.starts_with(" Running ")),
            "{second}"
        );
        assert!(finished.contains(" Ran pwd (.)"), "{finished}");
        assert!(!finished.lines().any(|line| line.starts_with(" Running ")));
    })
    .await
    .expect("runtime command lifecycle should finish within the deadline");
}
