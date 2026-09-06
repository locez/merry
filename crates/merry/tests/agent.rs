use merry::{
    AgentBuilder, AgentLoopConfig, AgentProfile, AgentProfileContext, ModelName, ModelProvider,
    SessionId,
};
use merry_core::{ProviderName, ToolInputSchema, ToolName, ToolSpec};
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelOutput,
    ModelProviderFuture, ModelRequest, ModelResponse, ModelStreamContext, ModelToolCall,
    ModelToolCallId, ToolArguments, testing::FakeModelProvider,
};
use merry_runtime::RegisteredTool;
use schemars::{JsonSchema, Schema};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

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

#[derive(Debug, Deserialize, JsonSchema)]
struct LookupOrderInput {
    order_id: String,
}

#[derive(Debug, Serialize)]
struct LookupOrderOutput {
    status: String,
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct StructuredAnswer {
    #[schemars(description = "Short answer for the caller.")]
    answer: String,
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

#[path = "agent/bridge_handoff.rs"]
mod bridge_handoff;

#[path = "agent/cancellation.rs"]
mod cancellation;

#[path = "agent/construction.rs"]
mod construction;

#[path = "agent/events.rs"]
mod events;

#[path = "agent/interactive.rs"]
mod interactive;

#[path = "agent/native_tools.rs"]
mod native_tools;

#[path = "agent/structured_output.rs"]
mod structured_output;
