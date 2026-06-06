use crate::cli_error::{CliError, stdout_error, unexpected};
use futures_util::StreamExt;
use merry_core::{PendingToolCall, RuntimeEvent, RuntimeEventKind};
use merry_runtime::{Runtime, StepContext, StepInput};
use tokio::io::{AsyncWrite, AsyncWriteExt, BufWriter};

pub(crate) async fn write_runtime_step_events<W>(
    runtime: &Runtime,
    input: StepInput,
    context: StepContext,
    writer: W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let mut writer = BufWriter::new(writer);
    write_runtime_step_events_to(runtime, input, context, &mut writer).await?;
    writer.flush().await.map_err(stdout_error)
}

pub(crate) async fn write_runtime_step_events_to<W>(
    runtime: &Runtime,
    input: StepInput,
    context: StepContext,
    writer: &mut W,
) -> Result<Vec<RuntimeEvent>, CliError>
where
    W: AsyncWrite + Unpin,
{
    let events = collect_runtime_step_events(runtime, input, context).await?;
    write_runtime_events(events.clone(), writer).await?;
    Ok(events)
}

pub(crate) async fn collect_runtime_step_events(
    runtime: &Runtime,
    input: StepInput,
    context: StepContext,
) -> Result<Vec<RuntimeEvent>, CliError> {
    let mut events = runtime.step(input, context).map_err(unexpected)?;
    let mut collected = Vec::new();
    while let Some(event) = events.next().await {
        collected.push(event);
    }

    Ok(collected)
}

pub(crate) async fn write_runtime_events<W>(
    events: Vec<RuntimeEvent>,
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    write_runtime_event_slice(&events, writer).await
}

pub(crate) async fn write_runtime_event_slice<W>(
    events: &[RuntimeEvent],
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    for event in events {
        write_runtime_event(event, writer).await?;
    }
    Ok(())
}

pub(crate) async fn write_runtime_event<W>(
    event: &RuntimeEvent,
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let line = serde_json::to_string(event).map_err(unexpected)?;
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(stdout_error)?;
    writer.write_all(b"\n").await.map_err(stdout_error)
}

pub(crate) fn first_pending_tool_call(events: &[RuntimeEvent]) -> Option<PendingToolCall> {
    events.iter().find_map(|event| match &event.kind {
        RuntimeEventKind::ToolCallPending { call } => Some(call.clone()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::write_runtime_step_events;
    use crate::test_support::{CompletingProvider, RecordingProvider};
    use merry_llm::{GenerationConfig, ModelName};
    use merry_runtime::{Runtime, StepContext, StepInput};
    use serde_json::Value;
    use std::sync::Arc;

    #[tokio::test]
    async fn writes_runtime_lifecycle_jsonl_without_model_events() {
        let runtime = Runtime::builder(merry_core::SessionId::new("runtime-events-test").unwrap())
            .model_provider(
                Arc::new(CompletingProvider::new()),
                ModelName::new("debug-model").unwrap(),
            )
            .build()
            .expect("runtime should build");
        let input = StepInput::user_text("hello").expect("valid input");
        let mut output = Vec::new();

        write_runtime_step_events(&runtime, input, StepContext::default(), &mut output)
            .await
            .unwrap_or_else(|_| panic!("runtime events should write"));

        let text = String::from_utf8(output).expect("output should be utf-8");
        let lines = text.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 4);
        let event_types = lines
            .iter()
            .map(|line| {
                let value = serde_json::from_str::<Value>(line).expect("line should be JSON");
                value["kind"]["type"].as_str().unwrap().to_owned()
            })
            .collect::<Vec<_>>();

        assert_eq!(
            event_types,
            [
                "session_started",
                "step_started",
                "artifact_recorded",
                "step_completed"
            ]
        );
        assert!(!text.contains("hidden"));
    }

    #[tokio::test]
    async fn passes_generation_config_to_runtime_step() {
        let provider = RecordingProvider::new();
        let requests = Arc::clone(&provider.requests);
        let runtime =
            Runtime::builder(merry_core::SessionId::new("runtime-events-config").unwrap())
                .model_provider(Arc::new(provider), ModelName::new("debug-model").unwrap())
                .build()
                .expect("runtime should build");
        let input = StepInput::user_text("hello").expect("valid input");
        let context = StepContext::default().with_generation_config(
            GenerationConfig::new(Some(16), false).expect("valid generation config"),
        );
        let mut output = Vec::new();

        write_runtime_step_events(&runtime, input, context, &mut output)
            .await
            .unwrap_or_else(|_| panic!("runtime events should write"));

        let requests = requests
            .lock()
            .expect("request mutex should not be poisoned")
            .clone();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].generation().max_output_tokens(), Some(16));
        assert!(!requests[0].generation().allow_parallel_tool_calls());
    }
}
