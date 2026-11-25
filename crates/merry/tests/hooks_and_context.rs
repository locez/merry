use merry::{
    AfterModelDecision, AfterToolDecision, AgentBuilder, AgentLoopBlockedReason, AgentLoopConfig,
    AgentLoopStatus, BeforeModelDecision, BeforeToolDecision, FileSessionStore, GenerationConfig,
    HookDecision, HookPoint, HookRegistry, HookRequest, HookSubscriptions, ModelName, Tool,
    ToolHookFilter,
    binding::{OwnedAgentRunMessage, ToolInvocationContent, ToolInvocationResult},
};
use merry_core::{
    ContextWindowSource, SessionId, ToolCallArguments, ToolInputSchema, ToolName, ToolOutput,
};
use merry_llm::{
    FinishReason, ModelCapabilities, ModelEvent, ModelOutput, ModelResponse, ModelToolCall,
    ModelToolCallId, ToolArguments, testing::FakeModelProvider,
};
use schemars::{JsonSchema, Schema};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    num::NonZeroU64,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Debug, Deserialize, JsonSchema)]
struct EchoArgs {
    value: String,
}

#[derive(Debug, Serialize)]
struct EchoOut {
    echoed: String,
}

fn text_turn(text: &str) -> Vec<Result<ModelEvent, merry_llm::ModelError>> {
    vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::text(text)],
            FinishReason::Stop,
            Some(merry_core::ModelUsage::new(100, 20)),
        ),
    })]
}

fn tool_calls_turn(
    calls: Vec<(&str, &str, serde_json::Value)>,
) -> Vec<Result<ModelEvent, merry_llm::ModelError>> {
    let outputs = calls
        .into_iter()
        .map(|(id, name, args)| {
            ModelOutput::tool_call(ModelToolCall::new(
                ModelToolCallId::new(id).expect("valid call id"),
                ToolName::new(name).expect("valid tool name"),
                ToolArguments::try_from(args).expect("valid tool arguments"),
            ))
        })
        .collect();
    vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            outputs,
            FinishReason::ToolCalls,
            Some(merry_core::ModelUsage::new(100, 20)),
        ),
    })]
}

fn echo_tool(name: &str, counter: Arc<AtomicUsize>) -> Tool {
    Tool::new(name, "Echo input value", move |args: EchoArgs| {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok::<EchoOut, String>(EchoOut { echoed: args.value })
        }
    })
    .expect("valid tool")
}

fn permissive_schema() -> ToolInputSchema {
    let schema = Schema::try_from(serde_json::json!({
        "type": "object",
        "additionalProperties": true
    }))
    .expect("valid schema");
    ToolInputSchema::new(schema).expect("valid input schema")
}

#[tokio::test]
async fn context_window_precedence_explicit_overrides_provider_and_fallback() {
    let model = ModelName::new("test-model").expect("valid model");

    // 1. Fallback (272k) when neither provider nor explicit override is set
    let fallback_provider = Arc::new(FakeModelProvider::new_turns(vec![text_turn("ok")]));
    let fallback_agent = AgentBuilder::new(SessionId::new("cw-fallback").expect("sid"))
        .model_provider(fallback_provider, model.clone())
        .build()
        .expect("build");
    let res = fallback_agent.run("hi").await.expect("run");
    let ctx = res
        .session_usage()
        .and_then(|u| u.context)
        .expect("context usage present");
    assert_eq!(ctx.resolved_model_window_tokens, 272_000);
    assert_eq!(ctx.source, ContextWindowSource::Fallback);

    // 2. Provider capabilities (128k) overrides fallback
    let caps = ModelCapabilities::new(true, true, true, true, None, None)
        .expect("caps")
        .with_max_input_tokens(NonZeroU64::new(128_000).expect("nz"));
    let provider_128k = Arc::new(FakeModelProvider::with_turns_and_capabilities(
        vec![text_turn("ok")],
        caps.clone(),
    ));
    let provider_agent = AgentBuilder::new(SessionId::new("cw-provider").expect("sid"))
        .model_provider(provider_128k, model.clone())
        .build()
        .expect("build");
    let res = provider_agent.run("hi").await.expect("run");
    let ctx = res
        .session_usage()
        .and_then(|u| u.context)
        .expect("context usage present");
    assert_eq!(ctx.resolved_model_window_tokens, 128_000);
    assert_eq!(ctx.source, ContextWindowSource::ProviderCapabilities);

    // 3. Explicit AgentBuilder override (64k) overrides provider capabilities (128k)
    let provider_override = Arc::new(FakeModelProvider::with_turns_and_capabilities(
        vec![text_turn("ok")],
        caps,
    ));
    let explicit_agent = AgentBuilder::new(SessionId::new("cw-explicit").expect("sid"))
        .model_provider(provider_override, model)
        .context_window_tokens(NonZeroU64::new(64_000).expect("nz"))
        .build()
        .expect("build");
    let res = explicit_agent.run("hi").await.expect("run");
    let ctx = res
        .session_usage()
        .and_then(|u| u.context)
        .expect("context usage present");
    assert_eq!(ctx.resolved_model_window_tokens, 64_000);
    assert_eq!(ctx.source, ContextWindowSource::ExplicitConfig);
}

#[tokio::test]
async fn model_step_hooks_support_steering_continue_and_stop_turn_without_breaking_prefix() {
    let model = ModelName::new("test-model").expect("valid model");
    let provider = Arc::new(FakeModelProvider::new_turns(vec![
        text_turn("draft 1"),
        text_turn("final answer"),
    ]));
    let seen_turns = Arc::new(AtomicUsize::new(0));
    let seen_turns_after = Arc::clone(&seen_turns);

    let hooks = HookRegistry::new()
        .before_model_step(|ctx| async move {
            let gen_override = GenerationConfig::new(Some(512), false).expect("gen");
            BeforeModelDecision::Continue {
                generation_override: Some(gen_override),
                dynamic_steering_note: Some(format!("turn-note-{}", ctx.model_turn)),
                force_compact: false,
            }
        })
        .after_model_step(move |ctx| {
            let seen_turns_after = Arc::clone(&seen_turns_after);
            async move {
                let count = seen_turns_after.fetch_add(1, Ordering::SeqCst) + 1;
                if count == 1 && ctx.is_final_turn {
                    return AfterModelDecision::continue_with("Please refine your answer");
                }
                AfterModelDecision::accept()
            }
        });

    let agent = AgentBuilder::new(SessionId::new("model-hooks").expect("sid"))
        .model_provider(Arc::clone(&provider) as _, model.clone())
        .hooks(hooks)
        .build()
        .expect("build");
    let result = agent.run("start").await.expect("run");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 2);
    assert_eq!(result.final_output(), Some("final answer"));

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    // Stable prefix must stay identical across turns (KV cache preserved)
    assert_eq!(
        requests[0].stable_prefix_messages(),
        requests[1].stable_prefix_messages()
    );
    assert_eq!(requests[0].generation().max_output_tokens(), Some(512));

    // Verify before_model_step StopTurn skips calling the provider altogether
    let stop_provider = Arc::new(FakeModelProvider::new_turns(vec![text_turn("never")]));
    let stop_hooks = HookRegistry::new().before_model_step(|_ctx| async move {
        BeforeModelDecision::stop_turn("budget guard", Some("stopped early".to_owned()))
    });
    let stop_agent = AgentBuilder::new(SessionId::new("model-hook-stop").expect("sid"))
        .model_provider(Arc::clone(&stop_provider) as _, model.clone())
        .hooks(stop_hooks)
        .build()
        .expect("build");
    let stop_res = stop_agent.run("start").await.expect("run");
    assert_eq!(
        stop_res.status(),
        &AgentLoopStatus::StoppedByHook {
            hook_point: HookPoint::BeforeModelStep,
            reason: "budget guard".to_owned(),
        }
    );
    assert_eq!(stop_res.model_turns_run(), 0);
    assert_eq!(stop_res.final_output(), Some("stopped early"));
    assert!(stop_provider.recorded_requests().is_empty());

    // Verify runaway ContinueWithInput respects max_model_turns
    let loop_provider = Arc::new(FakeModelProvider::new_turns(vec![
        text_turn("a"),
        text_turn("b"),
    ]));
    let loop_hooks = HookRegistry::new()
        .after_model_step(|_ctx| async move { AfterModelDecision::continue_with("keep going") });
    let loop_agent = AgentBuilder::new(SessionId::new("model-hook-limit").expect("sid"))
        .model_provider(loop_provider, model)
        .loop_config(AgentLoopConfig::new(2).expect("cfg"))
        .hooks(loop_hooks)
        .build()
        .expect("build");
    let limit_res = loop_agent.run("start").await.expect("run");
    assert_eq!(
        limit_res.status(),
        &AgentLoopStatus::Blocked {
            reason: AgentLoopBlockedReason::MaxModelTurnsReached { max_model_turns: 2 },
        }
    );
}

#[tokio::test]
async fn tool_hooks_modify_deny_short_circuit_and_stop_turn_durably_settles_batch() {
    let model = ModelName::new("test-model").expect("valid model");
    let exec_count = Arc::new(AtomicUsize::new(0));

    // Turn 1: 3 calls -> modify_input, deny, short_circuit; Turn 2: done
    let provider = Arc::new(FakeModelProvider::new_turns(vec![
        tool_calls_turn(vec![
            ("c1", "echo_mod", serde_json::json!({"value": "raw"})),
            ("c2", "echo_deny", serde_json::json!({"value": "bad"})),
            ("c3", "echo_sc", serde_json::json!({"value": "cached"})),
        ]),
        text_turn("all handled"),
    ]));

    let hooks =
        HookRegistry::new()
            .before_tool_call(|ctx| async move {
                match ctx.tool_name.as_str() {
                    "echo_mod" => BeforeToolDecision::modify_input(
                        ToolCallArguments::try_from(serde_json::json!({"value": "rewritten"}))
                            .expect("args"),
                    ),
                    "echo_deny" => BeforeToolDecision::deny("blocked by test policy"),
                    "echo_sc" => BeforeToolDecision::short_circuit(ToolOutput::text("from-cache")),
                    _ => BeforeToolDecision::allow(),
                }
            })
            .after_tool_call_for(
                [ToolName::new("echo_mod").expect("tn")],
                |_ctx| async move {
                    AfterToolDecision::modify_output(ToolOutput::text("redacted-output"))
                },
            );

    let agent = AgentBuilder::new(SessionId::new("tool-hooks").expect("sid"))
        .model_provider(Arc::clone(&provider) as _, model.clone())
        .tool(echo_tool("echo_mod", Arc::clone(&exec_count)))
        .tool(echo_tool("echo_deny", Arc::clone(&exec_count)))
        .tool(echo_tool("echo_sc", Arc::clone(&exec_count)))
        .hooks(hooks)
        .build()
        .expect("build");
    let res = agent.run("run tools").await.expect("run");
    assert_eq!(res.status(), &AgentLoopStatus::Completed);
    // Only echo_mod actually executed; echo_deny and echo_sc were skipped
    assert_eq!(exec_count.load(Ordering::SeqCst), 1);

    // Verify StopTurn in before_tool_call skips ALL calls in a 2-call batch and leaves session saveable
    let dir = tempfile::tempdir().expect("tempdir");
    let stop_count = Arc::new(AtomicUsize::new(0));
    let stop_provider = Arc::new(FakeModelProvider::new_turns(vec![tool_calls_turn(vec![
        ("s1", "echo_a", serde_json::json!({"value": "1"})),
        ("s2", "echo_b", serde_json::json!({"value": "2"})),
    ])]));
    let stop_hooks = HookRegistry::new().before_tool_call(|ctx| async move {
        if ctx.tool_name.as_str() == "echo_a" {
            BeforeToolDecision::stop_turn("handoff required", Some("paused for user".to_owned()))
        } else {
            BeforeToolDecision::allow()
        }
    });
    let stop_agent = AgentBuilder::new(SessionId::new("tool-hook-stop").expect("sid"))
        .model_provider(stop_provider, model)
        .tool(echo_tool("echo_a", Arc::clone(&stop_count)))
        .tool(echo_tool("echo_b", Arc::clone(&stop_count)))
        .session_store(FileSessionStore::new(dir.path()))
        .hooks(stop_hooks)
        .build()
        .expect("build");
    let stop_res = stop_agent.run("trigger stop").await.expect("run");
    assert_eq!(
        stop_res.status(),
        &AgentLoopStatus::StoppedByHook {
            hook_point: HookPoint::BeforeToolCall,
            reason: "handoff required".to_owned(),
        }
    );
    assert_eq!(stop_res.final_output(), Some("paused for user"));
    assert_eq!(stop_count.load(Ordering::SeqCst), 0);
    // Session must be 100% resume-safe with no lingering pending tool calls!
    stop_agent
        .save_session()
        .await
        .expect("session after hook StopTurn must save cleanly");
}

#[tokio::test]
async fn modify_input_revalidates_schema_and_after_tool_stop_turn_halts_loop() {
    let model = ModelName::new("test-model").expect("valid model");

    // 1. ModifyInput returning schema-invalid arguments (missing required `value: String`)
    //    must fail schema validation cleanly without executing the tool.
    let invalid_exec_count = Arc::new(AtomicUsize::new(0));
    let invalid_provider = Arc::new(FakeModelProvider::new_turns(vec![
        tool_calls_turn(vec![(
            "c1",
            "echo_checked",
            serde_json::json!({"value": "ok"}),
        )]),
        text_turn("recovered from invalid args"),
    ]));
    let invalid_hooks = HookRegistry::new().before_tool_call(|_ctx| async move {
        BeforeToolDecision::modify_input(
            ToolCallArguments::try_from(serde_json::json!({"wrong_field": 123})).expect("args"),
        )
    });
    let invalid_agent = AgentBuilder::new(SessionId::new("tool-hook-invalid-mod").expect("sid"))
        .model_provider(invalid_provider, model.clone())
        .tool(echo_tool("echo_checked", Arc::clone(&invalid_exec_count)))
        .hooks(invalid_hooks)
        .build()
        .expect("build");
    let invalid_res = invalid_agent.run("test validation").await.expect("run");
    assert_eq!(invalid_res.status(), &AgentLoopStatus::Completed);
    assert_eq!(
        invalid_res.final_output(),
        Some("recovered from invalid args")
    );
    assert_eq!(invalid_exec_count.load(Ordering::SeqCst), 0);

    // 2. AfterToolDecision::StopTurn commits the tool result and stops without a second LLM call.
    let after_exec_count = Arc::new(AtomicUsize::new(0));
    let after_provider = Arc::new(FakeModelProvider::new_turns(vec![
        tool_calls_turn(vec![(
            "a1",
            "echo_after",
            serde_json::json!({"value": "v"}),
        )]),
        text_turn("must not be called"),
    ]));
    let after_hooks = HookRegistry::new().after_tool_call(|_ctx| async move {
        AfterToolDecision::stop_turn("post-tool halt", Some("final from after hook".to_owned()))
    });
    let after_agent = AgentBuilder::new(SessionId::new("tool-hook-after-stop").expect("sid"))
        .model_provider(Arc::clone(&after_provider) as _, model)
        .tool(echo_tool("echo_after", Arc::clone(&after_exec_count)))
        .hooks(after_hooks)
        .build()
        .expect("build");
    let after_res = after_agent.run("run once then stop").await.expect("run");
    assert_eq!(
        after_res.status(),
        &AgentLoopStatus::StoppedByHook {
            hook_point: HookPoint::AfterToolCall,
            reason: "post-tool halt".to_owned(),
        }
    );
    assert_eq!(after_res.final_output(), Some("final from after hook"));
    assert_eq!(after_exec_count.load(Ordering::SeqCst), 1);
    assert_eq!(after_provider.recorded_requests().len(), 1);
}

#[tokio::test]
async fn unsubscribed_bridge_hooks_emit_zero_hook_invocations_and_filtered_only_emits_matches() {
    let model = ModelName::new("test-model").expect("valid model");

    // 1. No subscriptions -> zero HookInvocation messages
    let provider_none = Arc::new(FakeModelProvider::new_turns(vec![
        tool_calls_turn(vec![("c1", "alpha", serde_json::json!({}))]),
        text_turn("done"),
    ]));
    let agent_none = AgentBuilder::new(SessionId::new("bridge-none").expect("sid"))
        .model_provider(provider_none, model.clone())
        .bridge_tool(
            ToolName::new("alpha").expect("tn"),
            "alpha tool",
            permissive_schema(),
        )
        .expect("bridge")
        .build()
        .expect("build");
    let mut run = agent_none
        .stream_with_owned_tool_handoff("go")
        .expect("stream");
    let mut hook_count = 0usize;
    while let Some(msg) = run.next_message().await.expect("next") {
        match msg {
            OwnedAgentRunMessage::HookInvocation { .. } => hook_count += 1,
            OwnedAgentRunMessage::ToolInvocations { batch } => {
                let call = &batch.invocations()[0];
                run.submit_tool_invocation_results(
                    batch.id(),
                    vec![ToolInvocationResult::succeeded(
                        call.id().clone(),
                        ToolInvocationContent::text("ok"),
                    )],
                )
                .await
                .expect("submit");
            }
            OwnedAgentRunMessage::Event(_) => {}
            _ => {}
        }
    }
    assert_eq!(hook_count, 0);
    assert_eq!(
        run.result().await.expect("result").status(),
        &AgentLoopStatus::Completed
    );

    // 2. Subscribed ONLY to before_tool_call for "target": "other" must NOT emit a HookInvocation
    let provider_filtered = Arc::new(FakeModelProvider::new_turns(vec![
        tool_calls_turn(vec![
            ("c1", "other", serde_json::json!({})),
            ("c2", "target", serde_json::json!({})),
        ]),
        text_turn("done"),
    ]));
    let mut filter_set = BTreeSet::new();
    filter_set.insert(ToolName::new("target").expect("tn"));
    let subs = HookSubscriptions::none().with_before_tool_call(ToolHookFilter::Only(filter_set));

    let agent_filtered = AgentBuilder::new(SessionId::new("bridge-filtered").expect("sid"))
        .model_provider(provider_filtered, model)
        .bridge_tool(
            ToolName::new("other").expect("tn"),
            "other",
            permissive_schema(),
        )
        .expect("bridge")
        .bridge_tool(
            ToolName::new("target").expect("tn"),
            "target",
            permissive_schema(),
        )
        .expect("bridge")
        .bridge_hook_subscriptions(subs)
        .build()
        .expect("build");

    let mut run = agent_filtered
        .stream_with_owned_tool_handoff("go")
        .expect("stream");
    let mut seen_hook_tools = Vec::new();
    while let Some(msg) = run.next_message().await.expect("next") {
        match msg {
            OwnedAgentRunMessage::HookInvocation { invocation } => {
                if let HookRequest::BeforeToolCall(ctx) = invocation.request() {
                    seen_hook_tools.push(ctx.tool_name.as_str().to_owned());
                }
                run.submit_hook_decision(
                    invocation.id(),
                    HookDecision::BeforeToolCall(BeforeToolDecision::allow()),
                )
                .await
                .expect("submit hook");
            }
            OwnedAgentRunMessage::ToolInvocations { batch } => {
                let results = batch
                    .invocations()
                    .iter()
                    .map(|call| {
                        ToolInvocationResult::succeeded(
                            call.id().clone(),
                            ToolInvocationContent::text("ok"),
                        )
                    })
                    .collect();
                run.submit_tool_invocation_results(batch.id(), results)
                    .await
                    .expect("submit tools");
            }
            OwnedAgentRunMessage::Event(_) => {}
            _ => {}
        }
    }
    assert_eq!(seen_hook_tools, vec!["target".to_owned()]);
    assert_eq!(
        run.result().await.expect("result").status(),
        &AgentLoopStatus::Completed
    );
}
