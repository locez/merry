//! Debug and demonstration CLI for Merry.

use futures_util::StreamExt;
use merry_core::{
    PendingToolCall, RuntimeEvent, RuntimeEventKind, SessionId, ToolInputSchema, ToolName, ToolSpec,
};
use merry_llm::{GenerationConfig, ModelName};
use merry_provider_openai::{OpenAiProvider, OpenAiProviderConfig};
use merry_runtime::{
    RegisteredTool, Runtime, StepContext, StepInput, ToolExecutionContext, ToolExecutionOutcome,
    ToolExecutor, ToolExecutorFuture,
};
use std::{
    env, fmt, io,
    process::{ExitCode, Termination},
    sync::Arc,
};
use tokio::io::{AsyncWrite, AsyncWriteExt, BufWriter};

const DEFAULT_SESSION_ID: &str = "debug-session";
const DEFAULT_INPUT: &str = "debug step";
const DEBUG_TOOL_NAME: &str = "debug_echo";
const DEBUG_TOOL_CONTINUATION_INPUT: &str = "continue after debug tool";

fn main() -> CliExit {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => return CliExit::Unexpected(err.to_string()),
    };

    runtime.block_on(async_main())
}

async fn async_main() -> CliExit {
    match parse_args(env::args().skip(1)) {
        Ok(Command::Help) => {
            print!("{}", root_usage());
            CliExit::Success
        }
        Ok(Command::DebugHelp) => {
            print!("{}", debug_usage());
            CliExit::Success
        }
        Ok(Command::DebugOpenAiHelp) => {
            print!("{}", debug_openai_usage());
            CliExit::Success
        }
        Ok(Command::Debug { session_id, input }) => match run_debug(&session_id, &input).await {
            Ok(()) => CliExit::Success,
            Err(CliError::BrokenPipe) => CliExit::Success,
            Err(CliError::DebugUsage(message)) => CliExit::Usage {
                message,
                usage: debug_usage(),
            },
            Err(CliError::DebugOpenAiUsage(message)) => CliExit::Usage {
                message,
                usage: debug_openai_usage(),
            },
            Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
        },
        Ok(Command::DebugOpenAi {
            input,
            model,
            max_output_tokens,
            debug_tool_result,
        }) => match run_debug_openai(
            &input,
            model.as_deref(),
            max_output_tokens,
            debug_tool_result.as_deref(),
        )
        .await
        {
            Ok(()) => CliExit::Success,
            Err(CliError::BrokenPipe) => CliExit::Success,
            Err(CliError::DebugOpenAiUsage(message)) => CliExit::Usage {
                message,
                usage: debug_openai_usage(),
            },
            Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
            Err(CliError::DebugUsage(message)) => CliExit::Usage {
                message,
                usage: debug_usage(),
            },
        },
        Err(ParseError::Root(message)) => CliExit::Usage {
            message,
            usage: root_usage(),
        },
        Err(ParseError::Debug(message)) => CliExit::Usage {
            message,
            usage: debug_usage(),
        },
        Err(ParseError::DebugOpenAi(message)) => CliExit::Usage {
            message,
            usage: debug_openai_usage(),
        },
    }
}

enum Command {
    Help,
    DebugHelp,
    DebugOpenAiHelp,
    Debug {
        session_id: String,
        input: String,
    },
    DebugOpenAi {
        input: String,
        model: Option<String>,
        max_output_tokens: Option<u64>,
        debug_tool_result: Option<String>,
    },
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Command, ParseError> {
    let mut args = args.into_iter();
    let Some(command) = args.next() else {
        return Err(ParseError::Root("missing command".to_owned()));
    };

    match command.as_str() {
        "--help" => {
            if let Some(extra) = args.next() {
                return Err(ParseError::Root(format!(
                    "unexpected argument after --help: {extra}"
                )));
            }
            Ok(Command::Help)
        }
        "debug" => parse_debug_args(args),
        other => Err(ParseError::Root(format!("unknown command: {other}"))),
    }
}

fn parse_debug_args(args: impl IntoIterator<Item = String>) -> Result<Command, ParseError> {
    let mut session_id = DEFAULT_SESSION_ID.to_owned();
    let mut input = DEFAULT_INPUT.to_owned();
    let mut args = args.into_iter();

    let Some(first) = args.next() else {
        return Ok(Command::Debug { session_id, input });
    };

    if first == "openai" {
        return parse_debug_openai_args(args);
    }

    let mut args = std::iter::once(first).chain(args);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" => {
                if let Some(extra) = args.next() {
                    return Err(ParseError::Debug(format!(
                        "unexpected argument after debug --help: {extra}"
                    )));
                }
                return Ok(Command::DebugHelp);
            }
            "--session-id" => {
                session_id = args
                    .next()
                    .ok_or_else(|| ParseError::Debug("--session-id requires a value".to_owned()))?;
            }
            "--input" => {
                input = args
                    .next()
                    .ok_or_else(|| ParseError::Debug("--input requires a value".to_owned()))?;
            }
            other if other.starts_with("--") => {
                return Err(ParseError::Debug(format!("unknown debug option: {other}")));
            }
            other => {
                return Err(ParseError::Debug(format!(
                    "unexpected debug argument: {other}"
                )));
            }
        }
    }

    Ok(Command::Debug { session_id, input })
}

fn parse_debug_openai_args(args: impl IntoIterator<Item = String>) -> Result<Command, ParseError> {
    let mut input = None;
    let mut model = None;
    let mut max_output_tokens = None;
    let mut debug_tool_result = None;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" => {
                if let Some(extra) = args.next() {
                    return Err(ParseError::DebugOpenAi(format!(
                        "unexpected argument after debug openai --help: {extra}"
                    )));
                }
                return Ok(Command::DebugOpenAiHelp);
            }
            "--input" => {
                input = Some(args.next().ok_or_else(|| {
                    ParseError::DebugOpenAi("--input requires a value".to_owned())
                })?);
            }
            "--model" => {
                model = Some(args.next().ok_or_else(|| {
                    ParseError::DebugOpenAi("--model requires a value".to_owned())
                })?);
            }
            "--max-output-tokens" => {
                let value = args.next().ok_or_else(|| {
                    ParseError::DebugOpenAi("--max-output-tokens requires a value".to_owned())
                })?;
                max_output_tokens = Some(parse_max_output_tokens(&value)?);
            }
            "--debug-tool-result" => {
                debug_tool_result = Some(args.next().ok_or_else(|| {
                    ParseError::DebugOpenAi("--debug-tool-result requires a value".to_owned())
                })?);
            }
            other if other.starts_with("--") => {
                return Err(ParseError::DebugOpenAi(format!(
                    "unknown debug openai option: {other}"
                )));
            }
            other => {
                return Err(ParseError::DebugOpenAi(format!(
                    "unexpected debug openai argument: {other}"
                )));
            }
        }
    }

    let input =
        input.ok_or_else(|| ParseError::DebugOpenAi("--input requires a value".to_owned()))?;

    Ok(Command::DebugOpenAi {
        input,
        model,
        max_output_tokens,
        debug_tool_result,
    })
}

fn parse_max_output_tokens(value: &str) -> Result<u64, ParseError> {
    let tokens = value.parse::<u64>().map_err(|error| {
        ParseError::DebugOpenAi(format!(
            "--max-output-tokens must be a positive integer: {error}"
        ))
    })?;

    if tokens == 0 {
        return Err(ParseError::DebugOpenAi(
            "--max-output-tokens must be greater than zero".to_owned(),
        ));
    }

    Ok(tokens)
}

async fn run_debug(session_id: &str, input: &str) -> Result<(), CliError> {
    let session_id = SessionId::new(session_id).map_err(usage_error)?;
    let runtime = Runtime::builder(session_id).build().map_err(unexpected)?;
    let input = StepInput::user_text(input).map_err(usage_error)?;
    write_runtime_step_events(&runtime, input, StepContext::default(), tokio::io::stdout()).await
}

async fn run_debug_openai(
    input: &str,
    model: Option<&str>,
    max_output_tokens: Option<u64>,
    debug_tool_result: Option<&str>,
) -> Result<(), CliError> {
    let config = debug_openai_config(model)?;

    let session_id = SessionId::new(DEFAULT_SESSION_ID).map_err(debug_openai_usage_error)?;
    let model = ModelName::new(&config.model).map_err(debug_openai_usage_error)?;
    let provider = OpenAiProvider::new(config.provider);
    let mut builder = Runtime::builder(session_id).model_provider(Arc::new(provider), model);
    if let Some(result) = debug_tool_result {
        builder = builder.register_tool(debug_echo_tool(result)?);
    }
    let runtime = builder.build().map_err(unexpected)?;
    let input = StepInput::user_text(input).map_err(debug_openai_usage_error)?;
    let generation_config =
        GenerationConfig::new(max_output_tokens, false).map_err(debug_openai_usage_error)?;
    let context = StepContext::new(Default::default()).with_generation_config(generation_config);

    if debug_tool_result.is_some() {
        write_debug_openai_tool_events(&runtime, input, context, tokio::io::stdout()).await
    } else {
        write_runtime_step_events(&runtime, input, context, tokio::io::stdout()).await
    }
}

async fn write_runtime_step_events<W>(
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

async fn write_debug_openai_tool_events<W>(
    runtime: &Runtime,
    input: StepInput,
    context: StepContext,
    writer: W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let mut writer = BufWriter::new(writer);
    let events = write_runtime_step_events_to(runtime, input, context.clone(), &mut writer).await?;
    let pending = first_pending_tool_call(&events);

    let Some(pending) = pending else {
        writer.flush().await.map_err(stdout_error)?;
        return Err(CliError::Unexpected(format!(
            "debug tool `{DEBUG_TOOL_NAME}` was not called on the first step; no tool call was pending"
        )));
    };

    let actual_tool_name = pending.name().as_str();
    if actual_tool_name != DEBUG_TOOL_NAME {
        writer.flush().await.map_err(stdout_error)?;
        return Err(CliError::Unexpected(format!(
            "debug tool `{DEBUG_TOOL_NAME}` was not called on the first step; pending tool was `{actual_tool_name}`"
        )));
    }

    write_runtime_events(
        runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .map_err(unexpected)?,
        &mut writer,
    )
    .await?;

    let input = StepInput::user_text(DEBUG_TOOL_CONTINUATION_INPUT).map_err(unexpected)?;
    write_runtime_step_events_to(runtime, input, context, &mut writer).await?;

    writer.flush().await.map_err(stdout_error)
}

async fn write_runtime_step_events_to<W>(
    runtime: &Runtime,
    input: StepInput,
    context: StepContext,
    writer: &mut W,
) -> Result<Vec<RuntimeEvent>, CliError>
where
    W: AsyncWrite + Unpin,
{
    let mut events = runtime.step(input, context).map_err(unexpected)?;
    let mut written = Vec::new();
    while let Some(event) = events.next().await {
        write_runtime_event(&event, writer).await?;
        written.push(event);
    }

    Ok(written)
}

async fn write_runtime_events<W>(events: Vec<RuntimeEvent>, writer: &mut W) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    for event in events {
        write_runtime_event(&event, writer).await?;
    }
    Ok(())
}

async fn write_runtime_event<W>(event: &RuntimeEvent, writer: &mut W) -> Result<(), CliError>
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

fn first_pending_tool_call(events: &[RuntimeEvent]) -> Option<PendingToolCall> {
    events.iter().find_map(|event| match &event.kind {
        RuntimeEventKind::ToolCallPending { call } => Some(call.clone()),
        _ => None,
    })
}

fn debug_echo_tool(result: &str) -> Result<RegisteredTool, CliError> {
    if result.trim().is_empty() {
        return Err(debug_openai_usage_error(
            "--debug-tool-result must not be blank",
        ));
    }

    let schema = serde_json::from_value::<ToolInputSchema>(serde_json::json!({
        "type": "object",
        "additionalProperties": true
    }))
    .map_err(debug_openai_usage_error)?;
    let spec = ToolSpec::new(
        ToolName::new(DEBUG_TOOL_NAME).map_err(debug_openai_usage_error)?,
        "Return the fixed debug text provided by the CLI.",
        schema,
    )
    .map_err(debug_openai_usage_error)?;

    Ok(RegisteredTool::new(
        spec,
        Arc::new(DebugEchoExecutor {
            result: result.to_owned(),
        }),
    ))
}

struct DebugEchoExecutor {
    result: String,
}

impl ToolExecutor for DebugEchoExecutor {
    fn execute<'a>(
        &'a self,
        _call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move { Ok(ToolExecutionOutcome::succeeded_text(self.result.clone())) })
    }
}

fn debug_openai_config(model_flag: Option<&str>) -> Result<DebugOpenAiConfig, CliError> {
    if env::var("MERRY_OPENAI_DEBUG").as_deref() != Ok("1") {
        return Err(debug_openai_usage_error(
            "set MERRY_OPENAI_DEBUG=1 to enable live OpenAI-compatible debugging",
        ));
    }

    let api_key = required_env("OPENAI_API_KEY")?;
    let model = match model_flag {
        Some(model) => model.to_owned(),
        None => required_env("MERRY_OPENAI_MODEL")?,
    };

    let mut provider = OpenAiProviderConfig::new(&api_key).map_err(debug_openai_usage_error)?;

    if let Some(base_url) = optional_env("MERRY_OPENAI_BASE_URL")? {
        provider = provider
            .with_base_url(&base_url)
            .map_err(debug_openai_usage_error)?;
    }

    if let Some(organization) = optional_env("OPENAI_ORG_ID")? {
        provider = provider
            .with_organization(&organization)
            .map_err(debug_openai_usage_error)?;
    }

    if let Some(project) = optional_env("OPENAI_PROJECT_ID")? {
        provider = provider
            .with_project(&project)
            .map_err(debug_openai_usage_error)?;
    }

    Ok(DebugOpenAiConfig { provider, model })
}

fn required_env(name: &'static str) -> Result<String, CliError> {
    match optional_env(name)? {
        Some(value) => Ok(value),
        None => Err(debug_openai_usage_error(format!("{name} must be set"))),
    }
}

fn optional_env(name: &'static str) -> Result<Option<String>, CliError> {
    match env::var(name) {
        Ok(value) if value.trim().is_empty() => Err(debug_openai_usage_error(format!(
            "{name} must not be blank"
        ))),
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(debug_openai_usage_error(format!(
            "{name} must be valid UTF-8"
        ))),
    }
}

struct DebugOpenAiConfig {
    provider: OpenAiProviderConfig,
    model: String,
}

fn root_usage() -> &'static str {
    "Usage: merry <COMMAND>\n\nCommands:\n  debug    Print deterministic runtime events or run opt-in provider debugging\n\nOptions:\n  --help   Print help\n"
}

fn debug_usage() -> &'static str {
    "Usage: merry debug [--session-id <SESSION_ID>] [--input <TEXT>]\n       merry debug openai --input <TEXT> [--model <MODEL>] [--max-output-tokens <N>] [--debug-tool-result <TEXT>]\n\nCommands:\n  openai                     Run opt-in OpenAI-compatible model debugging\n\nOptions:\n  --session-id <SESSION_ID>  Session id to use [default: debug-session]\n  --input <TEXT>             User text input [default: debug step]\n  --help                     Print help\n"
}

fn debug_openai_usage() -> &'static str {
    "Usage: merry debug openai --input <TEXT> [--model <MODEL>] [--max-output-tokens <N>] [--debug-tool-result <TEXT>]\n\nOptions:\n  --input <TEXT>             User text input to send through Runtime::step\n  --model <MODEL>            Model name; falls back to MERRY_OPENAI_MODEL\n  --max-output-tokens <N>    Optional maximum output tokens for this step\n  --debug-tool-result <TEXT> Require first step to call debug_echo; return this text\n  --help                     Print help\n\nEnvironment:\n  MERRY_OPENAI_DEBUG=1       Required opt-in before any network attempt\n  OPENAI_API_KEY             Required after opt-in\n  MERRY_OPENAI_MODEL         Required when --model is omitted\n  MERRY_OPENAI_BASE_URL      Optional OpenAI-compatible base URL\n  OPENAI_ORG_ID              Optional organization header\n  OPENAI_PROJECT_ID          Optional project header\n"
}

fn unexpected(err: impl fmt::Display) -> CliError {
    CliError::Unexpected(err.to_string())
}

fn usage_error(err: impl fmt::Display) -> CliError {
    CliError::DebugUsage(err.to_string())
}

fn debug_openai_usage_error(err: impl fmt::Display) -> CliError {
    CliError::DebugOpenAiUsage(err.to_string())
}

fn stdout_error(err: io::Error) -> CliError {
    if err.kind() == io::ErrorKind::BrokenPipe {
        CliError::BrokenPipe
    } else {
        CliError::Unexpected(err.to_string())
    }
}

enum CliError {
    BrokenPipe,
    DebugUsage(String),
    DebugOpenAiUsage(String),
    Unexpected(String),
}

enum ParseError {
    Root(String),
    Debug(String),
    DebugOpenAi(String),
}

enum CliExit {
    Success,
    Usage {
        message: String,
        usage: &'static str,
    },
    Unexpected(String),
}

impl Termination for CliExit {
    fn report(self) -> ExitCode {
        match self {
            Self::Success => ExitCode::SUCCESS,
            Self::Usage { message, usage } => {
                eprintln!("{message}\n\n{usage}");
                ExitCode::from(2)
            }
            Self::Unexpected(message) => {
                eprintln!("{message}");
                ExitCode::FAILURE
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CliError, DEBUG_TOOL_CONTINUATION_INPUT, debug_echo_tool, write_debug_openai_tool_events,
    };
    use super::{DEBUG_TOOL_NAME, write_runtime_step_events};
    use futures_util::stream;
    use merry_core::{ProviderName, RuntimeEvent, ToolCallResultStatus, ToolName};
    use merry_llm::{
        FinishReason, GenerationConfig, ModelCapabilities, ModelError, ModelEvent,
        ModelEventStream, ModelName, ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest,
        ModelResponse, ModelStreamContext, ModelToolCall, ModelToolCallId, ToolArguments,
    };
    use merry_runtime::{Runtime, StepContext, StepInput};
    use serde_json::{Map, Value};
    use std::sync::{Arc, Mutex};

    struct CompletingProvider {
        name: ProviderName,
        capabilities: ModelCapabilities,
    }

    impl CompletingProvider {
        fn new() -> Self {
            Self {
                name: ProviderName::new("debug-test-provider").expect("valid provider name"),
                capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                    .expect("valid capabilities"),
            }
        }
    }

    impl ModelProvider for CompletingProvider {
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
                let response = ModelResponse::new(
                    vec![ModelOutput::text("hidden from runtime events")],
                    FinishReason::Stop,
                    None,
                );
                let events = vec![
                    Ok(ModelEvent::Started),
                    Ok(ModelEvent::OutputTextDelta {
                        delta: "hidden".to_owned(),
                    }),
                    Ok(ModelEvent::Completed { response }),
                ];
                Ok(Box::pin(stream::iter(events)) as ModelEventStream)
            })
        }
    }

    struct RecordingProvider {
        inner: CompletingProvider,
        requests: Arc<Mutex<Vec<ModelRequest>>>,
    }

    impl RecordingProvider {
        fn new() -> Self {
            Self {
                inner: CompletingProvider::new(),
                requests: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl ModelProvider for RecordingProvider {
        fn name(&self) -> &ProviderName {
            self.inner.name()
        }

        fn capabilities(&self) -> &ModelCapabilities {
            self.inner.capabilities()
        }

        fn stream_model<'a>(
            &'a self,
            request: ModelRequest,
            context: ModelStreamContext,
        ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
            self.requests
                .lock()
                .expect("request mutex should not be poisoned")
                .push(request.clone());
            self.inner.stream_model(request, context)
        }
    }

    type ScriptedStep = Vec<Result<ModelEvent, ModelError>>;

    #[derive(Debug, Clone)]
    struct ScriptedProvider {
        name: ProviderName,
        capabilities: ModelCapabilities,
        steps: Arc<Mutex<Vec<ScriptedStep>>>,
        requests: Arc<Mutex<Vec<ModelRequest>>>,
    }

    impl ScriptedProvider {
        fn new(scripts: Vec<ScriptedStep>) -> Self {
            Self {
                name: ProviderName::new("debug-scripted-provider").expect("valid provider name"),
                capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                    .expect("valid capabilities"),
                steps: Arc::new(Mutex::new(scripts.into_iter().rev().collect())),
                requests: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn recorded_requests(&self) -> Vec<ModelRequest> {
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

    #[tokio::test]
    async fn debug_openai_runtime_helper_writes_runtime_lifecycle_jsonl_without_model_events() {
        let runtime = Runtime::builder(merry_core::SessionId::new("debug-openai-test").unwrap())
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
    async fn debug_openai_runtime_helper_passes_generation_config_to_runtime_step() {
        let provider = RecordingProvider::new();
        let requests = Arc::clone(&provider.requests);
        let runtime = Runtime::builder(merry_core::SessionId::new("debug-openai-config").unwrap())
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

    #[tokio::test]
    async fn debug_openai_tool_helper_executes_one_pending_call_and_continues() {
        let call = ModelToolCall::new(
            ModelToolCallId::new("call-debug").expect("valid tool call id"),
            ToolName::new(DEBUG_TOOL_NAME).expect("valid tool name"),
            ToolArguments::new(Map::new()),
        );
        let provider = ScriptedProvider::new(vec![
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::tool_call(call)],
                    FinishReason::ToolCalls,
                    None,
                ),
            })],
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("continued after tool")],
                    FinishReason::Stop,
                    None,
                ),
            })],
        ]);
        let runtime = Runtime::builder(merry_core::SessionId::new("debug-openai-tool").unwrap())
            .register_tool(
                debug_echo_tool("debug result").unwrap_or_else(|_| panic!("valid debug tool")),
            )
            .model_provider(
                Arc::new(provider.clone()),
                ModelName::new("debug-model").unwrap(),
            )
            .build()
            .expect("runtime should build");
        let input = StepInput::user_text("please call the tool").expect("valid input");
        let context = StepContext::default().with_generation_config(
            GenerationConfig::new(Some(16), false).expect("valid generation config"),
        );
        let mut output = Vec::new();

        write_debug_openai_tool_events(&runtime, input, context, &mut output)
            .await
            .unwrap_or_else(|_| panic!("tool events should write"));

        let text = String::from_utf8(output).expect("output should be utf-8");
        let events = text
            .lines()
            .map(|line| serde_json::from_str::<RuntimeEvent>(line).expect("line should be JSON"))
            .collect::<Vec<_>>();
        let event_types = events
            .iter()
            .map(|event| {
                let value = serde_json::to_value(event).expect("event should serialize");
                value["kind"]["type"].as_str().unwrap().to_owned()
            })
            .collect::<Vec<_>>();

        assert_eq!(
            event_types,
            [
                "session_started",
                "step_started",
                "tool_call_pending",
                "artifact_recorded",
                "tool_call_resolved",
                "step_started",
                "artifact_recorded",
                "step_completed",
            ]
        );

        let resolved = events
            .iter()
            .find_map(|event| match &event.kind {
                merry_core::RuntimeEventKind::ToolCallResolved { result } => Some(result),
                _ => None,
            })
            .expect("tool should be resolved");
        assert_eq!(resolved.status(), ToolCallResultStatus::Succeeded);

        let requests = provider.recorded_requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].tools().len(), 1);
        assert_eq!(requests[0].tools()[0].name().as_str(), DEBUG_TOOL_NAME);
        assert!(requests[0].continuations().is_empty());
        assert_eq!(requests[0].generation().max_output_tokens(), Some(16));
        assert!(!requests[0].generation().allow_parallel_tool_calls());

        assert_eq!(requests[1].tools().len(), 1);
        assert_eq!(requests[1].tools()[0].name().as_str(), DEBUG_TOOL_NAME);
        assert_eq!(requests[1].continuations().len(), 1);
        let continuation = &requests[1].continuations()[0];
        assert_eq!(continuation.call().id().as_str(), "call-debug");
        assert_eq!(
            continuation.result().status(),
            ToolCallResultStatus::Succeeded
        );
        assert_eq!(
            continuation.result().content().as_text(),
            Some("debug result")
        );
        assert!(
            requests[1]
                .messages()
                .iter()
                .any(|message| message.content().as_text() == DEBUG_TOOL_CONTINUATION_INPUT)
        );
        assert_eq!(requests[1].generation().max_output_tokens(), Some(16));
        assert!(!requests[1].generation().allow_parallel_tool_calls());
    }

    #[tokio::test]
    async fn debug_openai_tool_helper_errors_when_first_step_calls_wrong_tool() {
        let wrong_tool_name = "wrong_tool";
        let call = ModelToolCall::new(
            ModelToolCallId::new("call-wrong").expect("valid tool call id"),
            ToolName::new(wrong_tool_name).expect("valid tool name"),
            ToolArguments::new(Map::new()),
        );
        let provider = ScriptedProvider::new(vec![
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::tool_call(call)],
                    FinishReason::ToolCalls,
                    None,
                ),
            })],
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("should not continue")],
                    FinishReason::Stop,
                    None,
                ),
            })],
        ]);
        let runtime =
            Runtime::builder(merry_core::SessionId::new("debug-openai-wrong-tool").unwrap())
                .register_tool(
                    debug_echo_tool("debug result").unwrap_or_else(|_| panic!("valid debug tool")),
                )
                .model_provider(
                    Arc::new(provider.clone()),
                    ModelName::new("debug-model").unwrap(),
                )
                .build()
                .expect("runtime should build");
        let input = StepInput::user_text("please call the tool").expect("valid input");
        let mut output = Vec::new();

        let error =
            write_debug_openai_tool_events(&runtime, input, StepContext::default(), &mut output)
                .await
                .expect_err("wrong first-step tool call should fail");

        match error {
            CliError::Unexpected(message) => {
                assert!(message.contains(DEBUG_TOOL_NAME));
                assert!(message.contains(wrong_tool_name));
            }
            _ => panic!("expected unexpected error for wrong tool call"),
        }

        let text = String::from_utf8(output).expect("output should be utf-8");
        let events = text
            .lines()
            .map(|line| serde_json::from_str::<RuntimeEvent>(line).expect("line should be JSON"))
            .collect::<Vec<_>>();
        assert!(
            !events.is_empty(),
            "first-step runtime events should be preserved"
        );
        let event_types = events
            .iter()
            .map(|event| {
                let value = serde_json::to_value(event).expect("event should serialize");
                value["kind"]["type"].as_str().unwrap().to_owned()
            })
            .collect::<Vec<_>>();

        assert!(event_types.iter().any(|kind| kind == "tool_call_pending"));
        assert!(!event_types.iter().any(|kind| kind == "tool_call_resolved"));
        let pending = events
            .iter()
            .find_map(|event| match &event.kind {
                merry_core::RuntimeEventKind::ToolCallPending { call } => Some(call),
                _ => None,
            })
            .expect("wrong tool call should remain pending in first-step events");
        assert_eq!(pending.name().as_str(), wrong_tool_name);

        let requests = provider.recorded_requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].continuations().is_empty());
    }

    #[tokio::test]
    async fn debug_openai_tool_helper_errors_when_first_step_does_not_call_debug_echo() {
        let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("completed without tool")],
                FinishReason::Stop,
                None,
            ),
        })]]);
        let runtime = Runtime::builder(merry_core::SessionId::new("debug-openai-no-tool").unwrap())
            .register_tool(
                debug_echo_tool("debug result").unwrap_or_else(|_| panic!("valid debug tool")),
            )
            .model_provider(
                Arc::new(provider.clone()),
                ModelName::new("debug-model").unwrap(),
            )
            .build()
            .expect("runtime should build");
        let input = StepInput::user_text("do not call the tool").expect("valid input");
        let mut output = Vec::new();

        let error =
            write_debug_openai_tool_events(&runtime, input, StepContext::default(), &mut output)
                .await
                .expect_err("missing first-step tool call should fail");

        match error {
            CliError::Unexpected(message) => {
                assert!(message.contains(DEBUG_TOOL_NAME));
                assert!(message.contains("no tool call was pending"));
            }
            _ => panic!("expected unexpected error for missing tool call"),
        }

        let text = String::from_utf8(output).expect("output should be utf-8");
        let events = text
            .lines()
            .map(|line| serde_json::from_str::<RuntimeEvent>(line).expect("line should be JSON"))
            .collect::<Vec<_>>();
        assert!(
            !events.is_empty(),
            "first-step runtime events should be preserved"
        );
        let event_types = events
            .iter()
            .map(|event| {
                let value = serde_json::to_value(event).expect("event should serialize");
                value["kind"]["type"].as_str().unwrap().to_owned()
            })
            .collect::<Vec<_>>();

        assert_eq!(
            event_types,
            [
                "session_started",
                "step_started",
                "artifact_recorded",
                "step_completed",
            ]
        );
        assert!(!event_types.iter().any(|kind| kind == "tool_call_pending"));
        assert!(!event_types.iter().any(|kind| kind == "tool_call_resolved"));

        let requests = provider.recorded_requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].continuations().is_empty());
    }
}
