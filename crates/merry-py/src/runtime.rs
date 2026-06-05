use crate::{error, serde_py::json_to_py};
use futures_core::Stream;
use futures_util::StreamExt;
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, ErrorInfo, ProviderName, SessionId, ToolCallId,
    ToolInputSchema, ToolName, ToolSpec,
};
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelName,
    ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse,
    ModelStreamContext, ModelToolCall, ModelToolCallId, ProviderErrorKind, ToolArguments,
    testing::FakeModelProvider,
};
use merry_provider_openai::{OpenAiProvider, OpenAiProviderConfig};
use merry_runtime::{
    AgentLoopConfig, AgentLoopResult, AgentLoopStatus, ArtifactContent, FinalOutputContract,
    RegisteredTool, Runtime, RuntimeBuilder, StepContext, StepInput, ToolExecutionContext,
    ToolExecutionError, ToolExecutionOutcome, ToolExecutor, ToolExecutorFuture, ToolRunner,
};
use pyo3::{prelude::*, types::PyDict};
use schemars::Schema;
use serde_json::{Map, Value, json};
use std::{
    pin::Pin,
    sync::{Arc, Mutex, mpsc},
    task::{Context, Poll},
    thread,
};
use tokio_util::sync::CancellationToken;

const INVALID_SESSION_ID_HINT: &str =
    "Use a non-empty stable session id without surrounding whitespace.";
const INVALID_SESSION_ID_MESSAGE: &str = "Invalid Merry runtime session id.";
const FINAL_OUTPUT_SCHEMA_HINT: &str =
    "Pass final_output_model as a Pydantic BaseModel with Field(description=...) on every field.";

#[pyclass(name = "Runtime")]
pub(crate) struct PyRuntime {
    session_id: SessionId,
    scenario: RuntimeScenario,
    tools: Vec<RegisteredTool>,
    runtime: Runtime,
}

#[pyclass(name = "RuntimeEventStream")]
pub(crate) struct NativeRuntimeEventStream {
    state: Arc<Mutex<NativeRuntimeEventStreamState>>,
    command_sender: tokio::sync::mpsc::UnboundedSender<StreamRunnerCommand>,
}

struct NativeRuntimeEventStreamState {
    receiver: mpsc::Receiver<StreamRunnerMessage>,
    events: Vec<merry_core::RuntimeEvent>,
    result: Option<AgentLoopResult>,
    error: Option<StreamRunnerError>,
    finished: bool,
}

enum StreamRunnerMessage {
    Event(merry_core::RuntimeEvent),
    Finished {
        result: Option<AgentLoopResult>,
    },
    Error {
        code: &'static str,
        message: String,
        hint: Option<&'static str>,
    },
}

enum StreamRunnerCommand {
    SubmitToolSuccessJson {
        call_id: String,
        artifact_id: String,
        content_json: String,
        ack_sender: mpsc::Sender<Result<(), StreamRunnerError>>,
    },
}

#[derive(Clone)]
struct StreamRunnerError {
    code: &'static str,
    message: String,
    hint: Option<&'static str>,
}

enum StreamReceiveError {
    Closed,
    Poisoned,
}

enum StreamResultError {
    Receive(StreamReceiveError),
    Runtime {
        code: &'static str,
        message: String,
        hint: Option<&'static str>,
    },
    MissingResult,
}

impl From<StreamReceiveError> for StreamResultError {
    fn from(error: StreamReceiveError) -> Self {
        Self::Receive(error)
    }
}

#[derive(Clone)]
enum RuntimeScenario {
    Empty,
    OpenAiCompatible {
        config: OpenAiProviderConfig,
        model: ModelName,
    },
    FakeResponse {
        final_text: String,
    },
    ScriptedToolCall {
        tool_name: ToolName,
        arguments: Map<String, Value>,
        final_text: String,
    },
    ScriptedToolCalls {
        calls: Vec<ScriptedToolCallSpec>,
        final_text: String,
    },
}

#[derive(Clone)]
struct ScriptedToolCallSpec {
    tool_name: ToolName,
    arguments: Map<String, Value>,
}

#[derive(Clone)]
enum StaticToolBehavior {
    DomainFailure {
        diagnostic_code: String,
        message: String,
        content: String,
    },
    ExecutorException {
        message: String,
    },
}

#[derive(Clone)]
struct StaticToolExecutor {
    behavior: StaticToolBehavior,
}

impl ToolExecutor for StaticToolExecutor {
    fn execute<'a>(
        &'a self,
        _call: merry_core::PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            match &self.behavior {
                StaticToolBehavior::DomainFailure {
                    diagnostic_code,
                    message,
                    content,
                } => {
                    let diagnostic = ErrorInfo::new(diagnostic_code, message)
                        .map_err(|source| ToolExecutionError::infrastructure(source.to_string()))?;
                    Ok(ToolExecutionOutcome::failed_json(
                        content.clone(),
                        diagnostic,
                    ))
                }
                StaticToolBehavior::ExecutorException { message } => {
                    Err(ToolExecutionError::infrastructure(message.clone()))
                }
            }
        })
    }
}

#[pymethods]
impl PyRuntime {
    #[new]
    fn new(session_id: String) -> PyResult<Self> {
        let session_id = SessionId::new(&session_id).map_err(|_error| {
            error::runtime_message_to_py(
                "runtime.invalid_session_id",
                INVALID_SESSION_ID_MESSAGE,
                Some(INVALID_SESSION_ID_HINT),
            )
        })?;
        let scenario = RuntimeScenario::Empty;
        let tools = Vec::new();
        let runtime = build_runtime_from(session_id.clone(), &scenario, &tools)
            .map_err(error::runtime_error_to_py)?;

        Ok(Self {
            session_id,
            scenario,
            tools,
            runtime,
        })
    }

    #[staticmethod]
    fn with_openai_compatible(
        api_key: String,
        model: String,
        base_url: Option<String>,
    ) -> PyResult<Self> {
        let mut config = OpenAiProviderConfig::new(&api_key).map_err(|source| {
            error::config_message_to_py(
                "config.openai_invalid",
                &source.to_string(),
                Some("Pass a non-empty OpenAI-compatible API key."),
            )
        })?;
        if let Some(base_url) = base_url {
            config = config.with_base_url(&base_url).map_err(|source| {
                error::config_message_to_py(
                    "config.openai_invalid",
                    &source.to_string(),
                    Some("Use an http or https OpenAI-compatible base URL."),
                )
            })?;
        }
        let model = ModelName::new(&model).map_err(|source| {
            error::config_message_to_py(
                "config.model_invalid",
                &source.to_string(),
                Some("Pass a non-empty model name without surrounding whitespace."),
            )
        })?;
        let session_id =
            SessionId::new("python-sdk-openai").expect("static session id must be valid");
        let scenario = RuntimeScenario::OpenAiCompatible { config, model };
        let tools = Vec::new();
        let runtime = build_runtime_from(session_id.clone(), &scenario, &tools)
            .map_err(error::runtime_error_to_py)?;

        Ok(Self {
            session_id,
            scenario,
            tools,
            runtime,
        })
    }

    #[staticmethod]
    #[pyo3(name = "_with_fake_response")]
    fn with_fake_response(final_text: String) -> PyResult<Self> {
        let session_id =
            SessionId::new("python-sdk-fake").expect("static session id must be valid");
        let scenario = RuntimeScenario::FakeResponse { final_text };
        let tools = Vec::new();
        let runtime = build_runtime_from(session_id.clone(), &scenario, &tools)
            .map_err(error::runtime_error_to_py)?;

        Ok(Self {
            session_id,
            scenario,
            tools,
            runtime,
        })
    }

    #[staticmethod]
    #[pyo3(name = "_with_scripted_tool_call")]
    fn with_scripted_tool_call(
        tool_name: String,
        arguments_json: String,
        final_text: String,
    ) -> PyResult<Self> {
        let tool_name = ToolName::new(&tool_name).map_err(|_error| {
            error::runtime_message_to_py(
                "tool.registration_invalid",
                "Invalid Merry tool name.",
                Some("Use a non-empty provider-portable tool name."),
            )
        })?;
        let arguments = parse_json_object(&arguments_json, "tool.input_invalid")?;
        let session_id =
            SessionId::new("python-sdk-scripted-tool").expect("static session id must be valid");
        let scenario = RuntimeScenario::ScriptedToolCall {
            tool_name,
            arguments,
            final_text,
        };
        let tools = Vec::new();
        let runtime = build_runtime_from(session_id.clone(), &scenario, &tools)
            .map_err(error::runtime_error_to_py)?;

        Ok(Self {
            session_id,
            scenario,
            tools,
            runtime,
        })
    }

    #[staticmethod]
    #[pyo3(name = "_with_scripted_tool_calls")]
    fn with_scripted_tool_calls(calls_json: String, final_text: String) -> PyResult<Self> {
        let calls = parse_scripted_tool_calls(&calls_json)?;
        let session_id =
            SessionId::new("python-sdk-scripted-tools").expect("static session id must be valid");
        let scenario = RuntimeScenario::ScriptedToolCalls { calls, final_text };
        let tools = Vec::new();
        let runtime = build_runtime_from(session_id.clone(), &scenario, &tools)
            .map_err(error::runtime_error_to_py)?;

        Ok(Self {
            session_id,
            scenario,
            tools,
            runtime,
        })
    }

    #[pyo3(name = "_register_static_tool_failure")]
    fn register_static_tool_failure(
        &mut self,
        name: String,
        description: String,
        diagnostic_code: String,
        message: String,
        content_json: String,
    ) -> PyResult<()> {
        let content = parse_json_object(&content_json, "tool.input_invalid")?;
        let tool = static_tool(
            &name,
            &description,
            StaticToolBehavior::DomainFailure {
                diagnostic_code,
                message,
                content: Value::Object(content).to_string(),
            },
        )?;
        self.tools.push(tool);
        self.rebuild_runtime().map_err(error::runtime_error_to_py)?;
        Ok(())
    }

    #[pyo3(name = "_register_static_tool_exception")]
    fn register_static_tool_exception(
        &mut self,
        name: String,
        description: String,
        message: String,
    ) -> PyResult<()> {
        let tool = static_tool(
            &name,
            &description,
            StaticToolBehavior::ExecutorException { message },
        )?;
        self.tools.push(tool);
        self.rebuild_runtime().map_err(error::runtime_error_to_py)?;
        Ok(())
    }

    #[pyo3(signature = (task, final_output_schema_json=None, max_model_turns=None))]
    fn run_blocking(
        &self,
        py: Python<'_>,
        task: String,
        final_output_schema_json: Option<String>,
        max_model_turns: Option<usize>,
    ) -> PyResult<Py<PyAny>> {
        let runtime = self.runtime.clone();
        let result = py.detach(move || {
            run_agent_loop_blocking(runtime, task, final_output_schema_json, max_model_turns)
        })?;
        agent_loop_result_to_python(py, result)
    }

    #[pyo3(signature = (task, final_output_schema_json=None, max_model_turns=None))]
    fn run_stream_blocking(
        &self,
        task: String,
        final_output_schema_json: Option<String>,
        max_model_turns: Option<usize>,
    ) -> NativeRuntimeEventStream {
        let runtime = self.runtime.clone();
        let (sender, receiver) = mpsc::channel();
        let (command_sender, command_receiver) = tokio::sync::mpsc::unbounded_channel();
        thread::spawn(move || {
            if let Err(error) = run_agent_loop_event_stream_blocking(
                runtime,
                task,
                final_output_schema_json,
                max_model_turns,
                sender.clone(),
                command_receiver,
            ) {
                let _ = sender.send(StreamRunnerMessage::Error {
                    code: error.code,
                    message: error.message,
                    hint: error.hint,
                });
            }
        });

        NativeRuntimeEventStream {
            command_sender,
            state: Arc::new(Mutex::new(NativeRuntimeEventStreamState {
                receiver,
                events: Vec::new(),
                result: None,
                error: None,
                finished: false,
            })),
        }
    }

    fn register_bridge_tool(
        &mut self,
        name: String,
        description: String,
        schema_json: String,
    ) -> PyResult<()> {
        let schema = parse_schema(&schema_json)?;
        let tool = bridge_tool(&name, &description, schema)?;
        self.tools.push(tool);
        self.rebuild_runtime().map_err(error::runtime_error_to_py)?;
        Ok(())
    }

    fn submit_tool_success_json_blocking(
        &self,
        py: Python<'_>,
        call_id: String,
        artifact_id: String,
        content_json: String,
    ) -> PyResult<Py<PyAny>> {
        let runtime = self.runtime.clone();
        let result = py.detach(move || {
            submit_tool_success_json_blocking(runtime, call_id, artifact_id, content_json)
        })?;
        runtime_events_to_python(py, &result)
    }
}

#[pymethods]
impl NativeRuntimeEventStream {
    fn next_blocking(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        let state = Arc::clone(&self.state);
        let message = py.detach(move || {
            let mut state = state.lock().map_err(|_| StreamReceiveError::Poisoned)?;
            receive_stream_message(&mut state)
        });

        match message {
            Ok(StreamRunnerMessage::Event(event)) => Ok(Some(runtime_event_to_python(py, &event)?)),
            Ok(StreamRunnerMessage::Finished { .. }) | Err(StreamReceiveError::Closed) => Ok(None),
            Ok(StreamRunnerMessage::Error {
                code,
                message,
                hint,
            }) => Err(error::runtime_message_to_py(code, &message, hint)),
            Err(StreamReceiveError::Poisoned) => Err(error::runtime_message_to_py(
                "runtime.stream_poisoned",
                "Runtime event stream receiver was poisoned.",
                Some("Retry the operation in a fresh Python process."),
            )),
        }
    }

    fn submit_tool_success_json_blocking(
        &self,
        py: Python<'_>,
        call_id: String,
        artifact_id: String,
        content_json: String,
    ) -> PyResult<()> {
        let command_sender = self.command_sender.clone();
        py.detach(move || {
            let (ack_sender, ack_receiver) = mpsc::channel();
            command_sender
                .send(StreamRunnerCommand::SubmitToolSuccessJson {
                    call_id,
                    artifact_id,
                    content_json,
                    ack_sender,
                })
                .map_err(|_| {
                    error::runtime_message_to_py(
                        "runtime.stream_closed",
                        "Runtime event stream closed before accepting the bridge tool result.",
                        Some("Consume bridge tool events from the active RuntimeStream."),
                    )
                })?;
            match ack_receiver.recv() {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err(error::runtime_message_to_py(
                    error.code,
                    &error.message,
                    error.hint,
                )),
                Err(_) => Err(error::runtime_message_to_py(
                    "runtime.stream_closed",
                    "Runtime event stream closed before recording the bridge tool result.",
                    Some("Consume bridge tool events from the active RuntimeStream."),
                )),
            }
        })
    }

    fn result_blocking(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let state = Arc::clone(&self.state);
        let result = py.detach(move || {
            let mut state = state
                .lock()
                .map_err(|_| StreamResultError::Receive(StreamReceiveError::Poisoned))?;
            while !state.finished {
                match receive_stream_message(&mut state)? {
                    StreamRunnerMessage::Event(_) => {}
                    StreamRunnerMessage::Finished { .. } => {}
                    StreamRunnerMessage::Error {
                        code,
                        message,
                        hint,
                    } => {
                        return Err(StreamResultError::Runtime {
                            code,
                            message,
                            hint,
                        });
                    }
                }
            }

            if let Some(error) = state.error.clone() {
                return Err(StreamResultError::Runtime {
                    code: error.code,
                    message: error.message,
                    hint: error.hint,
                });
            }

            state.result.clone().ok_or(StreamResultError::MissingResult)
        });

        match result {
            Ok(result) => agent_loop_result_to_python(py, result),
            Err(StreamResultError::Receive(StreamReceiveError::Poisoned)) => {
                Err(error::runtime_message_to_py(
                    "runtime.stream_poisoned",
                    "Runtime event stream receiver was poisoned.",
                    Some("Retry the operation in a fresh Python process."),
                ))
            }
            Err(StreamResultError::Receive(StreamReceiveError::Closed)) => {
                Err(error::runtime_message_to_py(
                    "runtime.stream_closed",
                    "Runtime event stream closed before producing a result.",
                    Some("Retry the operation or use Runtime.run(...)."),
                ))
            }
            Err(StreamResultError::Runtime {
                code,
                message,
                hint,
            }) => Err(error::runtime_message_to_py(code, &message, hint)),
            Err(StreamResultError::MissingResult) => Err(error::runtime_message_to_py(
                "runtime.stream_result_missing",
                "Runtime event stream finished without a result.",
                Some("Retry the operation or use Runtime.run(...)."),
            )),
        }
    }
}

fn receive_stream_message(
    state: &mut NativeRuntimeEventStreamState,
) -> Result<StreamRunnerMessage, StreamReceiveError> {
    if state.finished {
        return Err(StreamReceiveError::Closed);
    }

    let message = state
        .receiver
        .recv()
        .map_err(|_| StreamReceiveError::Closed)?;
    match &message {
        StreamRunnerMessage::Event(event) => state.events.push(event.clone()),
        StreamRunnerMessage::Finished { result } => {
            state.result = result.clone();
            state.finished = true;
        }
        StreamRunnerMessage::Error { .. } => {
            if let StreamRunnerMessage::Error {
                code,
                message,
                hint,
            } = &message
            {
                state.error = Some(StreamRunnerError {
                    code,
                    message: message.clone(),
                    hint: *hint,
                });
            }
            state.finished = true;
        }
    }
    Ok(message)
}

impl PyRuntime {
    fn rebuild_runtime(&mut self) -> Result<(), merry_runtime::RuntimeError> {
        self.runtime = build_runtime_from(self.session_id.clone(), &self.scenario, &self.tools)?;
        Ok(())
    }
}

fn build_runtime_from(
    session_id: SessionId,
    scenario: &RuntimeScenario,
    tools: &[RegisteredTool],
) -> Result<Runtime, merry_runtime::RuntimeError> {
    let mut builder = Runtime::builder(session_id);
    builder = configure_scenario(builder, scenario);
    if tools.iter().any(|tool| tool.runner() == ToolRunner::Bridge) {
        builder = builder.allow_bridge_tools();
    }
    for tool in tools {
        builder = builder.register_tool(tool.clone());
    }
    builder.build()
}

fn configure_scenario(builder: RuntimeBuilder, scenario: &RuntimeScenario) -> RuntimeBuilder {
    match scenario {
        RuntimeScenario::Empty => builder,
        RuntimeScenario::OpenAiCompatible { config, model } => {
            builder.model_provider(Arc::new(OpenAiProvider::new(config.clone())), model.clone())
        }
        RuntimeScenario::FakeResponse { final_text } => builder.model_provider(
            Arc::new(fake_response_provider(final_text)),
            fake_model_name(),
        ),
        RuntimeScenario::ScriptedToolCall {
            tool_name,
            arguments,
            final_text,
        } => builder.model_provider(
            Arc::new(scripted_tool_call_provider(
                tool_name.clone(),
                arguments.clone(),
                final_text,
            )),
            fake_model_name(),
        ),
        RuntimeScenario::ScriptedToolCalls { calls, final_text } => builder.model_provider(
            Arc::new(scripted_tool_calls_provider(calls, final_text)),
            fake_model_name(),
        ),
    }
}

fn fake_response_provider(final_text: &str) -> FakeModelProvider {
    FakeModelProvider::new(vec![
        Ok(ModelEvent::Started),
        Ok(ModelEvent::OutputTextDelta {
            delta: final_text.to_owned(),
        }),
        Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text(final_text)],
                FinishReason::Stop,
                None,
            ),
        }),
    ])
}

fn scripted_tool_call_provider(
    tool_name: ToolName,
    arguments: Map<String, Value>,
    final_text: &str,
) -> ScriptedModelProvider {
    scripted_tool_calls_provider(
        &[ScriptedToolCallSpec {
            tool_name,
            arguments,
        }],
        final_text,
    )
}

fn scripted_tool_calls_provider(
    calls: &[ScriptedToolCallSpec],
    final_text: &str,
) -> ScriptedModelProvider {
    let mut responses = Vec::new();
    for (index, call) in calls.iter().enumerate() {
        let call_id = if calls.len() == 1 {
            "call-python-tool".to_owned()
        } else {
            format!("call-python-tool-{}", index + 1)
        };
        let call = ModelToolCall::new(
            ModelToolCallId::new(&call_id).expect("scripted tool call id must be valid"),
            call.tool_name.clone(),
            ToolArguments::new(call.arguments.clone()),
        );
        responses.push(vec![
            ModelEvent::Started,
            ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::tool_call(call)],
                    FinishReason::ToolCalls,
                    None,
                ),
            },
        ]);
    }
    responses.push(vec![
        ModelEvent::Started,
        ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text(final_text)],
                FinishReason::Stop,
                None,
            ),
        },
    ]);

    ScriptedModelProvider::new(responses)
}

struct ScriptedModelProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    responses: Arc<Vec<Vec<ModelEvent>>>,
    next_response: Arc<Mutex<usize>>,
}

impl ScriptedModelProvider {
    fn new(responses: Vec<Vec<ModelEvent>>) -> Self {
        Self {
            name: ProviderName::new("python-sdk-scripted-provider")
                .expect("static provider name must be valid"),
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .expect("static capabilities must be valid"),
            responses: Arc::new(responses),
            next_response: Arc::new(Mutex::new(0)),
        }
    }
}

impl ModelProvider for ScriptedModelProvider {
    fn name(&self) -> &ProviderName {
        &self.name
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn stream_model<'a>(
        &'a self,
        _request: ModelRequest,
        context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ModelError::Cancelled);
            }

            let mut next_response = self
                .next_response
                .lock()
                .expect("scripted provider mutex must not be poisoned");
            let response = self.responses.get(*next_response).cloned().ok_or_else(|| {
                ModelError::provider(
                    ProviderErrorKind::Protocol,
                    "scripted provider response exhausted",
                )
            })?;
            *next_response += 1;

            let stream: ModelEventStream = Box::pin(ScriptedModelEventStream {
                events: response,
                index: 0,
                completed: false,
                cancellation_token: context.cancellation_token().clone(),
            });
            Ok(stream)
        })
    }
}

struct ScriptedModelEventStream {
    events: Vec<ModelEvent>,
    index: usize,
    completed: bool,
    cancellation_token: CancellationToken,
}

impl Stream for ScriptedModelEventStream {
    type Item = Result<ModelEvent, ModelError>;

    fn poll_next(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.completed {
            return Poll::Ready(None);
        }
        if self.cancellation_token.is_cancelled() {
            self.completed = true;
            return Poll::Ready(Some(Err(ModelError::Cancelled)));
        }

        let item = match self.events.get(self.index).cloned() {
            Some(item) => item,
            None => {
                self.completed = true;
                return Poll::Ready(None);
            }
        };
        self.index += 1;
        if matches!(item, ModelEvent::Completed { .. }) {
            self.completed = true;
        }

        Poll::Ready(Some(Ok(item)))
    }
}

fn static_tool(
    name: &str,
    description: &str,
    behavior: StaticToolBehavior,
) -> PyResult<RegisteredTool> {
    let spec = ToolSpec::new(
        ToolName::new(name).map_err(|_error| {
            error::runtime_message_to_py(
                "tool.registration_invalid",
                "Invalid Merry tool name.",
                Some("Use a non-empty provider-portable tool name."),
            )
        })?,
        description,
        ToolInputSchema::new(object_input_schema()).map_err(|_error| {
            error::runtime_message_to_py(
                "tool.registration_invalid",
                "Invalid Merry tool input schema.",
                Some("Use an object JSON schema for tool input."),
            )
        })?,
    )
    .map_err(|_error| {
        error::runtime_message_to_py(
            "tool.registration_invalid",
            "Invalid Merry tool specification.",
            Some("Use a non-empty tool description without control characters."),
        )
    })?;

    Ok(RegisteredTool::read_only(
        spec,
        Arc::new(StaticToolExecutor { behavior }),
    ))
}

fn bridge_tool(name: &str, description: &str, schema: Schema) -> PyResult<RegisteredTool> {
    let spec = ToolSpec::new(
        ToolName::new(name).map_err(|_error| {
            error::runtime_message_to_py(
                "tool.registration_invalid",
                "Invalid Merry tool name.",
                Some("Use a non-empty provider-portable tool name."),
            )
        })?,
        description,
        ToolInputSchema::new(schema).map_err(|_error| {
            error::runtime_message_to_py(
                "tool.registration_invalid",
                "Invalid Merry tool input schema.",
                Some("Use an object JSON schema for tool input."),
            )
        })?,
    )
    .map_err(|_error| {
        error::runtime_message_to_py(
            "tool.registration_invalid",
            "Invalid Merry tool specification.",
            Some("Use a non-empty tool description without control characters."),
        )
    })?;

    Ok(RegisteredTool::bridge(spec))
}

fn object_input_schema() -> Schema {
    Schema::try_from(json!({
        "type": "object",
        "additionalProperties": true
    }))
    .expect("static object schema must be valid")
}

fn parse_schema(json_text: &str) -> PyResult<Schema> {
    parse_schema_with_hint(
        json_text,
        "tool.schema_invalid",
        "Pass a JSON-serializable object JSON schema.",
    )
    .map_err(SchemaMessage::into_py_error)
}

fn parse_schema_with_hint(
    json_text: &str,
    code: &'static str,
    hint: &'static str,
) -> Result<Schema, SchemaMessage> {
    let value = serde_json::from_str::<Value>(json_text)
        .map_err(|source| SchemaMessage::new(code, source.to_string(), hint))?;
    Schema::try_from(value).map_err(|source| SchemaMessage::new(code, source.to_string(), hint))
}

fn parse_json_object(json_text: &str, code: &str) -> PyResult<Map<String, Value>> {
    match serde_json::from_str::<Value>(json_text) {
        Ok(Value::Object(value)) => Ok(value),
        Ok(_) => Err(error::runtime_message_to_py(
            code,
            "JSON value must be an object.",
            Some("Pass a mapping from Python."),
        )),
        Err(source) => Err(error::runtime_message_to_py(
            code,
            &source.to_string(),
            Some("Pass JSON-serializable mapping values."),
        )),
    }
}

fn parse_scripted_tool_calls(json_text: &str) -> PyResult<Vec<ScriptedToolCallSpec>> {
    let value = serde_json::from_str::<Value>(json_text).map_err(|source| {
        error::runtime_message_to_py(
            "tool.input_invalid",
            &source.to_string(),
            Some("Pass a JSON array of scripted tool call objects."),
        )
    })?;
    let Value::Array(items) = value else {
        return Err(error::runtime_message_to_py(
            "tool.input_invalid",
            "Scripted tool calls must be a JSON array.",
            Some("Pass [{'name': ..., 'arguments': {...}}] from Python tests."),
        ));
    };

    let mut calls = Vec::with_capacity(items.len());
    for item in items {
        let Value::Object(mut object) = item else {
            return Err(error::runtime_message_to_py(
                "tool.input_invalid",
                "Each scripted tool call must be a JSON object.",
                Some("Pass {'name': ..., 'arguments': {...}} for each call."),
            ));
        };
        let Some(Value::String(name)) = object.remove("name") else {
            return Err(error::runtime_message_to_py(
                "tool.input_invalid",
                "Each scripted tool call must include a string name.",
                Some("Pass a provider-portable Merry tool name."),
            ));
        };
        let Some(Value::Object(arguments)) = object.remove("arguments") else {
            return Err(error::runtime_message_to_py(
                "tool.input_invalid",
                "Each scripted tool call must include object arguments.",
                Some("Pass JSON-serializable mapping values."),
            ));
        };
        if !object.is_empty() {
            return Err(error::runtime_message_to_py(
                "tool.input_invalid",
                "Scripted tool call objects cannot include unknown fields.",
                Some("Use only name and arguments."),
            ));
        }
        let tool_name = ToolName::new(&name).map_err(|_error| {
            error::runtime_message_to_py(
                "tool.registration_invalid",
                "Invalid Merry tool name.",
                Some("Use a non-empty provider-portable tool name."),
            )
        })?;
        calls.push(ScriptedToolCallSpec {
            tool_name,
            arguments,
        });
    }

    Ok(calls)
}

fn fake_model_name() -> ModelName {
    ModelName::new("fake/python-sdk").expect("static model name must be valid")
}

fn run_agent_loop_blocking(
    runtime: Runtime,
    task: String,
    final_output_schema_json: Option<String>,
    max_model_turns: Option<usize>,
) -> PyResult<AgentLoopResult> {
    let final_output_contract = final_output_contract_from_schema_json(final_output_schema_json)
        .map_err(SchemaMessage::into_py_error)?;
    let config = agent_loop_config(final_output_contract, max_model_turns).map_err(|message| {
        error::runtime_message_to_py(message.code, &message.message, Some(message.hint))
    })?;
    let tokio_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|source| {
            error::runtime_message_to_py(
                "runtime.tokio_init_failed",
                &source.to_string(),
                Some("Retry the operation in a fresh Python thread or process."),
            )
        })?;

    tokio_runtime.block_on(async move {
        let input = StepInput::user_text(&task).map_err(error::runtime_error_to_py)?;
        let context = StepContext::new(CancellationToken::new());
        runtime
            .run_agent_loop(input, context, config)
            .await
            .map_err(error::agent_loop_error_to_py)
    })
}

fn run_agent_loop_event_stream_blocking(
    runtime: Runtime,
    task: String,
    final_output_schema_json: Option<String>,
    max_model_turns: Option<usize>,
    sender: mpsc::Sender<StreamRunnerMessage>,
    command_receiver: tokio::sync::mpsc::UnboundedReceiver<StreamRunnerCommand>,
) -> Result<(), StreamRunnerError> {
    let final_output_contract = final_output_contract_from_schema_json(final_output_schema_json)?;
    let config = agent_loop_config(final_output_contract, max_model_turns)?;
    let tokio_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|source| StreamRunnerError {
            code: "runtime.tokio_init_failed",
            message: source.to_string(),
            hint: Some("Retry the operation in a fresh Python thread or process."),
        })?;

    tokio_runtime.block_on(async move {
        run_agent_loop_event_stream(runtime, task, config, sender, command_receiver)
            .await
            .map_err(|message| StreamRunnerError {
                code: "runtime.stream_failed",
                message,
                hint: Some("Retry the operation or use Runtime.run(...) for a collected result."),
            })
    })
}

async fn run_agent_loop_event_stream(
    runtime: Runtime,
    task: String,
    config: AgentLoopConfig,
    sender: mpsc::Sender<StreamRunnerMessage>,
    mut command_receiver: tokio::sync::mpsc::UnboundedReceiver<StreamRunnerCommand>,
) -> Result<(), String> {
    let input = StepInput::user_text(&task).map_err(|source| source.to_string())?;
    let context = StepContext::new(CancellationToken::new());
    let mut stream = runtime
        .run_agent_loop_stream(input, context, config)
        .map_err(|source| source.to_string())?;

    loop {
        tokio::select! {
            event = stream.next() => {
                let Some(event) = event else {
                    break;
                };
                if sender.send(StreamRunnerMessage::Event(event)).is_err() {
                    return Ok(());
                }
            }
            command = command_receiver.recv() => {
                let Some(command) = command else {
                    return Ok(());
                };
                handle_stream_command(&stream, command).await;
            }
        }
    }

    let result = stream.result().await;
    let _ = sender.send(StreamRunnerMessage::Finished { result });
    Ok(())
}

async fn handle_stream_command(
    stream: &merry_runtime::AgentLoopEventStream,
    command: StreamRunnerCommand,
) {
    match command {
        StreamRunnerCommand::SubmitToolSuccessJson {
            call_id,
            artifact_id,
            content_json,
            ack_sender,
        } => {
            let result =
                parse_stream_tool_success(call_id, artifact_id, content_json).map_err(|error| {
                    StreamRunnerError {
                        code: "tool.result_invalid",
                        message: error,
                        hint: Some("Use the call id emitted by the active bridge tool request."),
                    }
                });
            let result = match result {
                Ok((result, content)) => stream
                    .submit_bridge_tool_result(result, content)
                    .await
                    .map_err(|source| StreamRunnerError {
                        code: "runtime.error",
                        message: source.to_string(),
                        hint: Some("Inspect the active runtime stream and bridge tool call id."),
                    }),
                Err(error) => Err(error),
            };
            let _ = ack_sender.send(result);
        }
    }
}

fn parse_stream_tool_success(
    call_id: String,
    artifact_id: String,
    content_json: String,
) -> Result<(merry_core::ToolCallResult, ArtifactContent), String> {
    let call_id = ToolCallId::new(&call_id).map_err(|source| source.to_string())?;
    let artifact = ArtifactRef::new(
        ArtifactId::new(&artifact_id).map_err(|source| source.to_string())?,
        ArtifactKind::Json,
    );
    Ok((
        merry_core::ToolCallResult::succeeded(call_id, artifact),
        ArtifactContent::json(content_json),
    ))
}

#[derive(Clone)]
struct SchemaMessage {
    code: &'static str,
    message: String,
    hint: &'static str,
}

impl SchemaMessage {
    fn new(code: &'static str, message: String, hint: &'static str) -> Self {
        Self {
            code,
            message,
            hint,
        }
    }

    fn into_py_error(self) -> PyErr {
        error::runtime_message_to_py(self.code, &self.message, Some(self.hint))
    }
}

impl From<SchemaMessage> for StreamRunnerError {
    fn from(message: SchemaMessage) -> Self {
        Self {
            code: message.code,
            message: message.message,
            hint: Some(message.hint),
        }
    }
}

fn final_output_contract_from_schema_json(
    schema_json: Option<String>,
) -> Result<Option<FinalOutputContract>, SchemaMessage> {
    let Some(schema_json) = schema_json else {
        return Ok(None);
    };

    let schema = parse_schema_with_hint(
        &schema_json,
        "final_output.schema_invalid",
        FINAL_OUTPUT_SCHEMA_HINT,
    )?;
    let input_schema = ToolInputSchema::new(schema).map_err(|source| {
        SchemaMessage::new(
            "final_output.schema_invalid",
            source.to_string(),
            FINAL_OUTPUT_SCHEMA_HINT,
        )
    })?;
    FinalOutputContract::new(input_schema)
        .map(Some)
        .map_err(|source| {
            SchemaMessage::new(
                "final_output.schema_invalid",
                source.to_string(),
                FINAL_OUTPUT_SCHEMA_HINT,
            )
        })
}

fn agent_loop_config(
    final_output_contract: Option<FinalOutputContract>,
    max_model_turns: Option<usize>,
) -> Result<AgentLoopConfig, SchemaMessage> {
    let config = match max_model_turns {
        Some(max_model_turns) => AgentLoopConfig::new(max_model_turns).map_err(|source| {
            SchemaMessage::new(
                "runtime.max_model_turns_invalid",
                source.to_string(),
                "Pass max_model_turns as an integer greater than zero.",
            )
        })?,
        None => AgentLoopConfig::default(),
    };
    match final_output_contract {
        Some(contract) => Ok(config.with_final_output_contract(contract)),
        None => Ok(config),
    }
}

fn submit_tool_success_json_blocking(
    runtime: Runtime,
    call_id: String,
    artifact_id: String,
    content_json: String,
) -> PyResult<Vec<merry_core::RuntimeEvent>> {
    let tokio_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|source| {
            error::runtime_message_to_py(
                "runtime.tokio_init_failed",
                &source.to_string(),
                Some("Retry the operation in a fresh Python thread or process."),
            )
        })?;

    tokio_runtime.block_on(async move {
        let call_id = ToolCallId::new(&call_id).map_err(|source| {
            error::runtime_message_to_py(
                "tool.result_invalid",
                &source.to_string(),
                Some("Use the call id emitted by the runtime tool_call_pending event."),
            )
        })?;
        let artifact = ArtifactRef::new(
            ArtifactId::new(&artifact_id).map_err(|source| {
                error::runtime_message_to_py(
                    "tool.result_invalid",
                    &source.to_string(),
                    Some("Use a non-empty non-reserved artifact id."),
                )
            })?,
            ArtifactKind::Json,
        );
        let result = merry_core::ToolCallResult::succeeded(call_id, artifact);
        runtime
            .submit_tool_result(result, ArtifactContent::json(content_json))
            .await
            .map_err(error::runtime_error_to_py)
    })
}

fn agent_loop_result_to_python(py: Python<'_>, result: AgentLoopResult) -> PyResult<Py<PyAny>> {
    let dict = PyDict::new(py);
    dict.set_item("status", status_label(result.status()))?;
    dict.set_item("model_turns_run", result.model_turns_run())?;
    dict.set_item("final_output", result.final_output())?;
    dict.set_item(
        "final_output_json",
        result
            .final_output_json()
            .map(merry_runtime::FinalOutput::json),
    )?;

    let events = serde_json::to_value(result.events()).expect("RuntimeEvent values must serialize");
    dict.set_item("events", json_to_py(py, &events)?)?;

    Ok(dict.unbind().into_any())
}

fn runtime_events_to_python(
    py: Python<'_>,
    events: &[merry_core::RuntimeEvent],
) -> PyResult<Py<PyAny>> {
    let value = serde_json::to_value(events).expect("RuntimeEvent values must serialize");
    json_to_py(py, &value)
}

fn runtime_event_to_python(
    py: Python<'_>,
    event: &merry_core::RuntimeEvent,
) -> PyResult<Py<PyAny>> {
    let value = serde_json::to_value(event).expect("RuntimeEvent values must serialize");
    json_to_py(py, &value)
}

fn status_label(status: &AgentLoopStatus) -> &'static str {
    match status {
        AgentLoopStatus::Completed => "completed",
        AgentLoopStatus::Failed { .. } => "failed",
        AgentLoopStatus::Cancelled { .. } => "cancelled",
        AgentLoopStatus::Blocked { .. } => "blocked",
        _ => "blocked",
    }
}
