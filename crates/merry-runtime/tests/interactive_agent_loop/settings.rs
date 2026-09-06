use crate::support::{
    models::{
        BlockingFirstProvider, RecordingProvider, completed_text_event,
        completed_text_event_with_usage, model_name,
    },
    runtime::{NoopChildFactory, session_id, wait_for_interactive_waiting},
};
use merry_core::{InteractiveRunState, RuntimeEvent};
use merry_llm::{GenerationConfig, ModelName, ModelRetryPolicy, ReasoningEffort};
use merry_runtime::{
    AgentLoopConfig, AutomaticCompactionConfig, CitationCompactionPolicy, InteractivePrimaryModel,
    InteractiveSettingsUpdate, InteractiveSubagentSettings, Runtime, StepContext, SubagentConfig,
    SubagentManager, SubagentTaskSpec, WaitMode, subagent_registered_tools,
};
use std::{num::NonZeroU64, sync::Arc};
use tokio::{
    sync::oneshot,
    time::{Duration, timeout},
};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn interactive_settings_update_changes_the_next_request_generation_config() {
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_text_event("first"))],
        vec![Ok(completed_text_event("second"))],
    ]);
    let runtime = Runtime::builder(session_id("interactive-update-generation"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime builds");
    let initial_generation = GenerationConfig::default().with_reasoning_effort(Some(
        ReasoningEffort::new("low").expect("valid reasoning effort"),
    ));
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()).with_generation_config(initial_generation),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    let _ = stream
        .next_event()
        .await
        .expect("stream error")
        .expect("waiting state");

    input.submit_next("first").await.expect("first queued");
    wait_for_interactive_waiting(&mut stream).await;
    let updated_generation = GenerationConfig::default().with_reasoning_effort(Some(
        ReasoningEffort::new("high").expect("valid reasoning effort"),
    ));
    control
        .update_settings(
            InteractiveSettingsUpdate::default().with_generation_config(updated_generation),
        )
        .await
        .expect("settings update accepted");
    input.submit_next("second").await.expect("second queued");
    wait_for_interactive_waiting(&mut stream).await;

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0]
            .generation()
            .reasoning_effort()
            .map(ReasoningEffort::as_str),
        Some("low")
    );
    assert_eq!(
        requests[1]
            .generation()
            .reasoning_effort()
            .map(ReasoningEffort::as_str),
        Some("high")
    );
}

#[tokio::test]
async fn interactive_settings_update_changes_the_next_request_primary_model() {
    let first_provider =
        RecordingProvider::new_with_steps(vec![vec![Ok(completed_text_event("first"))]]);
    let second_provider =
        RecordingProvider::new_with_steps(vec![vec![Ok(completed_text_event("second"))]]);
    let first_model = ModelName::new("fake/first").expect("valid first model");
    let second_model = ModelName::new("fake/second").expect("valid second model");
    let runtime = Runtime::builder(session_id("interactive-update-primary"))
        .model_provider(Arc::new(first_provider.clone()), first_model.clone())
        .build()
        .expect("runtime builds");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    let _ = stream
        .next_event()
        .await
        .expect("stream error")
        .expect("waiting state");

    input.submit_next("first").await.expect("first queued");
    wait_for_interactive_waiting(&mut stream).await;
    control
        .update_settings(InteractiveSettingsUpdate::default().with_primary_model(
            InteractivePrimaryModel::new(
                Arc::new(second_provider.clone()),
                second_model.clone(),
                ModelRetryPolicy::disabled(),
            ),
        ))
        .await
        .expect("settings update accepted");
    input.submit_next("second").await.expect("second queued");
    wait_for_interactive_waiting(&mut stream).await;

    let first_requests = first_provider.recorded_requests();
    let second_requests = second_provider.recorded_requests();
    assert_eq!(first_requests.len(), 1);
    assert_eq!(first_requests[0].model(), &first_model);
    assert_eq!(second_requests.len(), 1);
    assert_eq!(second_requests[0].model(), &second_model);
}

#[tokio::test]
async fn interactive_settings_update_changes_automatic_compaction_at_request_boundary() {
    let provider = RecordingProvider::new_with_steps(Vec::new());
    let runtime = Runtime::builder(session_id("interactive-update-compaction"))
        .model_provider(Arc::new(provider), model_name())
        .automatic_compaction(AutomaticCompactionConfig::disabled())
        .build()
        .expect("runtime builds");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, _input, control) = run.split();
    let _ = stream
        .next_event()
        .await
        .expect("stream error")
        .expect("waiting state");
    let policy =
        CitationCompactionPolicy::new(Some(128), Some(6144), 1).expect("valid compact policy");
    let updated = AutomaticCompactionConfig::enabled(policy);

    control
        .update_settings(InteractiveSettingsUpdate::default().with_automatic_compaction(updated))
        .await
        .expect("settings update accepted");

    assert_eq!(runtime.automatic_compaction_config().await, updated);
}

#[tokio::test]
async fn interactive_settings_update_changes_context_window_at_request_boundary() {
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_text_event_with_usage("first"))],
        vec![Ok(completed_text_event_with_usage("second"))],
    ]);
    let runtime = Runtime::builder(session_id("interactive-update-context-window"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime builds");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    let _ = stream
        .next_event()
        .await
        .expect("stream error")
        .expect("waiting state");

    input.submit_next("first").await.expect("first queued");
    wait_for_interactive_waiting(&mut stream).await;
    let fallback = runtime.usage().await.expect("fallback usage");
    let fallback_context = fallback.context.expect("fallback context");
    assert_eq!(fallback_context.resolved_model_window_tokens, 272_000);
    assert_eq!(
        fallback_context.source,
        merry_core::ContextWindowSource::Fallback
    );

    control
        .update_settings(
            InteractiveSettingsUpdate::default()
                .with_context_window_tokens(NonZeroU64::new(128_000)),
        )
        .await
        .expect("settings update accepted");
    input.submit_next("second").await.expect("second queued");
    wait_for_interactive_waiting(&mut stream).await;

    let configured = runtime.usage().await.expect("configured usage");
    let configured_context = configured.context.expect("configured context");
    assert_eq!(configured_context.resolved_model_window_tokens, 128_000);
    assert_eq!(
        configured_context.source,
        merry_core::ContextWindowSource::ExplicitConfig
    );
}

#[tokio::test]
async fn interactive_settings_update_does_not_interrupt_the_active_model_request() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let first_provider = BlockingFirstProvider::new(started_tx, release_rx);
    let second_provider =
        RecordingProvider::new_with_steps(vec![vec![Ok(completed_text_event("second"))]]);
    let first_model = ModelName::new("fake/first").expect("valid first model");
    let second_model = ModelName::new("fake/second").expect("valid second model");
    let runtime = Runtime::builder(session_id("interactive-update-running-primary"))
        .model_provider(Arc::new(first_provider.clone()), first_model.clone())
        .build()
        .expect("runtime builds");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    let _ = stream
        .next_event()
        .await
        .expect("stream error")
        .expect("waiting state");

    input.submit_next("first").await.expect("first queued");
    timeout(Duration::from_secs(1), started_rx)
        .await
        .expect("first request starts")
        .expect("start signal sent");
    timeout(
        Duration::from_secs(1),
        control.update_settings(InteractiveSettingsUpdate::default().with_primary_model(
            InteractivePrimaryModel::new(
                Arc::new(second_provider.clone()),
                second_model.clone(),
                ModelRetryPolicy::disabled(),
            ),
        )),
    )
    .await
    .expect("settings update is accepted while the request is active")
    .expect("settings update succeeds");

    assert_eq!(first_provider.recorded_requests().len(), 1);
    assert!(second_provider.recorded_requests().is_empty());
    release_tx.send(()).expect("release first request");
    wait_for_interactive_waiting(&mut stream).await;

    input.submit_next("second").await.expect("second queued");
    wait_for_interactive_waiting(&mut stream).await;

    let first_requests = first_provider.recorded_requests();
    let second_requests = second_provider.recorded_requests();
    assert_eq!(first_requests.len(), 1);
    assert_eq!(first_requests[0].model(), &first_model);
    assert_eq!(second_requests.len(), 1);
    assert_eq!(second_requests[0].model(), &second_model);
}

#[tokio::test]
async fn interactive_subagent_setting_keeps_the_tool_profile_stable() {
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_text_event("disabled"))],
        vec![Ok(completed_text_event("enabled"))],
    ]);
    let manager = SubagentManager::runtime_controlled(
        session_id("interactive-update-subagents"),
        SubagentConfig::default(),
        Arc::new(NoopChildFactory),
        false,
    );
    let [spawn, wait, cancel] =
        subagent_registered_tools(manager.clone()).expect("subagent tools build");
    let runtime = Runtime::builder(session_id("interactive-update-subagents"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .subagent_manager(manager)
        .register_tool(spawn)
        .register_tool(wait)
        .register_tool(cancel)
        .build()
        .expect("runtime builds");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    let _ = stream
        .next_event()
        .await
        .expect("stream error")
        .expect("waiting state");

    input.submit_next("first").await.expect("first queued");
    wait_for_interactive_waiting(&mut stream).await;
    control
        .update_settings(InteractiveSettingsUpdate::default().with_subagents(
            InteractiveSubagentSettings::new(true, SubagentConfig::default()),
        ))
        .await
        .expect("settings update accepted");
    input.submit_next("second").await.expect("second queued");
    wait_for_interactive_waiting(&mut stream).await;

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    let first_tools = requests[0]
        .tools()
        .iter()
        .map(|tool| tool.name().as_str())
        .collect::<Vec<_>>();
    let second_tools = requests[1]
        .tools()
        .iter()
        .map(|tool| tool.name().as_str())
        .collect::<Vec<_>>();
    assert_eq!(first_tools, second_tools);
    assert!(first_tools.contains(&"spawn_subagents"));
}

#[tokio::test]
async fn interactive_ignores_stale_subagent_wakeup_after_wait_acknowledges_completion() {
    let provider = RecordingProvider::new_with_steps(Vec::new());
    let manager = SubagentManager::new(
        session_id("interactive-stale-subagent-wakeup"),
        SubagentConfig::default(),
        Arc::new(NoopChildFactory),
    );
    let spawned = manager
        .spawn(
            vec![
                SubagentTaskSpec::new("Complete before the interactive run.", 1)
                    .expect("valid child task"),
            ],
            None,
            CancellationToken::new(),
        )
        .await
        .expect("child spawn succeeds");
    let agent_id = spawned.spawned[0].agent_id.clone();
    let wait = manager
        .wait(
            std::slice::from_ref(&agent_id),
            WaitMode::All,
            Some(Duration::from_secs(1)),
        )
        .await
        .expect("terminal child wait succeeds");
    assert!(wait.terminal);

    let runtime = Runtime::builder(session_id("interactive-stale-subagent-wakeup"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .subagent_manager(manager)
        .build()
        .expect("runtime builds");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut events, _input, control) = run.split();
    assert!(matches!(
        events.next_event().await.expect("interactive stream error"),
        Some(RuntimeEvent::InteractiveRunStateChanged {
            state: InteractiveRunState::WaitingForInput
        })
    ));

    let unexpected_event = timeout(Duration::from_millis(100), events.next_event()).await;
    assert!(
        unexpected_event.is_err(),
        "an acknowledged completion must not start another model turn: {unexpected_event:?}"
    );
    assert!(provider.recorded_requests().is_empty());

    control.close().await.expect("interactive run closes");
}
