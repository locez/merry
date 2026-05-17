use futures_executor::block_on;
use futures_util::TryStreamExt;
use merry_core::{ToolInputSchema, ToolName, ToolSpec};
use merry_llm::{
    FinishReason, GenerationConfig, ModelContent, ModelEvent, ModelMessage, ModelMessageRole,
    ModelName, ModelOutput, ModelProvider, ModelRequest, ModelResponse, ModelStreamContext, Usage,
    testing::FakeModelProvider,
};
use schemars::Schema;
use serde_json::json;

fn tool_spec() -> ToolSpec {
    let schema = Schema::try_from(json!({
        "type": "object",
        "properties": {
            "query": { "type": "string" }
        },
        "required": ["query"]
    }))
    .expect("test schema should be a JSON schema");

    ToolSpec::new(
        ToolName::new("search_notes").expect("valid tool name"),
        "Search local notes",
        ToolInputSchema::new(schema).expect("valid object schema"),
    )
    .expect("valid tool spec")
}

fn request_with_tool(tool: ToolSpec) -> ModelRequest {
    ModelRequest::new(
        ModelName::new("fake/model").expect("valid model name"),
        vec![
            ModelMessage::new(
                ModelMessageRole::User,
                ModelContent::text("Find the note").expect("valid content"),
            )
            .expect("valid message"),
        ],
        vec![tool],
        GenerationConfig::default(),
    )
    .expect("valid model request")
}

fn collect_events(
    provider: &dyn ModelProvider,
    request: ModelRequest,
) -> Result<Vec<ModelEvent>, merry_llm::ModelError> {
    let stream = block_on(provider.stream_model(request, ModelStreamContext::default()))?;
    block_on(stream.try_collect())
}

#[test]
fn runtime_tests_can_stream_fake_provider_events_without_openai_types() {
    let provider = FakeModelProvider::new(vec![
        Ok(ModelEvent::Started),
        Ok(ModelEvent::OutputTextDelta {
            delta: "result".to_owned(),
        }),
        Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("result")],
                FinishReason::Stop,
                Some(Usage::new(4, 1)),
            ),
        }),
    ]);
    let tool = tool_spec();
    let request = request_with_tool(tool.clone());

    let events = collect_events(&provider, request).expect("fake stream should succeed");

    assert_eq!(
        events,
        vec![
            ModelEvent::Started,
            ModelEvent::OutputTextDelta {
                delta: "result".to_owned()
            },
            ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("result")],
                    FinishReason::Stop,
                    Some(Usage::new(4, 1)),
                )
            }
        ]
    );

    let recorded_requests = provider.recorded_requests();
    assert_eq!(recorded_requests.len(), 1);
    assert_eq!(recorded_requests[0].tools(), &[tool]);
}
