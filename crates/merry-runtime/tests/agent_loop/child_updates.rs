use crate::support::{
    models::{ScriptedModelProvider, completed_text_event, model_name},
    runtime::session_id,
};
use merry_runtime::{
    AgentLoopConfig, AgentLoopStatus, ChildRuntimeFactory, ChildRuntimeInput, Runtime,
    RuntimeError, StepContext, StepInput, SubagentConfig, SubagentManager, SubagentStatusLabel,
    SubagentTaskSpec,
};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

struct FailingChildFactory;

impl ChildRuntimeFactory for FailingChildFactory {
    fn build_child(&self, _input: ChildRuntimeInput) -> Result<Runtime, RuntimeError> {
        Err(RuntimeError::InvalidStepInput {
            reason: "synthetic child startup failure",
        })
    }
}

fn runtime_with_provider_and_subagent_manager(
    session: &str,
    provider: ScriptedModelProvider,
    manager: SubagentManager,
) -> Runtime {
    Runtime::builder(session_id(session))
        .subagent_manager(manager)
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime with subagent manager should build")
}

#[tokio::test(flavor = "current_thread")]
async fn child_terminal_update_is_delivered_before_deferred_parent_input() {
    let parent_session = session_id("agent-loop-child-notification");
    let manager = SubagentManager::new(
        parent_session,
        SubagentConfig::default(),
        Arc::new(FailingChildFactory),
    );
    let spawned = manager
        .spawn(
            vec![
                SubagentTaskSpec::new("Finish before the parent turn.", 1)
                    .expect("valid child task"),
            ],
            None,
            CancellationToken::new(),
        )
        .await
        .expect("child spawn succeeds");
    let child_id = spawned.spawned[0].agent_id.clone();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let status = manager
                .snapshot()
                .await
                .into_iter()
                .find(|status| status.agent_id == child_id)
                .expect("child remains tracked");
            if status.status == SubagentStatusLabel::Failed {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("child should reach a terminal state");

    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_text_event("runtime update handled"))],
        vec![Ok(completed_text_event("parent input handled"))],
    ]);
    let runtime = runtime_with_provider_and_subagent_manager(
        "agent-loop-child-notification",
        provider.clone(),
        manager,
    );
    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Continue the parent task.").expect("valid parent input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::new(3).expect("valid loop budget"),
        )
        .await
        .expect("parent loop should complete");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 2);
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].messages().iter().any(|message| {
        message
            .content()
            .as_text()
            .contains("Runtime update: child agents reached terminal states")
    }));
    assert!(requests[0].messages().iter().any(|message| {
        message.content().as_text().contains(child_id.as_str())
            && message.content().as_text().contains("failed")
    }));
    assert!(
        requests[1]
            .messages()
            .iter()
            .any(|message| message.content().as_text() == "Continue the parent task.")
    );
}
