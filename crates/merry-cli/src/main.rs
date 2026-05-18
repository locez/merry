//! Debug and demonstration CLI for Merry.

use futures_util::StreamExt;
use merry_core::SessionId;
use merry_llm::ModelName;
use merry_provider_openai::{OpenAiProvider, OpenAiProviderConfig};
use merry_runtime::{Runtime, StepContext, StepInput};
use std::{
    env, fmt, io,
    process::{ExitCode, Termination},
    sync::Arc,
};
use tokio::io::{AsyncWrite, AsyncWriteExt, BufWriter};

const DEFAULT_SESSION_ID: &str = "debug-session";
const DEFAULT_INPUT: &str = "debug step";

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
        }) => match run_debug_openai(&input, model.as_deref(), max_output_tokens).await {
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
) -> Result<(), CliError> {
    let config = debug_openai_config(model)?;

    if max_output_tokens.is_some() {
        return Err(debug_openai_usage_error(
            "--max-output-tokens is not supported until Runtime::step generation config exists",
        ));
    }

    let session_id = SessionId::new(DEFAULT_SESSION_ID).map_err(debug_openai_usage_error)?;
    let model = ModelName::new(&config.model).map_err(debug_openai_usage_error)?;
    let provider = OpenAiProvider::new(config.provider);
    let runtime = Runtime::builder(session_id)
        .model_provider(Arc::new(provider), model)
        .build()
        .map_err(unexpected)?;
    let input = StepInput::user_text(input).map_err(debug_openai_usage_error)?;

    write_runtime_step_events(
        &runtime,
        input,
        StepContext::new(Default::default()),
        tokio::io::stdout(),
    )
    .await
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
    let mut events = runtime.step(input, context).map_err(unexpected)?;

    let mut writer = BufWriter::new(writer);

    while let Some(event) = events.next().await {
        let line = serde_json::to_string(&event).map_err(unexpected)?;
        writer
            .write_all(line.as_bytes())
            .await
            .map_err(stdout_error)?;
        writer.write_all(b"\n").await.map_err(stdout_error)?;
    }

    writer.flush().await.map_err(stdout_error)
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
    "Usage: merry debug [--session-id <SESSION_ID>] [--input <TEXT>]\n       merry debug openai --input <TEXT> [--model <MODEL>] [--max-output-tokens <N>]\n\nCommands:\n  openai                     Run opt-in OpenAI-compatible model debugging\n\nOptions:\n  --session-id <SESSION_ID>  Session id to use [default: debug-session]\n  --input <TEXT>             User text input [default: debug step]\n  --help                     Print help\n"
}

fn debug_openai_usage() -> &'static str {
    "Usage: merry debug openai --input <TEXT> [--model <MODEL>] [--max-output-tokens <N>]\n\nOptions:\n  --input <TEXT>             User text input to send through Runtime::step\n  --model <MODEL>            Model name; falls back to MERRY_OPENAI_MODEL\n  --max-output-tokens <N>    Rejected until Runtime::step generation config exists\n  --help                     Print help\n\nEnvironment:\n  MERRY_OPENAI_DEBUG=1       Required opt-in before any network attempt\n  OPENAI_API_KEY             Required after opt-in\n  MERRY_OPENAI_MODEL         Required when --model is omitted\n  MERRY_OPENAI_BASE_URL      Optional OpenAI-compatible base URL\n  OPENAI_ORG_ID              Optional organization header\n  OPENAI_PROJECT_ID          Optional project header\n"
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
    use super::write_runtime_step_events;
    use futures_util::stream;
    use merry_core::ProviderName;
    use merry_llm::{
        FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelName,
        ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse,
        ModelStreamContext,
    };
    use merry_runtime::{Runtime, StepContext, StepInput};
    use serde_json::Value;
    use std::sync::Arc;

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
}
