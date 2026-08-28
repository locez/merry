use merry::__internal::{
    AgentRunMessage, InteractiveMessage, ToolInvocationContent, ToolInvocationResult,
    ToolInvocationSubmission,
};
use merry::{
    AgentBuilder, AgentLoopConfig, AgentLoopStatus, AgentProfile, AgentProfileContext,
    FileSessionStore, ModelName, ModelProvider, RuntimeEvent, SessionId,
    StructuredOutputRetryPolicy, StructuredRunResult,
};
use merry_core::{ErrorInfo, ProviderName, ToolInputSchema, ToolName, ToolSpec};
use merry_llm::testing::FakeModelProvider;
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelOutput,
    ModelProviderFuture, ModelRequest, ModelResponse, ModelStreamContext, ModelToolCall,
    ModelToolCallId, ToolArguments,
};
use merry_runtime::RegisteredTool;
use schemars::{JsonSchema, Schema};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn session_id(value: &str) -> SessionId {
    SessionId::new(value).expect("test session id should be valid")
}

fn model_name() -> ModelName {
    ModelName::new("fake/model").expect("test model name should be valid")
}

fn text_provider(text: &str) -> Arc<FakeModelProvider> {
    Arc::new(FakeModelProvider::new(vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text(text)], FinishReason::Stop, None),
    })]))
}

fn bridge_tool() -> RegisteredTool {
    bridge_tool_named("bridge_lookup")
}

fn bridge_tool_named(name: &str) -> RegisteredTool {
    let schema =
        Schema::try_from(json!({ "type": "object" })).expect("bridge schema should be valid");
    let spec = ToolSpec::new(
        ToolName::new(name).expect("bridge tool name should be valid"),
        "Request a host-side lookup.",
        ToolInputSchema::new(schema).expect("bridge tool schema should be valid"),
    )
    .expect("bridge tool spec should be valid");
    RegisteredTool::bridge(spec)
}

fn agent(provider: Arc<dyn ModelProvider>) -> merry::Agent {
    AgentBuilder::new(session_id("sdk-test"))
        .model_provider(provider, model_name())
        .build()
        .expect("test agent should build")
}

struct TestProfile;

impl AgentProfile for TestProfile {
    fn configure(&self, context: &mut AgentProfileContext) -> Result<(), merry::AgentProfileError> {
        context.loop_config(AgentLoopConfig::new(2).expect("valid test loop config"));
        Ok(())
    }
}

#[test]
fn builder_accepts_a_generic_agent_profile() {
    let agent = AgentBuilder::new(session_id("generic-profile"))
        .model_provider(text_provider("profile"), model_name())
        .profile(TestProfile)
        .expect("generic profile should apply")
        .build()
        .expect("agent should build");

    assert_eq!(agent.loop_config().max_model_turns(), 2);
    assert!(agent.profile().is_some());
}

#[test]
fn coding_profile_is_accepted_through_the_generic_profile_boundary() {
    let root = tempfile::tempdir().expect("profile workspace should be created");
    let profile = merry::profiles::coding_agent(root.path())
        .build()
        .expect("coding profile should build");

    let agent = AgentBuilder::new(session_id("coding-profile"))
        .model_provider(text_provider("profile"), model_name())
        .profile(profile)
        .expect("coding profile should apply")
        .build()
        .expect("agent should build");

    assert!(agent.profile().is_some());
}

#[derive(Debug, Deserialize, JsonSchema)]
struct LookupOrderInput {
    order_id: String,
}

#[derive(Debug, Serialize)]
struct LookupOrderOutput {
    status: String,
}

#[tokio::test]
async fn typed_profile_tool_is_executed_by_run_without_host_handoff() {
    let root = tempfile::tempdir().expect("profile workspace should be created");
    let executions = Arc::new(AtomicUsize::new(0));
    let execution_counter = Arc::clone(&executions);
    let lookup_order = merry::Tool::new(
        "lookup_order",
        "Look up the current order status.",
        move |input: LookupOrderInput| {
            let execution_counter = Arc::clone(&execution_counter);
            async move {
                execution_counter.fetch_add(1, Ordering::SeqCst);
                Ok::<LookupOrderOutput, std::convert::Infallible>(LookupOrderOutput {
                    status: format!("order {} is ready", input.order_id),
                })
            }
        },
    )
    .expect("typed tool should build");
    let profile = merry::profiles::coding_agent(root.path())
        .tool(lookup_order)
        .build()
        .expect("coding profile should build");
    let call = ModelToolCall::new(
        ModelToolCallId::new("call-typed-tool").expect("test call id should be valid"),
        ToolName::new("lookup_order").expect("tool name should be valid"),
        ToolArguments::try_from(json!({"order_id": "A-42"}))
            .expect("tool arguments should be an object"),
    );
    let provider = Arc::new(FakeModelProvider::new_turns(vec![
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(call)],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("order checked")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]));
    let agent = AgentBuilder::new(session_id("typed-tool-run"))
        .model_provider(provider, model_name())
        .profile(profile)
        .expect("coding profile should apply")
        .build()
        .expect("agent should build");

    let result = agent
        .run("Check order A-42")
        .await
        .expect("run should complete");

    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.final_output(), Some("order checked"));
    assert!(result.events().iter().any(|event| matches!(
        event,
        RuntimeEvent::ToolCallFinished { result, .. }
            if result.status() == merry_core::ToolCallResultStatus::Succeeded
    )));
}

#[tokio::test]
async fn typed_profile_tool_is_executed_inside_event_only_stream() {
    let root = tempfile::tempdir().expect("profile workspace should be created");
    let executions = Arc::new(AtomicUsize::new(0));
    let execution_counter = Arc::clone(&executions);
    let lookup_order = merry::Tool::new(
        "lookup_order_stream",
        "Look up the current order status.",
        move |input: LookupOrderInput| {
            let execution_counter = Arc::clone(&execution_counter);
            async move {
                execution_counter.fetch_add(1, Ordering::SeqCst);
                Ok::<LookupOrderOutput, std::convert::Infallible>(LookupOrderOutput {
                    status: format!("order {} is ready", input.order_id),
                })
            }
        },
    )
    .expect("typed tool should build");
    let profile = merry::profiles::coding_agent(root.path())
        .tool(lookup_order)
        .build()
        .expect("coding profile should build");
    let call = ModelToolCall::new(
        ModelToolCallId::new("call-typed-stream-tool").expect("test call id should be valid"),
        ToolName::new("lookup_order_stream").expect("tool name should be valid"),
        ToolArguments::try_from(json!({"order_id": "A-42"}))
            .expect("tool arguments should be an object"),
    );
    let provider = Arc::new(FakeModelProvider::new_turns(vec![
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(call)],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("order streamed")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]));
    let agent = AgentBuilder::new(session_id("typed-tool-stream"))
        .model_provider(provider, model_name())
        .profile(profile)
        .expect("coding profile should apply")
        .build()
        .expect("agent should build");

    let mut stream = agent
        .stream("Check order A-42")
        .expect("event-only stream should start");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await.expect("event stream should advance") {
        events.push(event);
    }
    let result = stream
        .result()
        .await
        .expect("event-only stream should complete");

    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.final_output(), Some("order streamed"));
    assert_eq!(events, result.events());
}

#[tokio::test]
async fn typed_tool_domain_error_is_recorded_and_model_loop_continues() {
    let root = tempfile::tempdir().expect("profile workspace should be created");
    let failing_tool = merry::Tool::new(
        "lookup_order_failure",
        "Look up an order that may be unavailable.",
        |_input: LookupOrderInput| async {
            Err::<LookupOrderOutput, &'static str>("order service unavailable")
        },
    )
    .expect("typed tool should build");
    let profile = merry::profiles::coding_agent(root.path())
        .tool(failing_tool)
        .build()
        .expect("coding profile should build");
    let call = ModelToolCall::new(
        ModelToolCallId::new("call-typed-tool-failure").expect("test call id should be valid"),
        ToolName::new("lookup_order_failure").expect("tool name should be valid"),
        ToolArguments::try_from(json!({"order_id": "A-42"}))
            .expect("tool arguments should be an object"),
    );
    let provider = Arc::new(FakeModelProvider::new_turns(vec![
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(call)],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("failure handled")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]));
    let agent = AgentBuilder::new(session_id("typed-tool-failure"))
        .model_provider(provider, model_name())
        .profile(profile)
        .expect("coding profile should apply")
        .build()
        .expect("agent should build");

    let result = agent
        .run("Check order A-42")
        .await
        .expect("run should complete");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.final_output(), Some("failure handled"));
    assert!(result.events().iter().any(|event| matches!(
        event,
        RuntimeEvent::ToolCallFinished { result, .. }
            if result.status() == merry_core::ToolCallResultStatus::Failed
                && result.diagnostic().is_some_and(|info| info.code() == "tool_handler_failed")
    )));
}

#[tokio::test]
async fn run_returns_public_events_and_terminal_result() {
    let agent = agent(text_provider("hello"));

    let result = agent.run("say hello").await.expect("run should complete");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.final_output(), Some("hello"));
    assert!(result.events().iter().any(|event| matches!(
        event,
        RuntimeEvent::AssistantMessage { text, .. } if text == "hello"
    )));
}

#[tokio::test]
async fn stream_result_projects_the_same_public_contract() {
    let agent = agent(text_provider("streamed"));
    let mut stream = agent.stream("say streamed").expect("stream should start");
    let mut events = Vec::new();

    while let Some(event) = stream.next().await.expect("driver should advance") {
        events.push(event);
    }
    let result = stream
        .result()
        .await
        .expect("stream result should complete");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.final_output(), Some("streamed"));
    assert_eq!(events, result.events());
}

#[tokio::test]
async fn bridge_requests_are_explicit_driver_messages() {
    let call = ModelToolCall::new(
        ModelToolCallId::new("call-bridge").expect("test call id should be valid"),
        ToolName::new("bridge_lookup").expect("bridge tool name should be valid"),
        ToolArguments::try_from(json!({})).expect("bridge arguments should be an object"),
    );
    let provider = Arc::new(FakeModelProvider::new_turns(vec![
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(call)],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("bridge complete")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]));
    let agent = AgentBuilder::new(session_id("bridge-driver"))
        .model_provider(provider, model_name())
        .allow_bridge_tools()
        .register_tool(bridge_tool())
        .build()
        .expect("bridge agent should build");
    let mut stream = agent
        .stream_with_tool_handoff("use the bridge")
        .expect("stream should start");

    let mut saw_started = false;
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while let Some(message) = stream.next().await.expect("driver should advance") {
            match message {
                AgentRunMessage::Event(event) => {
                    saw_started |= matches!(
                        event.as_ref(),
                        RuntimeEvent::ToolCallStarted { call, .. }
                            if call.id().as_str() == "call-bridge"
                    );
                }
                AgentRunMessage::ToolInvocations { mut batch } => {
                    assert!(saw_started);
                    assert_eq!(batch.len(), 1);
                    assert_eq!(batch.invocations()[0].name().as_str(), "bridge_lookup");
                    let call_id = batch.invocations()[0].id().clone();
                    batch
                        .submit(vec![ToolInvocationResult::succeeded(
                            call_id,
                            ToolInvocationContent::json(r#"{"found":true}"#)
                                .expect("bridge result JSON should be valid"),
                        )])
                        .await
                        .expect("host result should be accepted");
                    break;
                }
                _ => panic!("unexpected future run message variant"),
            }
        }
    })
    .await
    .expect("bridge request should be emitted");

    let mut saw_finished = false;
    let mut saw_final_output = false;
    while let Some(message) = stream.next().await.expect("driver should advance") {
        match message {
            AgentRunMessage::Event(event) => {
                saw_finished |= matches!(
                    event.as_ref(),
                    RuntimeEvent::ToolCallFinished { result, .. }
                    if result.status() == merry_core::ToolCallResultStatus::Succeeded
                );
                if let RuntimeEvent::ToolCallFinished { result, .. } = event.as_ref() {
                    assert!(result.artifact().id().as_str().starts_with("tool-result-"));
                }
                saw_final_output |= matches!(
                    event.as_ref(),
                    RuntimeEvent::AssistantMessage { text, .. } if text == "bridge complete"
                );
            }
            AgentRunMessage::ToolInvocations { .. } => {
                panic!("the test provider should issue only one tool invocation batch")
            }
            _ => panic!("unexpected future run message variant"),
        }
    }
    let result = stream.result().await.expect("bridge run should complete");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.final_output(), Some("bridge complete"));
    assert!(saw_finished);
    assert!(saw_final_output);
}

#[tokio::test]
async fn bridge_invocations_are_delivered_as_one_ordered_batch() {
    let first_call = ModelToolCall::new(
        ModelToolCallId::new("call-bridge-1").expect("test call id should be valid"),
        ToolName::new("bridge_lookup").expect("bridge tool name should be valid"),
        ToolArguments::try_from(json!({})).expect("bridge arguments should be an object"),
    );
    let second_call = ModelToolCall::new(
        ModelToolCallId::new("call-bridge-2").expect("test call id should be valid"),
        ToolName::new("bridge_lookup").expect("bridge tool name should be valid"),
        ToolArguments::try_from(json!({})).expect("bridge arguments should be an object"),
    );
    let provider = Arc::new(FakeModelProvider::new_turns(vec![
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![
                    ModelOutput::tool_call(first_call),
                    ModelOutput::tool_call(second_call),
                ],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("batch complete")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]));
    let agent = AgentBuilder::new(session_id("bridge-batch-driver"))
        .model_provider(provider, model_name())
        .allow_bridge_tools()
        .register_tool(bridge_tool())
        .build()
        .expect("bridge agent should build");
    let mut stream = agent
        .stream_with_tool_handoff("use both bridge calls")
        .expect("stream should start");

    loop {
        let message = stream
            .next()
            .await
            .expect("driver should advance")
            .expect("driver should emit a message");
        match message {
            AgentRunMessage::Event(_) => {}
            AgentRunMessage::ToolInvocations { mut batch } => {
                assert_eq!(
                    batch
                        .invocations()
                        .iter()
                        .map(|invocation| invocation.id().as_str())
                        .collect::<Vec<_>>(),
                    ["call-bridge-1", "call-bridge-2"]
                );

                let incomplete = vec![ToolInvocationResult::succeeded(
                    batch.invocations()[0].id().clone(),
                    ToolInvocationContent::text("first"),
                )];
                assert!(matches!(
                    batch.submit(incomplete).await,
                    Err(merry::AgentError::ToolInvocationBatchMismatch { .. })
                ));

                let results = batch
                    .invocations()
                    .iter()
                    .rev()
                    .map(|invocation| {
                        ToolInvocationResult::succeeded(
                            invocation.id().clone(),
                            ToolInvocationContent::json(format!(
                                r#"{{"resolved":"{}"}}"#,
                                invocation.id().as_str()
                            ))
                            .expect("bridge result JSON should be valid"),
                        )
                    })
                    .collect();
                batch
                    .submit(results)
                    .await
                    .expect("complete bridge result batch should be accepted");
                break;
            }
            _ => panic!("unexpected future run message variant"),
        }
    }

    let mut finished_ids = Vec::new();
    while let Some(message) = stream.next().await.expect("driver should advance") {
        match message {
            AgentRunMessage::Event(event) => {
                if let RuntimeEvent::ToolCallFinished { result, .. } = event.as_ref() {
                    finished_ids.push(result.call_id().as_str().to_owned());
                }
            }
            AgentRunMessage::ToolInvocations { .. } => {
                panic!("the test provider should issue only one invocation batch")
            }
            _ => panic!("unexpected future run message variant"),
        }
    }

    let result = stream.result().await.expect("bridge run should complete");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.final_output(), Some("batch complete"));
    assert_eq!(finished_ids, ["call-bridge-1", "call-bridge-2"]);
}

#[tokio::test]
async fn bridge_submission_validation_error_keeps_batch_open_for_retry() {
    let call = ModelToolCall::new(
        ModelToolCallId::new("call-bridge-retry").expect("test call id should be valid"),
        ToolName::new("bridge_lookup").expect("bridge tool name should be valid"),
        ToolArguments::try_from(json!({})).expect("bridge arguments should be an object"),
    );
    let provider = Arc::new(FakeModelProvider::new_turns(vec![
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(call)],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("bridge retry complete")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]));
    let agent = AgentBuilder::new(session_id("bridge-retry"))
        .model_provider(provider, model_name())
        .allow_bridge_tools()
        .register_tool(bridge_tool())
        .build()
        .expect("bridge agent should build");
    let mut driver = agent
        .stream_with_tool_handoff("retry the bridge result")
        .expect("stream should start");

    loop {
        let message = tokio::time::timeout(std::time::Duration::from_secs(1), driver.next())
            .await
            .expect("bridge retry should not deadlock")
            .expect("driver should advance")
            .expect("driver should emit a message");
        if let AgentRunMessage::ToolInvocations { mut batch } = message {
            let call_id = batch.invocations()[0].id().clone();
            let invalid = batch
                .submit(vec![ToolInvocationResult::succeeded(
                    call_id.clone(),
                    ToolInvocationContent::text(""),
                )])
                .await;
            assert!(matches!(
                invalid,
                Err(merry::AgentError::Runtime {
                    source: merry_runtime::RuntimeError::UnsupportedToolResultContent { .. }
                })
            ));

            batch
                .submit(vec![ToolInvocationResult::succeeded(
                    call_id,
                    ToolInvocationContent::text("resolved"),
                )])
                .await
                .expect("corrected bridge result should be accepted");
            break;
        }
    }

    while driver
        .next()
        .await
        .expect("driver should continue after corrected result")
        .is_some()
    {}
    let result = driver.result().await.expect("bridge retry should complete");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.final_output(), Some("bridge retry complete"));
}

#[tokio::test]
async fn bridge_domain_failure_is_recorded_and_loop_continues() {
    let call = ModelToolCall::new(
        ModelToolCallId::new("call-bridge-failure").expect("test call id should be valid"),
        ToolName::new("bridge_lookup").expect("bridge tool name should be valid"),
        ToolArguments::try_from(json!({"key": "missing"}))
            .expect("bridge arguments should be an object"),
    );
    let provider = Arc::new(FakeModelProvider::new_turns(vec![
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(call)],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("failure handled")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]));
    let agent = AgentBuilder::new(session_id("bridge-failure"))
        .model_provider(provider, model_name())
        .allow_bridge_tools()
        .register_tool(bridge_tool())
        .build()
        .expect("bridge agent should build");
    let mut driver = agent
        .stream_with_tool_handoff("handle the missing lookup")
        .expect("driver should start");

    loop {
        let message = driver
            .next()
            .await
            .expect("driver should advance")
            .expect("driver should emit a tool invocation batch");
        if let AgentRunMessage::ToolInvocations { mut batch } = message {
            let call_id = batch.invocations()[0].id().clone();
            let diagnostic =
                ErrorInfo::new("lookup_not_found", "the requested lookup was not found")
                    .expect("test diagnostic should be valid");
            let submission = batch
                .submit(vec![ToolInvocationResult::failed(
                    call_id,
                    ToolInvocationContent::json(r#"{"found":false}"#)
                        .expect("bridge result JSON should be valid"),
                    diagnostic,
                )])
                .await
                .expect("failed host result should be accepted");
            assert_eq!(submission, ToolInvocationSubmission::Accepted);
            break;
        }
    }

    let mut saw_failed_result = false;
    while let Some(message) = driver.next().await.expect("driver should advance") {
        if let AgentRunMessage::Event(event) = message {
            saw_failed_result |= matches!(
                event.as_ref(),
                RuntimeEvent::ToolCallFinished { result, .. }
                    if result.status() == merry_core::ToolCallResultStatus::Failed
                        && result.diagnostic().is_some_and(|info| info.code() == "lookup_not_found")
            );
        }
    }
    let result = driver
        .result()
        .await
        .expect("failed tool run should continue");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.final_output(), Some("failure handled"));
    assert!(saw_failed_result);
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct StructuredAnswer {
    #[schemars(description = "Short answer for the caller.")]
    answer: String,
}

#[tokio::test]
async fn structured_run_builds_schema_and_decodes_recorded_output() {
    let call = ModelToolCall::new(
        ModelToolCallId::new("call-final").expect("test call id should be valid"),
        ToolName::new("merry_final_output").expect("final output tool name should be valid"),
        ToolArguments::try_from(serde_json::json!({"answer": "typed"}))
            .expect("test arguments should be an object"),
    );
    let provider = Arc::new(FakeModelProvider::new(vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
            None,
        ),
    })]));
    let agent = agent(provider);

    let result: StructuredRunResult<StructuredAnswer> = agent
        .run_structured("return a typed answer")
        .await
        .expect("structured run should complete");

    assert_eq!(result.output().answer, "typed");
    assert_eq!(result.run().status(), &AgentLoopStatus::Completed);
}

#[tokio::test]
async fn structured_run_rejects_non_object_schema_before_starting_provider() {
    let provider = Arc::new(FakeModelProvider::new(vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::text("provider should not be called")],
            FinishReason::Stop,
            None,
        ),
    })]));
    let agent = agent(provider);

    let error = agent
        .run_structured::<String>("return a scalar")
        .await
        .expect_err("scalar structured output should be rejected before the run");

    assert!(matches!(
        error,
        merry::AgentError::FinalOutputContract {
            source: merry_runtime::FinalOutputContractError::RootSchemaMustBeObject,
        }
    ));
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct StrictStructuredAnswer {
    #[schemars(description = "Numeric answer for the caller.")]
    answer: u64,
}

fn final_output_call(id: &str, answer: serde_json::Value) -> ModelToolCall {
    ModelToolCall::new(
        ModelToolCallId::new(id).expect("test call id should be valid"),
        ToolName::new("merry_final_output").expect("final output tool name should be valid"),
        ToolArguments::try_from(json!({"answer": answer}))
            .expect("final output arguments should be an object"),
    )
}

#[tokio::test]
async fn structured_output_retry_stays_in_one_run_and_recovers() {
    let provider = Arc::new(FakeModelProvider::new_turns(vec![
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(final_output_call(
                    "call-final-invalid",
                    json!("not-a-number"),
                ))],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(final_output_call(
                    "call-final-valid",
                    json!(42),
                ))],
                FinishReason::ToolCalls,
                None,
            ),
        })],
    ]));
    let agent = AgentBuilder::new(session_id("structured-retry"))
        .model_provider(provider, model_name())
        .structured_output_retry_policy(StructuredOutputRetryPolicy::new(1))
        .build()
        .expect("structured retry agent should build");

    let result: StructuredRunResult<StrictStructuredAnswer> = agent
        .run_structured("return a numeric answer")
        .await
        .expect("structured output should recover after one retry");

    assert_eq!(result.output().answer, 42);
    assert_eq!(result.run().status(), &AgentLoopStatus::Completed);
    assert_eq!(result.run().model_turns_run(), 2);
    assert!(result.run().events().iter().any(|event| matches!(
        event,
        RuntimeEvent::ToolCallFinished { result, .. }
            if result.status() == merry_core::ToolCallResultStatus::Failed
    )));
}

#[tokio::test]
async fn structured_output_failure_retains_run_when_retries_are_exhausted() {
    let provider = Arc::new(FakeModelProvider::new(vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::tool_call(final_output_call(
                "call-final-exhausted",
                json!("not-a-number"),
            ))],
            FinishReason::ToolCalls,
            None,
        ),
    })]));
    let agent = AgentBuilder::new(session_id("structured-retry-exhausted"))
        .model_provider(provider, model_name())
        .structured_output_retry_policy(StructuredOutputRetryPolicy::disabled())
        .build()
        .expect("structured retry agent should build");

    let error = agent
        .run_structured::<StrictStructuredAnswer>("return a numeric answer")
        .await
        .expect_err("invalid structured output should be reported");
    let merry::AgentError::StructuredOutputNotRecorded { run } = error else {
        panic!("structured failure should retain its run result");
    };

    assert!(matches!(run.status(), AgentLoopStatus::Failed { .. }));
    assert_eq!(run.model_turns_run(), 1);
    assert!(run.events().iter().any(|event| matches!(
        event,
        RuntimeEvent::ToolCallFinished { result, .. }
            if result.status() == merry_core::ToolCallResultStatus::Failed
    )));
}

#[derive(Debug)]
struct PendingProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
}

impl PendingProvider {
    fn new() -> Self {
        Self {
            name: ProviderName::new("pending-test-provider")
                .expect("test provider name should be valid"),
            capabilities: ModelCapabilities::new(true, false, false, false, None, None)
                .expect("test capabilities should be valid"),
        }
    }
}

impl ModelProvider for PendingProvider {
    fn name(&self) -> &ProviderName {
        &self.name
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn stream_model<'a>(
        &'a self,
        _request: ModelRequest,
        _context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async {
            let stream: ModelEventStream = Box::pin(futures_util::stream::pending());
            Ok(stream)
        })
    }
}

#[tokio::test]
async fn cancelling_a_stream_waits_for_a_cancelled_terminal_result() {
    let agent = agent(Arc::new(PendingProvider::new()));
    let mut stream = agent
        .stream("wait until cancelled")
        .expect("stream should start");

    let result = tokio::time::timeout(std::time::Duration::from_secs(1), stream.cancel())
        .await
        .expect("cancellation should settle");
    let result = result.expect("cancel should return a runtime result");

    assert!(matches!(result.status(), AgentLoopStatus::Cancelled { .. }));
}

#[tokio::test]
async fn cancelling_a_stream_with_pending_bridge_request_returns_cancelled_result() {
    let call = ModelToolCall::new(
        ModelToolCallId::new("call-bridge-cancel").expect("test call id should be valid"),
        ToolName::new("bridge_lookup").expect("bridge tool name should be valid"),
        ToolArguments::try_from(json!({})).expect("bridge arguments should be an object"),
    );
    let provider = Arc::new(FakeModelProvider::new(vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
            None,
        ),
    })]));
    let agent = AgentBuilder::new(session_id("bridge-cancel"))
        .model_provider(provider, model_name())
        .allow_bridge_tools()
        .register_tool(bridge_tool())
        .build()
        .expect("bridge agent should build");
    let mut driver = agent
        .stream_with_tool_handoff("cancel the pending bridge")
        .expect("driver should start");

    let result = loop {
        let message = tokio::time::timeout(std::time::Duration::from_secs(1), driver.next())
            .await
            .expect("tool invocation batch should be emitted")
            .expect("driver should advance")
            .expect("driver should emit a tool invocation batch");
        if let AgentRunMessage::ToolInvocations { batch } = message {
            break tokio::time::timeout(std::time::Duration::from_secs(1), batch.cancel())
                .await
                .expect("bridge cancellation should settle")
                .expect("cancel should return a runtime result");
        }
    };

    assert!(matches!(result.status(), AgentLoopStatus::Cancelled { .. }));
}

#[tokio::test]
async fn dropping_an_unresolved_invocation_batch_requests_cancellation() {
    let call = ModelToolCall::new(
        ModelToolCallId::new("call-bridge-drop").expect("test call id should be valid"),
        ToolName::new("bridge_lookup").expect("bridge tool name should be valid"),
        ToolArguments::try_from(json!({})).expect("bridge arguments should be an object"),
    );
    let provider = Arc::new(FakeModelProvider::new(vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
            None,
        ),
    })]));
    let agent = AgentBuilder::new(session_id("bridge-drop"))
        .model_provider(provider, model_name())
        .allow_bridge_tools()
        .register_tool(bridge_tool())
        .build()
        .expect("bridge agent should build");
    let mut driver = agent
        .stream_with_tool_handoff("drop the pending bridge")
        .expect("driver should start");

    loop {
        let message = tokio::time::timeout(std::time::Duration::from_secs(1), driver.next())
            .await
            .expect("tool invocation batch should be emitted")
            .expect("driver should advance")
            .expect("driver should emit a message");
        match message {
            AgentRunMessage::Event(_) => {}
            AgentRunMessage::ToolInvocations { batch } => {
                drop(batch);
                break;
            }
            _ => panic!("unexpected future run message variant"),
        }
    }

    let result = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while driver
            .next()
            .await
            .expect("driver should settle after dropped batch")
            .is_some()
        {}
        driver.result().await
    })
    .await
    .expect("dropped batch should not leave the producer waiting");
    let result = result.expect("cancelled run should return a result");
    assert!(matches!(result.status(), AgentLoopStatus::Cancelled { .. }));
}

#[tokio::test]
async fn interactive_run_exposes_shared_handles_and_public_events() {
    let agent = agent(text_provider("interactive"));
    let run = agent
        .start_interactive()
        .expect("interactive run should start");
    let (mut events, input, control) = run.split();

    assert_eq!(input.run_id(), control.run_id());
    let first = tokio::time::timeout(std::time::Duration::from_secs(1), events.next())
        .await
        .expect("interactive run should announce its initial state")
        .expect("interactive stream should remain healthy")
        .expect("interactive stream should remain open");
    assert!(matches!(
        first,
        RuntimeEvent::InteractiveRunStateChanged { .. }
    ));

    input
        .submit_next("say hello")
        .await
        .expect("interactive input should be accepted");
    let saw_output = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while let Some(event) = events
            .next()
            .await
            .expect("interactive stream should remain healthy")
        {
            if matches!(
                event,
                RuntimeEvent::AssistantMessage { ref text, .. } if text == "interactive"
            ) {
                return true;
            }
        }
        false
    })
    .await
    .expect("interactive run should emit model output");
    assert!(saw_output);

    control
        .close()
        .await
        .expect("interactive run should close cleanly");
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        events.wait_until_closed(),
    )
    .await
    .expect("interactive stream should reach its closed state")
    .expect("interactive stream should close cleanly");
}

#[tokio::test]
async fn interactive_facade_uses_the_same_ordered_bridge_batch_contract() {
    let first_call = ModelToolCall::new(
        ModelToolCallId::new("interactive-call-1").expect("test call id should be valid"),
        ToolName::new("bridge_lookup").expect("bridge tool name should be valid"),
        ToolArguments::try_from(json!({"key": "first"}))
            .expect("bridge arguments should be an object"),
    );
    let second_call = ModelToolCall::new(
        ModelToolCallId::new("interactive-call-2").expect("test call id should be valid"),
        ToolName::new("bridge_lookup").expect("bridge tool name should be valid"),
        ToolArguments::try_from(json!({"key": "second"}))
            .expect("bridge arguments should be an object"),
    );
    let provider = Arc::new(FakeModelProvider::new_turns(vec![
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![
                    ModelOutput::tool_call(first_call),
                    ModelOutput::tool_call(second_call),
                ],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("interactive bridge complete")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]));
    let agent = AgentBuilder::new(session_id("interactive-bridge-facade"))
        .model_provider(provider, model_name())
        .allow_bridge_tools()
        .register_tool(bridge_tool())
        .build()
        .expect("interactive bridge agent should build");
    let run = agent
        .start_interactive()
        .expect("interactive bridge run should start");
    let (mut stream, input, control) = run.split();

    let _ = stream
        .next()
        .await
        .expect("initial interactive message should be readable")
        .expect("interactive stream should remain open");
    input
        .submit_next("resolve both bridge calls")
        .await
        .expect("interactive input should be accepted");

    let mut batch = loop {
        let message =
            tokio::time::timeout(std::time::Duration::from_secs(1), stream.next_message())
                .await
                .expect("interactive bridge batch should be emitted")
                .expect("interactive stream should remain healthy")
                .expect("interactive stream should remain open");
        match message {
            InteractiveMessage::Event(_) => {}
            InteractiveMessage::ToolInvocations { batch } => break batch,
            _ => panic!("unexpected future interactive message variant"),
        }
    };
    assert_eq!(
        batch
            .invocations()
            .iter()
            .map(|invocation| invocation.id().as_str())
            .collect::<Vec<_>>(),
        ["interactive-call-1", "interactive-call-2"]
    );

    let results = batch
        .invocations()
        .iter()
        .rev()
        .map(|invocation| {
            ToolInvocationResult::succeeded(
                invocation.id().clone(),
                ToolInvocationContent::text(format!("result for {}", invocation.name())),
            )
        })
        .collect();
    batch
        .submit(results)
        .await
        .expect("complete interactive bridge batch should be accepted");
    drop(batch);

    let mut finished = 0;
    let mut saw_final_output = false;
    loop {
        let Some(message) =
            tokio::time::timeout(std::time::Duration::from_secs(1), stream.next_message())
                .await
                .expect("interactive continuation should not deadlock")
                .expect("interactive stream should remain healthy")
        else {
            break;
        };
        match message {
            InteractiveMessage::Event(event) => match event.as_ref() {
                RuntimeEvent::ToolCallFinished { .. } => finished += 1,
                RuntimeEvent::AssistantMessage { text, .. }
                    if text == "interactive bridge complete" =>
                {
                    saw_final_output = true
                }
                RuntimeEvent::InteractiveRunStateChanged { .. } if saw_final_output => break,
                _ => {}
            },
            InteractiveMessage::ToolInvocations { .. } => {
                panic!("interactive emitted a second handoff before continuation completed")
            }
            _ => panic!("unexpected future interactive message variant"),
        }
    }
    assert_eq!(finished, 2);
    assert!(saw_final_output);

    control
        .close()
        .await
        .expect("interactive bridge run should close");
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        stream.wait_until_closed(),
    )
    .await
    .expect("interactive stream should close")
    .expect("interactive stream should close cleanly");
}

#[tokio::test]
async fn builder_resumes_a_saved_session_with_the_same_contract() {
    let temp = tempfile::tempdir().expect("session store directory should be created");
    let store = FileSessionStore::new(temp.path());
    let session = session_id("resume-test");
    let first_agent = AgentBuilder::new(session.clone())
        .model_provider(text_provider("saved"), model_name())
        .session_store(store.clone())
        .build()
        .expect("first agent should build");

    first_agent
        .run("create persisted state")
        .await
        .expect("first run should complete");
    first_agent
        .save_session()
        .await
        .expect("session should be saved");

    let resumed = AgentBuilder::new(session)
        .model_provider(text_provider("resumed"), model_name())
        .resume_from_store(store)
        .await
        .expect("saved session should resume");

    assert_eq!(resumed.session_id().as_str(), "resume-test");
    assert!(resumed.run("continue").await.is_ok());
}

#[test]
fn building_without_a_primary_provider_is_rejected() {
    let error = match AgentBuilder::new(session_id("missing-provider")).build() {
        Ok(_) => panic!("provider-less agent should be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.to_string(), "a primary model provider is required");
}

#[test]
fn typed_tool_rejects_the_runtime_final_output_name() {
    let error = merry::Tool::new(
        merry_runtime::FINAL_OUTPUT_TOOL_NAME,
        "Application tool using a reserved name.",
        |_input: StructuredAnswer| async { Ok::<String, String>(String::new()) },
    )
    .expect_err("runtime-owned final output name must be rejected");

    assert!(matches!(
        error,
        merry::ToolBuildError::ReservedName { name }
            if name.as_str() == merry_runtime::FINAL_OUTPUT_TOOL_NAME
    ));
}

#[test]
fn runtime_rejects_a_reserved_final_output_tool_name() {
    let result = merry_runtime::Runtime::builder(session_id("reserved-final-output-tool"))
        .register_tool(bridge_tool_named(merry_runtime::FINAL_OUTPUT_TOOL_NAME))
        .build();

    assert!(matches!(
        result,
        Err(merry_runtime::RuntimeError::ReservedToolName { name })
            if name.as_str() == merry_runtime::FINAL_OUTPUT_TOOL_NAME
    ));
}
