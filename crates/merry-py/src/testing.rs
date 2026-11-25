//! Deterministic native factories used by Python parity tests.

use crate::{agent::PyAgent, error};
use merry::{
    AgentBuilder, FileSessionStore, GenerationConfig, ModelName, ParallelToolCalls, PromptBlock,
    PromptProfile, ReasoningEffort, ServiceTier,
};
use merry_core::{ProviderName, SessionId, ToolInputSchema, ToolName, ToolSpec};
use merry_llm::testing::FakeModelProvider;
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelOutput,
    ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse, ModelStreamContext,
    ModelToolCall, ModelToolCallId, ToolArguments,
};
use pyo3::{prelude::*, types::PyModule};
use schemars::Schema;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(test_agent_with_fake_response, module)?)?;
    module.add_function(wrap_pyfunction!(
        test_agent_with_streamed_text_delta,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        test_agent_with_scripted_tool_call,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        test_agent_with_parallel_tool_calls,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(test_agent_with_final_output, module)?)?;
    module.add_function(wrap_pyfunction!(test_agent_with_pending_response, module)?)?;
    module.add_function(wrap_pyfunction!(test_agent_with_scripted_turns, module)?)?;
    module.add_function(wrap_pyfunction!(test_agent_echoing_request, module)?)?;
    module.add_function(wrap_pyfunction!(test_agent_with_context_and_hooks, module)?)?;
    Ok(())
}

#[pyfunction]
fn test_agent_with_fake_response(session_id: String, final_text: String) -> PyResult<PyAgent> {
    let provider = Arc::new(FakeModelProvider::new(vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::text(&final_text)],
            FinishReason::Stop,
            None,
        ),
    })]));
    build_agent(session_id, provider)
}

#[pyfunction]
fn test_agent_with_streamed_text_delta(
    session_id: String,
    delta: String,
    final_text: String,
) -> PyResult<PyAgent> {
    let provider = Arc::new(FakeModelProvider::new(vec![
        Ok(ModelEvent::OutputTextDelta { delta }),
        Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text(&final_text)],
                FinishReason::Stop,
                None,
            ),
        }),
    ]));
    build_agent(session_id, provider)
}

#[pyfunction]
fn test_agent_with_scripted_tool_call(
    session_id: String,
    tool_name: String,
    arguments_json: String,
    final_text: String,
) -> PyResult<PyAgent> {
    let session_id = SessionId::new(&session_id)
        .map_err(|error| error::config_message_to_py(error.to_string()))?;
    let model_name = ModelName::new("fake-model")
        .map_err(|error| error::config_message_to_py(error.to_string()))?;
    let tool_name = ToolName::new(&tool_name)
        .map_err(|error| error::config_message_to_py(error.to_string()))?;
    let arguments = parse_tool_arguments(&arguments_json)?;
    let call_id = ModelToolCallId::new("test-call")
        .map_err(|error| error::protocol_message_to_py(error.to_string()))?;
    let call = ModelToolCall::new(call_id, tool_name.clone(), arguments);
    let first_turn = vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
            None,
        ),
    })];
    let second_turn = vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::text(&final_text)],
            FinishReason::Stop,
            None,
        ),
    })];
    let provider = Arc::new(FakeModelProvider::new_turns(vec![first_turn, second_turn]));
    let spec = permissive_bridge_tool(tool_name)?;
    let agent = AgentBuilder::new(session_id)
        .model_provider(provider, model_name)
        .bridge_tool(
            spec.name().clone(),
            spec.description(),
            spec.input_schema().clone(),
        )
        .map_err(error::agent_build_error_to_py)?
        .build()
        .map_err(error::agent_build_error_to_py)?;
    Ok(PyAgent::new(agent, None))
}

#[derive(Deserialize)]
struct ScriptedCallSpec {
    id: String,
    name: String,
    arguments: Value,
}

#[pyfunction]
fn test_agent_with_parallel_tool_calls(
    session_id: String,
    calls_json: String,
    final_text: String,
) -> PyResult<PyAgent> {
    let session_id = SessionId::new(&session_id)
        .map_err(|error| error::config_message_to_py(error.to_string()))?;
    let model_name = ModelName::new("fake-model")
        .map_err(|error| error::config_message_to_py(error.to_string()))?;
    let specs = serde_json::from_str::<Vec<ScriptedCallSpec>>(&calls_json)
        .map_err(|error| error::protocol_message_to_py(error.to_string()))?;

    let mut outputs = Vec::with_capacity(specs.len());
    let mut registered_names = std::collections::BTreeSet::new();
    let mut builder = AgentBuilder::new(session_id);

    for item in specs {
        let tool_name = ToolName::new(&item.name)
            .map_err(|error| error::config_message_to_py(error.to_string()))?;
        let call_id = ModelToolCallId::new(&item.id)
            .map_err(|error| error::protocol_message_to_py(error.to_string()))?;
        let arguments = ToolArguments::try_from(item.arguments)
            .map_err(|error| error::protocol_message_to_py(error.to_string()))?;
        if registered_names.insert(tool_name.clone()) {
            let spec = permissive_bridge_tool(tool_name.clone())?;
            builder = builder
                .bridge_tool(
                    spec.name().clone(),
                    spec.description(),
                    spec.input_schema().clone(),
                )
                .map_err(error::agent_build_error_to_py)?;
        }
        outputs.push(ModelOutput::tool_call(ModelToolCall::new(
            call_id, tool_name, arguments,
        )));
    }

    let first_turn = vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(outputs, FinishReason::ToolCalls, None),
    })];
    let second_turn = vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::text(&final_text)],
            FinishReason::Stop,
            None,
        ),
    })];
    let provider = Arc::new(FakeModelProvider::new_turns(vec![first_turn, second_turn]));
    let agent = builder
        .model_provider(provider, model_name)
        .build()
        .map_err(error::agent_build_error_to_py)?;
    Ok(PyAgent::new(agent, None))
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ScriptedTurnSpec {
    Text { text: String },
    ToolCalls { calls: Vec<ScriptedCallSpec> },
}

#[pyfunction]
fn test_agent_with_scripted_turns(
    session_id: String,
    turns_json: String,
    tool_names: Vec<String>,
    session_store_path: Option<String>,
) -> PyResult<PyAgent> {
    let session_id = SessionId::new(&session_id)
        .map_err(|error| error::config_message_to_py(error.to_string()))?;
    let model_name = ModelName::new("fake-model")
        .map_err(|error| error::config_message_to_py(error.to_string()))?;
    let turn_specs = serde_json::from_str::<Vec<ScriptedTurnSpec>>(&turns_json)
        .map_err(|error| error::protocol_message_to_py(error.to_string()))?;

    let mut turns = Vec::with_capacity(turn_specs.len());
    for spec in turn_specs {
        let turn = match spec {
            ScriptedTurnSpec::Text { text } => vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text(&text)],
                    FinishReason::Stop,
                    None,
                ),
            })],
            ScriptedTurnSpec::ToolCalls { calls } => {
                let mut outputs = Vec::with_capacity(calls.len());
                for item in calls {
                    let tool_name = ToolName::new(&item.name)
                        .map_err(|error| error::config_message_to_py(error.to_string()))?;
                    let call_id = ModelToolCallId::new(&item.id)
                        .map_err(|error| error::protocol_message_to_py(error.to_string()))?;
                    let arguments = ToolArguments::try_from(item.arguments)
                        .map_err(|error| error::protocol_message_to_py(error.to_string()))?;
                    outputs.push(ModelOutput::tool_call(ModelToolCall::new(
                        call_id, tool_name, arguments,
                    )));
                }
                vec![Ok(ModelEvent::Completed {
                    response: ModelResponse::new(outputs, FinishReason::ToolCalls, None),
                })]
            }
        };
        turns.push(turn);
    }

    let provider = Arc::new(FakeModelProvider::new_turns(turns));
    let mut builder = AgentBuilder::new(session_id).model_provider(provider, model_name);
    for raw_name in tool_names {
        let tool_name = ToolName::new(&raw_name)
            .map_err(|error| error::config_message_to_py(error.to_string()))?;
        let spec = permissive_bridge_tool(tool_name)?;
        builder = builder
            .bridge_tool(
                spec.name().clone(),
                spec.description(),
                spec.input_schema().clone(),
            )
            .map_err(error::agent_build_error_to_py)?;
    }
    if let Some(path) = session_store_path.as_ref() {
        builder = builder.session_store(FileSessionStore::new(path));
    }
    let agent = builder.build().map_err(error::agent_build_error_to_py)?;
    Ok(PyAgent::new(agent, session_store_path))
}

// Keep test-only PyO3 factory scalar-only to mirror PyAgentBuilder's ABI.
#[allow(clippy::too_many_arguments)]
#[pyfunction]
#[pyo3(signature = (
    session_id,
    turns_json,
    tool_names,
    provider_context_window_tokens=None,
    explicit_context_window_tokens=None,
    hook_subscriptions_json=None,
    max_model_turns=None,
))]
fn test_agent_with_context_and_hooks(
    session_id: String,
    turns_json: String,
    tool_names: Vec<String>,
    provider_context_window_tokens: Option<u64>,
    explicit_context_window_tokens: Option<u64>,
    hook_subscriptions_json: Option<String>,
    max_model_turns: Option<usize>,
) -> PyResult<PyAgent> {
    let session_id = SessionId::new(&session_id)
        .map_err(|error| error::config_message_to_py(error.to_string()))?;
    let model_name = ModelName::new("fake-model")
        .map_err(|error| error::config_message_to_py(error.to_string()))?;
    let turn_specs = serde_json::from_str::<Vec<ScriptedTurnSpec>>(&turns_json)
        .map_err(|error| error::protocol_message_to_py(error.to_string()))?;

    let mut turns = Vec::with_capacity(turn_specs.len());
    for spec in turn_specs {
        let usage = Some(merry_core::ModelUsage::new(100, 25));
        let turn = match spec {
            ScriptedTurnSpec::Text { text } => vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text(&text)],
                    FinishReason::Stop,
                    usage,
                ),
            })],
            ScriptedTurnSpec::ToolCalls { calls } => {
                let mut outputs = Vec::with_capacity(calls.len());
                for item in calls {
                    let tool_name = ToolName::new(&item.name)
                        .map_err(|error| error::config_message_to_py(error.to_string()))?;
                    let call_id = ModelToolCallId::new(&item.id)
                        .map_err(|error| error::protocol_message_to_py(error.to_string()))?;
                    let arguments = ToolArguments::try_from(item.arguments)
                        .map_err(|error| error::protocol_message_to_py(error.to_string()))?;
                    outputs.push(ModelOutput::tool_call(ModelToolCall::new(
                        call_id, tool_name, arguments,
                    )));
                }
                vec![Ok(ModelEvent::Completed {
                    response: ModelResponse::new(outputs, FinishReason::ToolCalls, usage),
                })]
            }
        };
        turns.push(turn);
    }

    let mut capabilities = ModelCapabilities::new(true, true, true, true, None, None)
        .map_err(|error| error::config_message_to_py(error.to_string()))?;
    if let Some(tokens) = provider_context_window_tokens {
        let tokens = std::num::NonZeroU64::new(tokens).ok_or_else(|| {
            error::config_message_to_py("provider_context_window_tokens must be greater than zero")
        })?;
        capabilities = capabilities.with_max_input_tokens(tokens);
    }
    let provider = Arc::new(FakeModelProvider::with_turns_and_capabilities(
        turns,
        capabilities,
    ));
    let mut builder = AgentBuilder::new(session_id).model_provider(provider, model_name);
    if let Some(tokens) = explicit_context_window_tokens {
        let tokens = std::num::NonZeroU64::new(tokens).ok_or_else(|| {
            error::config_message_to_py("explicit_context_window_tokens must be greater than zero")
        })?;
        builder = builder.context_window_tokens(tokens);
    }
    if let Some(turns_limit) = max_model_turns {
        let loop_cfg = merry::AgentLoopConfig::new(turns_limit)
            .map_err(|error| error::config_message_to_py(error.to_string()))?;
        builder = builder.loop_config(loop_cfg);
    }
    if let Some(subs_json) = hook_subscriptions_json {
        let subs = crate::protocol::parse_hook_subscriptions(&subs_json)
            .map_err(error::config_message_to_py)?;
        if !subs.is_empty() {
            builder = builder.bridge_hook_subscriptions(subs);
        }
    }
    for raw_name in tool_names {
        let tool_name = ToolName::new(&raw_name)
            .map_err(|error| error::config_message_to_py(error.to_string()))?;
        let spec = permissive_bridge_tool(tool_name)?;
        builder = builder
            .bridge_tool(
                spec.name().clone(),
                spec.description(),
                spec.input_schema().clone(),
            )
            .map_err(error::agent_build_error_to_py)?;
    }
    let agent = builder.build().map_err(error::agent_build_error_to_py)?;
    Ok(PyAgent::new(agent, None))
}

// Keep the test-only PyO3 factory scalar-only to mirror PyAgentBuilder's ABI.
#[allow(clippy::too_many_arguments)]
#[pyfunction]
fn test_agent_echoing_request(
    session_id: String,
    base_instructions: Option<String>,
    progress_commentary: Option<bool>,
    progress_commentary_instructions: Option<String>,
    stable_blocks: Vec<(String, String)>,
    max_output_tokens: Option<u64>,
    parallel_tool_calls: Option<String>,
    reasoning_effort: Option<String>,
    service_tier: Option<String>,
) -> PyResult<PyAgent> {
    let session_id = SessionId::new(&session_id)
        .map_err(|error| error::config_message_to_py(error.to_string()))?;
    let model_name = ModelName::new("echo-model")
        .map_err(|error| error::config_message_to_py(error.to_string()))?;

    let mut profile = PromptProfile::default();
    if let Some(base) = base_instructions {
        profile = profile
            .with_base_instructions(base)
            .map_err(|error| error::config_message_to_py(error.to_string()))?;
    }
    if let Some(commentary) = progress_commentary_instructions {
        profile = profile
            .with_progress_commentary_instructions(commentary)
            .map_err(|error| error::config_message_to_py(error.to_string()))?;
    }
    for (tag, text) in stable_blocks {
        let block = PromptBlock::new(tag, text)
            .map_err(|error| error::config_message_to_py(error.to_string()))?;
        profile = profile
            .with_stable_block(block)
            .map_err(|error| error::config_message_to_py(error.to_string()))?;
    }

    let parallel = match parallel_tool_calls.as_deref() {
        None | Some("auto") => ParallelToolCalls::Auto,
        Some("enabled") => ParallelToolCalls::Enabled,
        Some("disabled") => ParallelToolCalls::Disabled,
        Some(other) => {
            return Err(error::config_message_to_py(format!(
                "unsupported parallel_tool_calls mode: {other}"
            )));
        }
    };
    let reasoning = reasoning_effort
        .as_deref()
        .map(ReasoningEffort::new)
        .transpose()
        .map_err(|error| error::config_message_to_py(error.to_string()))?;
    let tier = service_tier
        .as_deref()
        .map(ServiceTier::new)
        .transpose()
        .map_err(|error| error::config_message_to_py(error.to_string()))?;
    let generation = GenerationConfig::new(max_output_tokens, false)
        .map_err(|error| error::config_message_to_py(error.to_string()))?
        .with_parallel_tool_calls(parallel)
        .with_reasoning_effort(reasoning)
        .with_service_tier(tier);

    let mut builder = AgentBuilder::new(session_id)
        .model_provider(Arc::new(EchoRequestProvider::new()), model_name)
        .prompt_profile(profile)
        .generation_config(generation);
    if let Some(enabled) = progress_commentary {
        builder = builder.progress_commentary(enabled);
    }
    let agent = builder.build().map_err(error::agent_build_error_to_py)?;
    Ok(PyAgent::new(agent, None))
}

#[pyfunction]
fn test_agent_with_final_output(
    session_id: String,
    arguments_json: String,
    final_text: String,
) -> PyResult<PyAgent> {
    let session_id = SessionId::new(&session_id)
        .map_err(|error| error::config_message_to_py(error.to_string()))?;
    let model_name = ModelName::new("fake-model")
        .map_err(|error| error::config_message_to_py(error.to_string()))?;
    let tool_name = ToolName::new(merry::FINAL_OUTPUT_TOOL_NAME)
        .map_err(|error| error::config_message_to_py(error.to_string()))?;
    let arguments = parse_tool_arguments(&arguments_json)?;
    let call_id = ModelToolCallId::new("test-final-output-call")
        .map_err(|error| error::protocol_message_to_py(error.to_string()))?;
    let call = ModelToolCall::new(call_id, tool_name, arguments);
    let provider = Arc::new(FakeModelProvider::new(vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
            None,
        ),
    })]));
    let agent = AgentBuilder::new(session_id)
        .model_provider(provider, model_name)
        .build()
        .map_err(error::agent_build_error_to_py)?;
    let _ = final_text;
    Ok(PyAgent::new(agent, None))
}

#[pyfunction]
fn test_agent_with_pending_response(session_id: String) -> PyResult<PyAgent> {
    let session_id = SessionId::new(&session_id)
        .map_err(|error| error::config_message_to_py(error.to_string()))?;
    let model_name = ModelName::new("pending-model")
        .map_err(|error| error::config_message_to_py(error.to_string()))?;
    let agent = AgentBuilder::new(session_id)
        .model_provider(Arc::new(PendingProvider::new()), model_name)
        .build()
        .map_err(error::agent_build_error_to_py)?;
    Ok(PyAgent::new(agent, None))
}

fn build_agent(session_id: String, provider: Arc<FakeModelProvider>) -> PyResult<PyAgent> {
    let session_id = SessionId::new(&session_id)
        .map_err(|error| error::config_message_to_py(error.to_string()))?;
    let model_name = ModelName::new("fake-model")
        .map_err(|error| error::config_message_to_py(error.to_string()))?;
    let agent = AgentBuilder::new(session_id)
        .model_provider(provider, model_name)
        .build()
        .map_err(error::agent_build_error_to_py)?;
    Ok(PyAgent::new(agent, None))
}

fn parse_tool_arguments(arguments_json: &str) -> PyResult<ToolArguments> {
    let arguments = serde_json::from_str::<Value>(arguments_json)
        .map_err(|error| error::protocol_message_to_py(error.to_string()))?;
    ToolArguments::try_from(arguments)
        .map_err(|error| error::protocol_message_to_py(error.to_string()))
}

fn permissive_bridge_tool(tool_name: ToolName) -> PyResult<ToolSpec> {
    let schema = Schema::try_from(serde_json::json!({
        "type": "object",
        "additionalProperties": true
    }))
    .map_err(|error| error::protocol_message_to_py(error.to_string()))?;
    let schema = ToolInputSchema::new(schema)
        .map_err(|error| error::protocol_message_to_py(error.to_string()))?;
    ToolSpec::new(tool_name, "Test bridge tool.", schema)
        .map_err(|error| error::protocol_message_to_py(error.to_string()))
}

struct PendingProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
}

impl PendingProvider {
    fn new() -> Self {
        Self {
            name: ProviderName::new("pending-test-provider")
                .expect("static provider name should be valid"),
            capabilities: ModelCapabilities::new(true, true, false, false, None, None)
                .expect("static provider capabilities should be valid"),
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

struct EchoRequestProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
}

impl EchoRequestProvider {
    fn new() -> Self {
        Self {
            name: ProviderName::new("echo-test-provider")
                .expect("static provider name should be valid"),
            capabilities: ModelCapabilities::new(true, true, false, false, None, None)
                .expect("static provider capabilities should be valid"),
        }
    }
}

impl ModelProvider for EchoRequestProvider {
    fn name(&self) -> &ProviderName {
        &self.name
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn stream_model<'a>(
        &'a self,
        request: ModelRequest,
        _context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            let stable_prefix: Vec<&str> = request
                .stable_prefix_messages()
                .iter()
                .map(|message| message.content().as_text())
                .collect();
            let payload = serde_json::json!({
                "stable_prefix": stable_prefix,
                "generation": request.generation(),
            });
            let text = payload.to_string();
            let stream: ModelEventStream = Box::pin(futures_util::stream::iter(vec![Ok(
                ModelEvent::Completed {
                    response: ModelResponse::new(
                        vec![ModelOutput::text(&text)],
                        FinishReason::Stop,
                        None,
                    ),
                },
            )]));
            Ok(stream)
        })
    }
}
