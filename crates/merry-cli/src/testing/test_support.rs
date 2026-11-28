use crate::cli_error::{CliError, unexpected};
use futures_util::stream;
use merry::events::{ProviderName, ToolName};
use merry::process::{
    ProcessActionIntent, ProcessExitStatus, ProcessRunner, ProcessRunnerContext,
    ProcessRunnerError, ProcessRunnerFuture, ProcessRunnerOutput,
};
use merry::providers::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelName,
    ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse,
    ModelStreamContext, ModelToolCall, ModelToolCallId, ToolArguments,
};
use std::sync::{Arc, Mutex};

pub(crate) fn process_tool_call(
    call_id: &str,
    argv: &[&str],
    cwd: Option<&str>,
) -> Result<ModelEvent, CliError> {
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        "command".to_owned(),
        serde_json::Value::String(argv.join(" ")),
    );
    arguments.insert(
        "cwd".to_owned(),
        cwd.map_or(serde_json::Value::Null, |cwd| {
            serde_json::Value::String(cwd.to_owned())
        }),
    );
    tool_call(
        call_id,
        merry::profiles::CODING_LOOP_PROCESS_TOOL,
        arguments,
    )
}

pub(crate) fn workspace_tool_call<const N: usize>(
    call_id: &str,
    tool_name: &str,
    arguments: [(&str, serde_json::Value); N],
) -> Result<ModelEvent, CliError> {
    tool_call(
        call_id,
        tool_name,
        arguments
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

pub(crate) fn tool_call(
    call_id: &str,
    tool_name: &str,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> Result<ModelEvent, CliError> {
    let call = ModelToolCall::new(
        ModelToolCallId::new(call_id).map_err(unexpected)?,
        ToolName::new(tool_name).map_err(unexpected)?,
        ToolArguments::new(arguments),
    );
    Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
            None,
        ),
    })
}

#[derive(Clone)]
pub(crate) struct FakeProcessRunner {
    outputs: Arc<Mutex<Vec<FakeProcessRunnerStep>>>,
}

impl FakeProcessRunner {
    pub(crate) fn succeeding(stdout: impl Into<String>) -> Self {
        Self::scripted([FakeProcessRunnerStep::success(stdout)])
    }

    fn scripted<const N: usize>(steps: [FakeProcessRunnerStep; N]) -> Self {
        Self {
            outputs: Arc::new(Mutex::new(steps.into_iter().rev().collect())),
        }
    }
}

#[derive(Clone)]
struct FakeProcessRunnerStep {
    status: ProcessExitStatus,
    stdout: String,
    stderr: String,
}

impl FakeProcessRunnerStep {
    pub(crate) fn success(stdout: impl Into<String>) -> Self {
        Self {
            status: ProcessExitStatus::Exited(0),
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }
}

impl ProcessRunner for FakeProcessRunner {
    fn run<'a>(
        &'a self,
        intent: ProcessActionIntent,
        context: ProcessRunnerContext,
    ) -> ProcessRunnerFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ProcessRunnerError::Cancelled);
            }
            let output = self
                .outputs
                .lock()
                .expect("fake process outputs mutex should not be poisoned")
                .pop()
                .unwrap_or_else(|| FakeProcessRunnerStep::success(String::new()));

            ProcessRunnerOutput::new(
                &intent,
                output.status,
                output.stdout,
                false,
                output.stderr,
                false,
            )
            .map_err(|source| ProcessRunnerError::infrastructure(source.to_string()))
        })
    }
}

pub(crate) type ScriptedStep = Vec<Result<ModelEvent, ModelError>>;

#[derive(Debug, Clone)]
pub(crate) struct ScriptedProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    steps: Arc<Mutex<Vec<ScriptedStep>>>,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}

impl ScriptedProvider {
    pub(crate) fn new(scripts: Vec<ScriptedStep>) -> Self {
        Self {
            name: ProviderName::new("debug-scripted-provider").expect("valid provider name"),
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .expect("valid capabilities"),
            steps: Arc::new(Mutex::new(scripts.into_iter().rev().collect())),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(crate) fn recorded_requests(&self) -> Vec<ModelRequest> {
        self.requests
            .lock()
            .expect("request mutex should not be poisoned")
            .clone()
    }
}

impl ModelProvider for ScriptedProvider {
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
            self.requests
                .lock()
                .expect("request mutex should not be poisoned")
                .push(request);

            let script = self
                .steps
                .lock()
                .expect("step mutex should not be poisoned")
                .pop()
                .unwrap_or_default();

            Ok(Box::pin(stream::iter(script)) as ModelEventStream)
        })
    }
}

pub(crate) fn model_name() -> ModelName {
    ModelName::new("debug-model").expect("valid model name")
}

pub(crate) async fn collect_runtime_step_events(
    runtime: &merry::runtime::Runtime,
    input: merry::runtime::StepInput,
    context: merry::runtime::StepContext,
) -> Result<Vec<merry::runtime::RuntimeJournalEvent>, CliError> {
    use futures_util::StreamExt;

    let mut events = runtime.step(input, context).map_err(unexpected)?;
    let mut collected = Vec::new();
    while let Some(event) = events.next().await {
        collected.push(event);
    }

    Ok(collected)
}

pub(crate) fn first_pending_tool_call(
    events: &[merry::runtime::RuntimeJournalEvent],
) -> Option<merry::events::PendingToolCall> {
    events.iter().find_map(|event| match &event.payload {
        merry::runtime::RuntimeJournalPayload::ToolCallPending { call } => Some(call.clone()),
        _ => None,
    })
}
