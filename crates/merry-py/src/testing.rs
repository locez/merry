//! Deterministic native factories used by Python parity tests.

use crate::{agent::PyAgent, error};
use merry::{AgentBuilder, ModelName};
use merry_core::{ProviderName, SessionId, ToolInputSchema, ToolName, ToolSpec};
use merry_llm::testing::FakeModelProvider;
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelOutput,
    ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse, ModelStreamContext,
    ModelToolCall, ModelToolCallId, ToolArguments,
};
use pyo3::{prelude::*, types::PyModule};
use schemars::Schema;
use serde_json::Value;
use std::sync::Arc;

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(test_agent_with_fake_response, module)?)?;
    module.add_function(wrap_pyfunction!(
        test_agent_with_scripted_tool_call,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(test_agent_with_final_output, module)?)?;
    module.add_function(wrap_pyfunction!(test_agent_with_pending_response, module)?)?;
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
    let arguments = serde_json::from_str::<Value>(&arguments_json)
        .map_err(|error| error::protocol_message_to_py(error.to_string()))?;
    let arguments = ToolArguments::try_from(arguments)
        .map_err(|error| error::protocol_message_to_py(error.to_string()))?;
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
    let schema = Schema::try_from(serde_json::json!({
        "type": "object",
        "additionalProperties": true
    }))
    .map_err(|error| error::protocol_message_to_py(error.to_string()))?;
    let schema = ToolInputSchema::new(schema)
        .map_err(|error| error::protocol_message_to_py(error.to_string()))?;
    let spec = ToolSpec::new(tool_name, "Test bridge tool.", schema)
        .map_err(|error| error::protocol_message_to_py(error.to_string()))?;
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
    Ok(PyAgent::new(agent))
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
    let arguments = serde_json::from_str::<Value>(&arguments_json)
        .map_err(|error| error::protocol_message_to_py(error.to_string()))?;
    let arguments = ToolArguments::try_from(arguments)
        .map_err(|error| error::protocol_message_to_py(error.to_string()))?;
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
    Ok(PyAgent::new(agent))
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
    Ok(PyAgent::new(agent))
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
    Ok(PyAgent::new(agent))
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
